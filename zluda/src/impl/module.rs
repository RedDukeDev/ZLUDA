use super::driver;
use crate::r#impl::function;
use crate::r#impl::{driver::GlobalState, function::Function};
use cuda_types::{cuda::*, dark_api::FatbinFileHeader};
use dark_api::FunctionArgInfo;
use hip_runtime_sys::*;
use rustc_hash::FxHashMap;
use std::collections::hash_map;
use std::sync::{Mutex, OnceLock};
use std::{
    borrow::Cow,
    ffi::{CStr, CString},
    fs, mem,
    ops::ControlFlow,
};
use zluda_common::{CodeLibraryRef, CodeModuleRef, ZludaObject};

// Functions containing unrecognized PTX directives are dropped silently: the
// module loads fine, but cuModuleGetFunction then cannot find them. With
// ZLUDA_DEBUG_COMPILE set we report what was dropped and why - without it,
// working on the PTX frontend means guessing.
pub(crate) fn debug_compile() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ZLUDA_DEBUG_COMPILE").is_some())
}

pub(crate) struct Module {
    pub(crate) base: hipModule_t,
    pub(crate) sm_version: u32,
    pub(crate) bit32: Option<Metadata32Bit>,
    mutable: Mutex<ModuleMutable>,
}

struct ModuleMutable {
    functions: FxHashMap<CString, Box<Function>>,
}

impl ModuleMutable {
    pub(crate) fn get_function(
        &mut self,
        module: hipModule_t,
        sm_version: u32,
        name: CString,
        explicit_args_size_align: Option<Vec<FunctionArgInfo>>,
    ) -> Result<&Function, CUerror> {
        Ok(match self.functions.entry(name) {
            hash_map::Entry::Occupied(entry) => &*entry.into_mut(),
            hash_map::Entry::Vacant(entry) => {
                let mut func_handle = unsafe { std::mem::zeroed() };
                unsafe { hipModuleGetFunction(&mut func_handle, module, entry.key().as_ptr()) }?;
                let func = Box::new(Function {
                    base: func_handle,
                    sm_version,
                    explicit_args_size_align,
                    name: entry.key().to_string_lossy().into_owned(),
                });
                &*entry.insert(func)
            }
        })
    }
}

impl ZludaObject for Module {
    const COOKIE: usize = 0xe9138bd040487d4a;

    type Error = CUerror;
    type CudaHandle = CUmodule;

    fn drop_checked(&mut self) -> CUresult {
        unsafe { hipModuleUnload(self.base) }?;
        Ok(())
    }
}

pub(crate) struct Metadata32Bit {
    pub globals: Vec<Global32Bit>,
    pub explicit_args_size_align: FxHashMap<String, Vec<FunctionArgInfo>>,
}

impl Metadata32Bit {
    fn new(meta: &kernel_metadata::ModuleMetadata32Bit) -> Self {
        let globals = meta
            .globals
            .iter()
            .map(|g| Global32Bit {
                name: CString::new(&*g.name).unwrap(),
                initializer: g.initializer.to_vec(),
                align: g.align,
            })
            .collect();
        let explicit_args_size_align = meta
            .explicit_args_size_align
            .iter()
            .map(|(key, value)| {
                let key = key.to_string();
                let value = value
                    .iter()
                    .map(|(size, align)| FunctionArgInfo {
                        size: *size,
                        align: *align,
                    })
                    .collect();
                (key, value)
            })
            .collect();
        Self {
            globals,
            explicit_args_size_align,
        }
    }

    fn from_archived(archived: &kernel_metadata::ArchivedModuleMetadata32Bit) -> Self {
        let globals = archived
            .globals
            .iter()
            .map(|g| Global32Bit {
                name: CString::new(g.name.as_str()).unwrap(),
                initializer: g.initializer.to_vec(),
                align: g.align.to_native(),
            })
            .collect();
        let explicit_args_size_align = archived
            .explicit_args_size_align
            .iter()
            .map(|kv| {
                (
                    kv.0.to_string(),
                    kv.1.iter()
                        .map(|x| FunctionArgInfo {
                            size: x.0.to_native(),
                            align: x.1.to_native(),
                        })
                        .collect(),
                )
            })
            .collect();
        Self {
            globals,
            explicit_args_size_align,
        }
    }
}

pub(crate) struct Global32Bit {
    pub name: CString,
    pub initializer: Vec<u8>,
    pub align: u32,
}

impl Module {
    pub(crate) fn new(base: hipModule_t, sm_version: u32, bit32: Option<Metadata32Bit>) -> Self {
        Self {
            base,
            sm_version,
            mutable: Mutex::new(ModuleMutable {
                functions: FxHashMap::default(),
            }),
            bit32,
        }
    }

    pub(crate) fn get_function<'a>(&'a self, name: &CStr) -> Result<&'static Function, CUerror> {
        let mut mutable = self.mutable.lock().map_err(|_| CUerror::UNKNOWN)?;
        let explicit_args = self.bit32.as_ref().and_then(|meta| {
            meta.explicit_args_size_align
                .get(name.to_str().ok()?)
                .map(Vec::clone)
        });
        mutable
            .get_function(self.base, self.sm_version, name.to_owned(), explicit_args)
            .map(|f| unsafe { (f as *const Function).as_ref().unwrap() })
    }
}

fn get_best_ptx_and_compile(
    global_state: &GlobalState,
    image: CodeLibraryRef<'_>,
) -> Result<(hipModule_t, u32, Option<Metadata32Bit>), CUerror> {
    let mut ptx_modules = Vec::new();
    unsafe {
        CodeLibraryRef::iterate_modules(image, |_, module| match module {
            Ok(CodeModuleRef::Text(ptx)) => {
                ptx_modules.push(Cow::Borrowed(ptx));
            }
            Ok(CodeModuleRef::File(file)) => {
                if file.header.kind != FatbinFileHeader::HEADER_KIND_PTX {
                    return;
                }
                if let Ok(text) = file.get_or_decompress_content(true) {
                    if let Some(text) = cow_bytes_to_str(text) {
                        ptx_modules.push(text);
                    }
                }
            }
            _ => {}
        })
    };
    let maybe_module = ptx_modules
        .iter()
        .rev() // TODO: actually sort by SM
        .try_fold(
            None,
            |acc: Option<(&Cow<'_, str>, ptx_parser::Module<'_>)>, src| {
                if debug_compile() {
                    if let Err(errors) = ptx_parser::parse_module_checked(src) {
                        for e in &errors {
                            eprintln!("[zluda] unrecognized PTX: {}", e);
                        }
                    }
                }
                let maybe_ast = if cfg!(debug_assertions) {
                    ptx_parser::parse_module_checked(src)
                } else {
                    Ok(ptx_parser::parse_module_unchecked(src))
                };
                match maybe_ast {
                    Err(_) => ControlFlow::Continue(acc),
                    Ok(ast) => {
                        if ast.invalid_directives == 0 {
                            return ControlFlow::Break((src, ast));
                        } else {
                            ControlFlow::Continue(Some(match acc {
                                Some(best_known) => {
                                    if ast.invalid_directives < best_known.1.invalid_directives {
                                        (src, ast)
                                    } else {
                                        best_known
                                    }
                                }
                                None => (src, ast),
                            }))
                        }
                    }
                }
            },
        );
    let (text, module) = match maybe_module {
        ControlFlow::Break((ast, module)) | ControlFlow::Continue(Some((ast, module))) => {
            (Some(ast), module)
        }
        ControlFlow::Continue(None) => {
            if cfg!(debug_assertions) {
                return Err(CUerror::NO_BINARY_FOR_GPU);
            }
            (
                None,
                ptx_parser::Module {
                    ptx_version: (1, 0),
                    sm_version: 0,
                    directives: Vec::new(),
                    invalid_directives: usize::MAX,
                    address_size: 64,
                },
            )
        }
    };
    if debug_compile() {
        eprintln!(
            "[zluda] {} PTX module(s) in image, picked sm_{} with {} invalid directive(s)",
            ptx_modules.len(),
            module.sm_version,
            module.invalid_directives
        );
    }
    // TODO: get this information on initialization
    let hip_properties = get_hip_properties()?;
    let gcn_arch = get_gcn_arch(&hip_properties)?;
    let cumode = llvm_zluda::is_cumode(gcn_arch);
    // Read once here and carried in `attributes`, so the value that names the
    // cache entry is the same value that governs code generation below. Reading
    // the environment again inside the compiler would let the two disagree if it
    // changed in between.
    let codegen_parts = llvm_zluda::codegen_parts();
    let attributes = ExtraCacheAttributes {
        clock_rate: hip_properties.clockRate as u32,
        is_debug: cfg!(debug_assertions),
        cumode,
        codegen_parts,
        ignore_maxnreg: std::env::var("ZLUDA_IGNORE_MAXNREG")
            .map(|v| v == "1")
            .unwrap_or(false),
        num_vgpr_override: std::env::var("ZLUDA_NUM_VGPR")
            .ok()
            .and_then(|v| v.parse::<u32>().ok()),
    };
    let mut cache_with_key = match (text, global_state.cache_path.as_ref()) {
        (Some(text), Some(p)) => (|| {
            let cache = zluda_cache::ModuleCache::open(p)?;
            let key = get_cache_key(gcn_arch, &text, &attributes)?;
            Some((cache, key))
        })(),
        _ => None,
    };
    // How many kernels the PTX asks for. Both the cache and the translation are
    // held to it: an object with none of them is not an answer.
    let kernels_wanted = count_kernels_declared(&module);
    let cached_binary = load_cached_binary(&mut cache_with_key, kernels_wanted, cumode);
    let (elf_module, sm_version, zluda32) = cached_binary.ok_or(CUerror::UNKNOWN).or_else(|_| {
        compile_and_cache(
            gcn_arch,
            attributes,
            module,
            kernels_wanted,
            &mut cache_with_key,
        )
    })?;
    let mut hip_module = unsafe { mem::zeroed() };
    unsafe { hipModuleLoadData(&mut hip_module, elf_module.as_ptr().cast()) }?;
    Ok((hip_module, sm_version, zluda32))
}

fn cow_bytes_to_str<'a>(data: Cow<'a, [u8]>) -> Option<Cow<'a, str>> {
    match data {
        Cow::Borrowed(bytes) => std::str::from_utf8(bytes).map(Cow::Borrowed).ok(),
        Cow::Owned(bytes) => String::from_utf8(bytes).map(Cow::Owned).ok(),
    }
}

pub(crate) fn load_hip_module(
    library: CodeLibraryRef,
) -> Result<(hipModule_t, u32, Option<Metadata32Bit>), CUerror> {
    let global_state = driver::global_state()?;
    get_best_ptx_and_compile(global_state, library)
}

// The two launch-tuning fields below are skipped when unset, so a build
// without them serializes byte-for-byte like a build that predates them:
// adding the fields must not invalidate every existing cache entry.
#[derive(serde::Serialize)]
struct ExtraCacheAttributes {
    is_debug: bool,
    clock_rate: u32,
    cumode: bool,
    /// ZLUDA_IGNORE_MAXNREG: drop the PTX .maxnreg directive instead of
    /// enforcing it as amdgpu-num-vgpr. Belongs in the key because the
    /// compiled code differs.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    ignore_maxnreg: bool,
    /// ZLUDA_NUM_VGPR: force the per-function VGPR budget. Belongs in the key
    /// because the compiled code differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    num_vgpr_override: Option<u32>,
    /// How many parts the module is cut into before code generation.
    ///
    /// This belongs in the key because it changes the output: splitting narrows
    /// what the optimiser can see, so a module compiled as one object and the
    /// same module compiled as N are not the same code. The count depends on the
    /// host's CPU count, so without it a cache written on one machine answers for
    /// another that would have compiled something different.
    codegen_parts: u32,
}

fn get_hip_properties<'a>() -> Result<hipDeviceProp_tR0600, CUerror> {
    let hip_dev = super::context::get_current_device()?;
    let mut props = unsafe { mem::zeroed() };
    unsafe { hipGetDevicePropertiesR0600(&mut props, hip_dev) }?;
    Ok(props)
}

fn get_gcn_arch<'a>(props: &'a hipDeviceProp_tR0600) -> Result<&'a str, CUerror> {
    let gcn_arch = unsafe { CStr::from_ptr(props.gcnArchName.as_ptr()) };
    gcn_arch.to_str().map_err(|_| CUerror::UNKNOWN)
}

fn get_cache_key<'a, 'b>(
    isa: &'a str,
    text: &str,
    attributes: &impl serde::Serialize,
) -> Option<zluda_cache::ModuleKey<'a>> {
    // Serialization here is deterministic. When marking a type with
    // #[derive(serde::Serialize)] the derived implementation will just write
    // fields in the order of their declaration. It's not explictly guaranteed
    // by serde, but it is the only sensible thing to do, so I feel safe
    // to rely on it
    let serialized_attributes = serde_json::to_string(attributes).ok()?;
    Some(zluda_cache::ModuleKey {
        hash: blake3::hash(text.as_bytes()).to_hex(),
        compiler_version: "builtin",
        // What identifies the compiler that produced the entry.
        //
        // This has to move whenever the generated code can change, or a cache
        // written by one build is served to another and the difference is
        // invisible: the entry looks perfectly valid, it was stored under a
        // perfectly valid key, and every kernel in it is simply the older
        // translation. This was briefly a frozen literal, which meant that the
        // four commits that followed it -- hardware WMMA for RDNA 4, WGP mode
        // enforcement, and a recompiled ptx_impl -- all produced cache hits on
        // entries built before them, silently negating the work on any machine
        // whose cache was already warm.
        //
        // Three inputs, because the compiled output depends on all three and
        // they do not move together:
        //   - this repository's revision (VERGEN_GIT_SHA, from zluda/build.rs);
        //   - the embedded PTX implementation modules, which are build inputs
        //     committed to the tree and can be rebuilt without a code change;
        //   - codegen_parts in `attributes`, added above, which is host
        //     dependent.
        zluda_version: concat!(
            env!("VERGEN_GIT_SHA"),
            "/",
            env!("ZLUDA_PTX_IMPL_DIGEST"),
            // The two hashes above cannot see changes to this crate's own
            // translation passes: the git sha freezes before the commit lands
            // and the digest only covers the bitcode. Any change to emitted
            // code bumps this marker, or every user with a warm cache keeps
            // running the binary the old key compiled.
            "/fp8-inline-r1",
        ),
        device: isa,
        backend_key: serialized_attributes,
        last_access: zluda_cache::ModuleCache::time_now(),
    })
}

fn count_kernels_declared(module: &ptx_parser::Module) -> usize {
    module
        .directives
        .iter()
        .filter(|directive| match directive {
            ptx_parser::Directive::Method(_, function) => function.func_directive.name.is_kernel(),
            _ => false,
        })
        .count()
}

fn load_cached_binary(
    cache_with_key: &mut Option<(zluda_cache::ModuleCache, zluda_cache::ModuleKey)>,
    kernels_wanted: usize,
    cumode: bool,
) -> Option<(Vec<u8>, u32, Option<Metadata32Bit>)> {
    let mut binary = cache_with_key
        .as_mut()
        .and_then(|(c, key)| c.get_module_binary(key));
    // Which key the answer was actually stored under. An entry found through the
    // legacy key below has to be *removed* through that same key: the two differ
    // in `backend_key`, and `remove_module` matches on all five fields, so
    // deleting with the current key would match no row and leave the entry in
    // place to be found again on the next run.
    let mut matched_key = cache_with_key.as_ref().map(|(_, key)| key.clone());
    if binary.is_none() && cumode {
        if let Some((c, key)) = cache_with_key.as_mut() {
            let legacy_backend_key = key.backend_key.replace(",\"cumode\":true", "");
            if legacy_backend_key != key.backend_key {
                let mut legacy_key = key.clone();
                legacy_key.backend_key = legacy_backend_key;
                if let Some(found) = c.get_module_binary(&legacy_key) {
                    matched_key = Some(legacy_key);
                    binary = Some(found);
                }
            }
        }
    }
    let binary = binary?;
    // An entry with none of the kernels the PTX declares cannot be right, and
    // one such entry is in the everyday cache to this day: the same PTX that
    // gives a 14 MB object under one build gave a 2312-byte one under another,
    // with a .text of length zero and not a symbol in it. It was stored under a
    // perfectly good key, so every run afterwards was handed it back and every
    // kernel in that module was NOT_FOUND, with nothing to say why. Throwing it
    // out here turns that into one slow run instead of a cache that has to be
    // deleted by hand.
    //
    // Only a definite "no kernels" counts. `count_kernels` returns None when it
    // cannot parse the object at all, and that is not the same statement: it is
    // not evidence that the entry is bad. Treating the two alike would let a
    // parse limitation throw away a good translation *and* delete the entry
    // that produced it, so every run would re-translate and delete again -- the
    // very "translates all fifteen modules every time" symptom this guard was
    // written to remove. An unparseable object is trusted here and fails later
    // with whatever the loader has to say about it.
    if kernels_wanted > 0 && kernel_metadata::count_kernels(&binary) == Some(0) {
        eprintln!(
            "[zluda] the cached translation of this module has no kernels in it, though the PTX declares {}. Dropping it and translating again.",
            kernels_wanted
        );
        if let Some((cache, _)) = cache_with_key.as_mut() {
            if let Some(key) = matched_key.as_ref() {
                cache.remove_module(key);
            }
        }
        return None;
    }
    let sm_version = kernel_metadata::ModuleMetadataV1::read_object(&binary)?
        .sm_version
        .to_native();
    let zluda32 = kernel_metadata::ModuleMetadata32Bit::read_object(&binary)
        .map(Metadata32Bit::from_archived);
    Some((binary, sm_version, zluda32))
}

fn compile_and_cache(
    gcn_arch: &str,
    attributes: ExtraCacheAttributes,
    ast: ptx_parser::Module,
    kernels_wanted: usize,
    cache_with_key: &mut Option<(zluda_cache::ModuleCache, zluda_cache::ModuleKey)>,
) -> Result<(Vec<u8>, u32, Option<Metadata32Bit>), CUerror> {
    let llvm_module = ptx::to_llvm_module(
        ast,
        ptx::Attributes {
            clock_rate: attributes.clock_rate,
            cumode: llvm_zluda::is_cumode(gcn_arch),
            ignore_maxnreg: attributes.ignore_maxnreg,
            num_vgpr_override: attributes.num_vgpr_override,
        },
        |_| {},
    )
    .map_err(|e| {
        if debug_compile() {
            eprintln!("[zluda] ptx::to_llvm_module failed: {}", e);
        }
        CUerror::UNKNOWN
    })?;
    // With ZLUDA_DUMP_IR set to a directory, the LLVM IR of every module is
    // written there before the backend sees it.
    //
    // It answers questions the PTX cannot and the disassembly answers badly. An
    // uninitialised value is spelled undef here, where in machine code it is
    // indistinguishable from a register that happens to be read early; and how
    // far one PTX instruction expands is plain, which is where the time goes.
    if let Some(dir) = std::env::var_os("ZLUDA_DUMP_IR") {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let _ = std::fs::create_dir_all(&dir);
        let path = std::path::Path::new(&dir).join(format!("module_{:03}.ll", n));
        if let Ok(mut file) = std::fs::File::create(&path) {
            let _ = file.write_all(llvm_module.llvm_ir.print_module_to_string().to_str().as_bytes());
        }
    }

    let ptx_impl = llvm_module.linked_bitcode();
    let sm_version = llvm_module.metadata.sm_version;
    let metadata32 = llvm_module.metadata32.as_ref().map(Metadata32Bit::new);
    let elf_module = llvm_zluda::compile(
        &llvm_module.context,
        gcn_arch,
        llvm_module.llvm_ir,
        ptx_impl,
        llvm_module.attributes_ir,
        llvm_module.metadata,
        llvm_module.metadata32,
        None,
    )
    .map_err(|e| {
        if debug_compile() {
            eprintln!("[zluda] llvm_zluda::compile failed: {}", e);
        }
        CUerror::UNKNOWN
    })?;
    // A translation can come back successful and empty, and one that does must
    // not be written down.
    //
    // The everyday cache still holds an example: the same PTX that gives a
    // 14 MB object under one build gave a 2312-byte one under another, with a
    // .text of length zero and not one symbol in it. Nothing complained. It was
    // stored under a perfectly good key, so every run afterwards got it back
    // and every kernel in that module was NOT_FOUND -- with nothing to say why,
    // and no way out short of deleting the cache by hand.
    //
    // Failing here instead costs a translation that was worthless anyway, and
    // says what happened while there is still something to say it about.
    //
    // `Some(0)` only. `count_kernels` answers None when it cannot parse the
    // object, which is a statement about this program and not about the
    // translation: refusing to cache on that would turn a parse limitation into
    // a hard compile failure for a module that is perfectly good, where the
    // loader would have accepted it.
    if kernels_wanted > 0 && kernel_metadata::count_kernels(&elf_module) == Some(0) {
        eprintln!(
            "[zluda] this module translated to an object with no kernels in it: {} were declared in the PTX and none came out. Not caching it.",
            kernels_wanted
        );
        return Err(CUerror::UNKNOWN);
    }
    if let Some((cache, key)) = cache_with_key {
        key.last_access = zluda_cache::ModuleCache::time_now();
        // A translation that was not stored is not a failure -- this run has the
        // object it needs -- but it is the reason the *next* run will translate
        // the same module again, so it is reported. Silently discarding it is how
        // "the cache never persists" stays invisible.
        if let Err(error) = cache.insert_module(key, &elf_module) {
            eprintln!(
                "[zluda] the translation of this module could not be written to the cache \
                 ({}); it will be translated again next time",
                error
            );
        }
    }
    Ok((elf_module, sm_version, metadata32))
}

pub(crate) fn load(module: &mut CUmodule, fname: &CStr) -> CUresult {
    let mut image = fs::read(fname.to_str().map_err(|_| CUerror::INVALID_VALUE)?)
        .map_err(|_| CUerror::INVALID_VALUE)?;
    // Null-terminate the image, in case it's text
    image.push(0);
    let library = unsafe { CodeLibraryRef::try_load(image.as_ptr() as *const std::ffi::c_void) }
        .map_err(|_| CUerror::NO_BINARY_FOR_GPU)?;
    let (hip_module, sm_version, meta32) = load_hip_module(library)?;
    *module = Module::new(hip_module, sm_version, meta32).wrap();
    Ok(())
}

pub(crate) fn load_data(module: &mut CUmodule, image: &std::ffi::c_void) -> CUresult {
    let library =
        unsafe { CodeLibraryRef::try_load(image) }.map_err(|_| CUerror::NO_BINARY_FOR_GPU)?;
    let (hip_module, sm_version, meta32) = load_hip_module(library)?;
    *module = Module::new(hip_module, sm_version, meta32).wrap();
    Ok(())
}

pub(crate) fn load_data_ex(
    module: &mut CUmodule,
    image: &std::ffi::c_void,
    _num_options: ::std::os::raw::c_uint,
    _options: Option<&mut CUjit_option_enum>,
    _option_values: Option<&mut *mut ::core::ffi::c_void>,
) -> CUresult {
    load_data(module, image)
}

pub(crate) fn unload(hmod: CUmodule) -> CUresult {
    zluda_common::drop_checked::<Module>(hmod)
}

pub(crate) fn get_function(
    hfunc: &mut &function::Function,
    module: &Module,
    name: &CStr,
) -> CUresult {
    *hfunc = module.get_function(name)?;
    Ok(())
}

pub(crate) fn get_global_v2(
    dptr: *mut hipDeviceptr_t,
    bytes: *mut usize,
    hmod: &Module,
    name: *const ::core::ffi::c_char,
) -> hipError_t {
    unsafe { hipModuleGetGlobal(dptr, bytes, hmod.base, name) }
}

pub(crate) fn get_loading_mode(mode: &mut cuda_types::cuda::CUmoduleLoadingMode) -> CUresult {
    *mode = cuda_types::cuda::CUmoduleLoadingMode::CU_MODULE_LAZY_LOADING;
    Ok(())
}

pub(crate) fn load_fat_binary(module: &mut CUmodule, image: &std::ffi::c_void) -> CUresult {
    load_data(module, image)
}

pub(crate) unsafe fn get_tex_ref(
    texref: *mut *mut textureReference,
    hmod: &Module,
    name: *const ::core::ffi::c_char,
) -> hipError_t {
    hipModuleGetTexRef(texref, hmod.base, name)
}

pub(crate) unsafe fn get_surf_ref(
    surfref: *mut *mut textureReference,
    hmod: &Module,
    name: *const ::core::ffi::c_char,
) -> hipError_t {
    hipModuleGetTexRef(surfref, hmod.base, name)
}
