use crate::schema::modules;
use arrayvec::ArrayString;
use diesel::{connection::SimpleConnection, prelude::*};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::time::Duration;

pub(crate) mod models;
pub(crate) mod schema;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

#[derive(Clone)]
pub struct ModuleKey<'a> {
    pub hash: ArrayString<64>,
    pub compiler_version: &'static str,
    pub zluda_version: &'static str,
    pub device: &'a str,
    pub backend_key: String,
    pub last_access: i64,
}

pub struct ModuleCache(SqliteConnection);

impl ModuleCache {
    // ZLUDA_CACHE_DIR moves the cache somewhere else.
    //
    // Two reasons, both from working on the translation itself. Comparing two
    // builds means the older one's entries must not answer for the newer, and
    // the key does not tell them apart, so each needs a cache of its own.
    // And the file is held open for as long as a process using it lives: a run
    // that hangs in the driver leaves it locked, and then the cache cannot be
    // emptied at all, which is how an afternoon gets spent measuring stale
    // results.
    pub fn create_cache_dir_and_get_path() -> Option<String> {
        let mut cache_dir = match std::env::var_os("ZLUDA_CACHE_DIR") {
            Some(dir) => std::path::PathBuf::from(dir),
            None => {
                let mut dir = dirs::cache_dir()?;
                dir.extend(["zluda", "ComputeCache"]);
                dir
            }
        };
        // We ensure that the cache directory exists
        std::fs::create_dir_all(&cache_dir).ok()?;
        // No need to create the file, it will be created by SQLite on first access
        // zluda.db might be in use by older versions of ZLUDA
        cache_dir.push("zluda2.db");
        Some(cache_dir.to_string_lossy().into())
    }

    pub fn open(file_path: &str) -> Option<Self> {
        let mut conn = SqliteConnection::establish(file_path).ok()?;
        // busy_timeout defaults to 0: a second writer gets SQLITE_BUSY at once
        // rather than waiting for the first to finish. WAL still serialises
        // writers, it just does not make them queue for each other on its own,
        // and under that default a batch of parallel translations -- exactly what
        // precompiling a whole network at once produces, up to sixteen processes
        // finishing within moments of each other -- had most of its inserts
        // silently lost: the module was translated correctly and translating it
        // again was the only sign anything went wrong, because there was no
        // record it ever happened.
        //
        // Five seconds, not thirty. The timeout is *not* what makes writes
        // durable (see insert_module, which retries and reports), so its only
        // remaining job is to absorb a brief collision. A long one is not free:
        // get_module_binary is an UPDATE, so every cache *read* also takes the
        // write lock, and a reader that waits out a thirty-second timeout before
        // giving up turns a cheap miss into a thirty-second stall on a path that
        // runs once per module.
        conn.batch_execute(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;",
        )
        .ok()?;
        conn.run_pending_migrations(MIGRATIONS).ok()?;
        Some(Self(conn))
    }

    /// Looks an entry up, refreshing its `last_access`.
    ///
    /// Deliberately quieter about contention than `insert_module`: a miss here is
    /// cheap (it means "translate it"), so waiting a long time for the write lock
    /// only delays the work that has to happen anyway. A short timeout is set for
    /// this statement and restored afterwards, so one busy reader cannot pin the
    /// connection for the full timeout that writers are allowed.
    pub fn get_module_binary(&mut self, key: &ModuleKey) -> Option<Vec<u8>> {
        self.0
            .batch_execute("PRAGMA busy_timeout = 100;")
            .ok()?;
        let found = diesel::update(modules::dsl::modules)
            .set(modules::last_access.eq(key.last_access))
            .filter(modules::hash.eq(key.hash.as_str()))
            .filter(modules::compiler_version.eq(&key.compiler_version))
            .filter(modules::zluda_version.eq(key.zluda_version))
            .filter(modules::device.eq(key.device))
            .filter(modules::backend_key.eq(&key.backend_key))
            .returning(modules::binary)
            .get_result(&mut self.0)
            .ok();
        // Restore the write path's longer patience. A failure here only leaves the
        // shorter timeout in place for the next statement, which is recoverable.
        let _ = self.0.batch_execute("PRAGMA busy_timeout = 5000;");
        found
    }

    /// Writes an entry, retrying the two failure modes that are worth retrying.
    ///
    /// A busy timeout alone is not enough to make writes durable. SQLite returns
    /// `SQLITE_BUSY_SNAPSHOT` immediately -- without consulting the busy handler
    /// at all -- when a write is attempted on a connection whose cached WAL read
    /// snapshot has gone stale, which is the *common* case here because
    /// `get_module_binary` reads and writes on the same connection. The fix is
    /// not a longer wait but a fresh statement: a new statement takes a new
    /// snapshot. So a busy failure is retried a few times with a short backoff,
    /// and if it still fails the caller is told rather than left to discover it
    /// by noticing that the next run translated everything again.
    pub fn insert_module(
        &mut self,
        key: &ModuleKey,
        binary: &[u8],
    ) -> Result<(), diesel::result::Error> {
        const ATTEMPTS: u32 = 5;
        for attempt in 0..ATTEMPTS {
            let result = diesel::insert_into(modules::dsl::modules)
                .values(models::AddModule {
                    hash: key.hash.as_str(),
                    compiler_version: &key.compiler_version,
                    zluda_version: key.zluda_version,
                    device: key.device,
                    backend_key: &key.backend_key,
                    last_access: key.last_access,
                    binary,
                })
                .execute(&mut self.0);
            match result {
                Ok(_) => return Ok(()),
                Err(error) => {
                    // The insert already exists is not a failure: another process
                    // won the race and stored the same translation.
                    if is_unique_violation(&error) {
                        return Ok(());
                    }
                    if !is_busy(&error) || attempt + 1 == ATTEMPTS {
                        return Err(error);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1 << attempt));
                }
            }
        }
        unreachable!("the loop either returns or reports")
    }

    // Throw an entry away. Used when what came back cannot be right -- a
    // translated object with no kernels in it, say -- so that the next run
    // translates again instead of being handed the same broken answer for as
    // long as the cache lives.
    pub fn remove_module(&mut self, key: &ModuleKey) {
        diesel::delete(modules::dsl::modules)
            .filter(modules::hash.eq(key.hash.as_str()))
            .filter(modules::compiler_version.eq(&key.compiler_version))
            .filter(modules::zluda_version.eq(key.zluda_version))
            .filter(modules::device.eq(key.device))
            .filter(modules::backend_key.eq(&key.backend_key))
            .execute(&mut self.0)
            .ok();
    }

    pub fn time_now() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as i64
    }
}

/// Whether a failed insert failed because another writer holds the lock.
///
/// The message is inspected as well as the kind because the interesting case,
/// `SQLITE_BUSY_SNAPSHOT`, is not one of diesel's named kinds: it arrives as
/// `Unknown` carrying SQLite's own "database is locked" text. It is also the case
/// that matters most here, since it is returned without consulting the busy
/// handler at all.
fn is_busy(error: &diesel::result::Error) -> bool {
    match error {
        diesel::result::Error::DatabaseError(kind, info) => {
            matches!(
                kind,
                diesel::result::DatabaseErrorKind::SerializationFailure
            ) || {
                let message = info.message();
                message.contains("locked") || message.contains("busy")
            }
        }
        _ => false,
    }
}

/// Whether a failed insert failed because the row is already there. Two processes
/// translating the same module at once both try to store it, and the second one
/// losing that race has not gone wrong.
fn is_unique_violation(error: &diesel::result::Error) -> bool {
    match error {
        diesel::result::Error::DatabaseError(kind, info) => {
            matches!(
                kind,
                diesel::result::DatabaseErrorKind::UniqueViolation
            ) || info.message().contains("UNIQUE constraint failed")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        schema::{globals::dsl::*, modules::dsl::*},
        ModuleCache,
    };
    use arrayvec::ArrayString;
    use diesel::prelude::*;

    #[derive(Queryable, Selectable)]
    #[diesel(table_name = crate::schema::modules)]
    #[diesel(check_for_backend(diesel::sqlite::Sqlite))]
    pub struct Module {
        pub id: i64,
        pub hash: String,
        pub binary: Vec<u8>,
        pub last_access: i64,
    }

    #[derive(Queryable, Selectable)]
    #[diesel(table_name = crate::schema::globals)]
    #[diesel(check_for_backend(diesel::sqlite::Sqlite))]
    pub struct Global {
        pub key: String,
        pub value: i64,
    }

    #[test]
    fn empty_db_returns_no_module() {
        let mut db = ModuleCache::open(":memory:").unwrap();
        let module_binary = db.get_module_binary(&super::ModuleKey {
            hash: ArrayString::from("test_hash").unwrap(),
            compiler_version: "1.0.0",
            zluda_version: "1.0.0",
            device: "test_device",
            backend_key: "{}".to_string(),
            last_access: 123,
        });
        assert!(module_binary.is_none());
        let all_modules = modules.select(Module::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_modules.len(), 0);
        let all_globals: Vec<Global> = globals.select(Global::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_globals[0].key, "total_size");
        assert_eq!(all_globals[0].value, 0);
    }

    #[test]
    fn newly_inserted_module_increments_total_size() {
        let mut db = ModuleCache::open(":memory:").unwrap();
        db.insert_module(
            &super::ModuleKey {
                hash: ArrayString::from("test_hash1").unwrap(),
                compiler_version: "1.0.0",
                zluda_version: "1.0.0",
                device: "test_device",
                backend_key: "{}".to_string(),
                last_access: 123,
            },
            &[1, 2, 3, 4, 5],
        ).unwrap();
        db.insert_module(
            &super::ModuleKey {
                hash: ArrayString::from("test_hash2").unwrap(),
                compiler_version: "1.0.0",
                zluda_version: "1.0.0",
                device: "test_device",
                backend_key: "{}".to_string(),
                last_access: 124,
            },
            &[1, 2, 3],
        ).unwrap();
        let mut all_modules = modules.select(Module::as_select()).load(&mut db.0).unwrap();
        all_modules.sort_by_key(|m: &Module| m.id);
        assert_eq!(all_modules.len(), 2);
        assert_eq!(all_modules[0].hash, "test_hash1");
        assert_eq!(all_modules[0].last_access, 123);
        assert_eq!(all_modules[0].binary, &[1, 2, 3, 4, 5]);
        assert_eq!(all_modules[1].hash, "test_hash2");
        assert_eq!(all_modules[1].last_access, 124);
        assert_eq!(all_modules[1].binary, &[1, 2, 3]);
        let all_globals = globals.select(Global::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_globals[0].key, "total_size");
        assert_eq!(all_globals[0].value, 8);
    }

    #[test]
    fn get_bumps_last_access() {
        let mut db = ModuleCache::open(":memory:").unwrap();
        db.insert_module(
            &super::ModuleKey {
                hash: ArrayString::from("test_hash").unwrap(),
                compiler_version: "1.0.0",
                zluda_version: "1.0.0",
                device: "test_device",
                backend_key: "{}".to_string(),
                last_access: 123,
            },
            &[1, 2, 3, 4, 5],
        ).unwrap();
        let module_binary = db
            .get_module_binary(&super::ModuleKey {
                hash: ArrayString::from("test_hash").unwrap(),
                compiler_version: "1.0.0",
                zluda_version: "1.0.0",
                device: "test_device",
                backend_key: "{}".to_string(),
                last_access: 124,
            })
            .unwrap();
        let all_modules = modules.select(Module::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_modules.len(), 1);
        assert_eq!(all_modules[0].last_access, 124);
        assert_eq!(module_binary, &[1, 2, 3, 4, 5]);
        assert_eq!(all_modules[0].binary, &[1, 2, 3, 4, 5]);
        let all_globals = globals.select(Global::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_globals[0].key, "total_size");
        assert_eq!(all_globals[0].value, 5);
    }

    #[test]
    fn removed_module_is_gone_and_total_size_follows() {
        let mut db = ModuleCache::open(":memory:").unwrap();
        let entry = |access| super::ModuleKey {
            hash: ArrayString::from("test_hash").unwrap(),
            compiler_version: "1.0.0",
            zluda_version: "1.0.0",
            device: "test_device",
            backend_key: "{}".to_string(),
            last_access: access,
        };
        db.insert_module(&entry(123), &[1, 2, 3, 4, 5]).unwrap();
        db.remove_module(&entry(124));
        assert!(db.get_module_binary(&entry(125)).is_none());
        let all_modules = modules.select(Module::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_modules.len(), 0);
        // The delete trigger has to have run too, or the cache would think it
        // is holding bytes nobody can reach.
        let all_globals = globals.select(Global::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_globals[0].value, 0);
        // And the key is free again, so the next translation can take it.
        db.insert_module(&entry(126), &[9, 9]).unwrap();
        assert_eq!(db.get_module_binary(&entry(127)).unwrap(), &[9, 9]);
    }

    #[test]
    fn double_insert_does_not_override() {
        let mut db = ModuleCache::open(":memory:").unwrap();
        db.insert_module(
            &super::ModuleKey {
                hash: ArrayString::from("test_hash").unwrap(),
                compiler_version: "1.0.0",
                zluda_version: "1.0.0",
                device: "test_device",
                backend_key: "{}".to_string(),
                last_access: 123,
            },
            &[1, 2, 3, 4, 5],
        ).unwrap();
        db.insert_module(
            &super::ModuleKey {
                hash: ArrayString::from("test_hash").unwrap(),
                compiler_version: "1.0.0",
                zluda_version: "1.0.0",
                device: "test_device",
                backend_key: "{}".to_string(),
                last_access: 124,
            },
            &[5, 4, 3, 2, 1],
        ).unwrap();
        let all_modules = modules.select(Module::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_modules.len(), 1);
        assert_eq!(all_modules[0].last_access, 123);
        assert_eq!(all_modules[0].binary, &[1, 2, 3, 4, 5]);
        let all_globals = globals.select(Global::as_select()).load(&mut db.0).unwrap();
        assert_eq!(all_globals[0].key, "total_size");
        assert_eq!(all_globals[0].value, 5);
    }
}
