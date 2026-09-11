use vergen_gix::{Emitter, Gix};

fn main() {
    if cfg!(windows) {
        println!("cargo:rustc-link-arg=delayimp.lib");
        println!("cargo:rustc-link-arg=/DELAYLOAD:amdhip64_7.dll");
        let dll_path = "bin/nvcudart_hybrid64.dll";
        println!("cargo:rerun-if-changed={}", dll_path);
        check_lfs_file(dll_path);
    }
    let git = Gix::builder().sha(false).build();
    Emitter::default()
        .add_instructions(&git)
        .unwrap()
        .emit()
        .unwrap();

    emit_ptx_impl_digest();
}

/// Digest of the two embedded PTX implementation modules, for the module cache
/// key (`zluda/src/impl/module.rs`, `get_cache_key`).
///
/// The compiled output of a module depends on these byte for byte -- they are
/// linked into every translated module -- but they are committed build inputs,
/// so they can be rebuilt without any change to this repository's revision.
/// Without a digest of them in the key, a recompiled `zluda_ptx_impl.bc` is
/// served from the entries built before it, and the rebuild has no effect on any
/// machine whose cache is already warm.
///
/// FNV-1a rather than a cryptographic hash: this only has to change when the
/// bytes change, it must not pull a dependency into a build script, and it must
/// stay stable across toolchain upgrades (which `DefaultHasher` does not
/// promise). 68 KB per file is nothing at build time.
fn emit_ptx_impl_digest() {
    use std::fs::File;
    use std::io::Read;

    const FILES: [&str; 2] = [
        "../ptx/lib/zluda_ptx_impl.bc",
        "../ptx/lib/zluda_ptx_impl_constrained.bc",
    ];

    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    for path in FILES {
        println!("cargo:rerun-if-changed={}", path);
        let mut file = File::open(path)
            .unwrap_or_else(|e| panic!("Failed to open {}: {}", path, e));
        let mut buffer = [0u8; 8192];
        loop {
            let read = file
                .read(&mut buffer)
                .unwrap_or_else(|e| panic!("Failed to read {}: {}", path, e));
            if read == 0 {
                break;
            }
            for &byte in &buffer[..read] {
                digest ^= byte as u64;
                digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        // Separate the two files so that a permutation of their contents cannot
        // hash to the same value.
        digest ^= 0xff;
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }

    println!("cargo:rustc-env=ZLUDA_PTX_IMPL_DIGEST={:016x}", digest);
}

fn check_lfs_file(bc_path: &str) {
    use std::fs::File;
    use std::io::Read;

    let mut magic = [0u8; 2];
    File::open(bc_path)
        .unwrap_or_else(|e| panic!("Failed to open {}: {}", bc_path, e))
        .read_exact(&mut magic)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", bc_path, e));
    assert!(
        &magic == b"MZ",
        "{} is a git lfs stub and not the actual file. Run `git lfs pull` to fetch it",
        bc_path
    );
}
