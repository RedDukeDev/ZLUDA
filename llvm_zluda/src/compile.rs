use crate::utils::{Context, Message, PassBuilderOptions, TargetMachine};
use crate::LLVMZludaParseCommandLineOptions;
use crate::{ffi::LLVMZludaLinkWithLLD, utils::Module};
use llvm_sys::{
    core::*,
    target::{
        LLVMInitializeAMDGPUAsmPrinter, LLVMInitializeAMDGPUTarget, LLVMInitializeAMDGPUTargetInfo,
        LLVMInitializeAMDGPUTargetMC,
    },
    target_machine::{
        LLVMCodeGenFileType, LLVMCodeGenOptLevel, LLVMCodeModel, LLVMGetTargetFromTriple,
        LLVMRelocMode,
    },
    transforms::pass_builder::LLVMRunPasses,
};
use llvm_sys::{LLVMLinkage, LLVMVisibility};
use std::ffi::{CStr, CString};
use std::sync::OnceLock;
use std::{fs, ptr};
use tempfile::NamedTempFile;

const OCKL_MODULE: &[u8] = include_bytes!("device-libs/ockl.bc");

// https://llvm.org/docs/AMDGPUUsage.html#address-spaces
const CONSTANT_ADDRESS_SPACE: u32 = 4;

fn load_module(ctx: &Context, bc: &[u8], name: &std::ffi::CStr) -> Result<Module, String> {
    let module = Module::try_from_bitcode(ctx, bc, Some(name))
        .ok_or(("Failed to parse bitcode").to_string())?;
    module.verify()?;
    Ok(module)
}

// TODO: see if there's a way to reduce duplication with attributes.rs
fn add_constant(context: &Context, module: &Module, name: &CStr, attribute: u32) {
    let attribute_type = unsafe { LLVMInt32TypeInContext(context.get()) };
    let global = unsafe {
        LLVMAddGlobalInAddressSpace(
            module.get(),
            attribute_type,
            name.as_ptr(),
            CONSTANT_ADDRESS_SPACE,
        )
    };
    unsafe { LLVMSetLinkage(global, LLVMLinkage::LLVMExternalLinkage) };
    unsafe { LLVMSetVisibility(global, LLVMVisibility::LLVMHiddenVisibility) };
    unsafe { LLVMSetInitializer(global, LLVMConstInt(attribute_type, attribute as u64, 0)) };
    unsafe { LLVMSetGlobalConstant(global, 1) };
}

fn path_to_cstring(path: &std::path::Path) -> Result<CString, String> {
    let path_str = path
        .to_str()
        .ok_or(("path is not valid as str").to_string())?;
    CString::new(path_str).map_err(|_| ("path includes invalid null byte").to_string())
}

fn get_isa_version_from_gcn_arch(gcn_arch: &str) -> Result<u32, String> {
    let base: u32 = gcn_arch
        .replace("gfx", "")
        .parse()
        .map_err(|_| ("could not get ISA version from gcn_arch").to_string())?;
    let stepping = base % 10;
    let minor = (base / 10) % 10;
    let major = base / 100;
    Ok(major * 1000 + minor * 100 + stepping)
}

fn create_oclc_constants(ctx: &Context, gcn_arch: &str) -> Result<Module, String> {
    let module = Module::new(ctx, c"oclc_constants");

    // used by ockl
    add_constant(ctx, &module, c"__oclc_wavefrontsize64", 0);
    add_constant(ctx, &module, c"__oclc_wavefrontsize_log2", 5);
    add_constant(ctx, &module, c"__oclc_ABI_version", 500);
    add_constant(
        ctx,
        &module,
        c"__oclc_ISA_version",
        get_isa_version_from_gcn_arch(gcn_arch)?,
    );

    // used by ocml
    add_constant(ctx, &module, c"__oclc_unsafe_math_opt", 0);
    add_constant(ctx, &module, c"__oclc_correctly_rounded_sqrt32", 1);
    add_constant(ctx, &module, c"__oclc_finite_only_opt", 0);
    Ok(module)
}

/// `ZLUDA_CUMODE`, read once for the process.
///
/// `Some(true)` means CU mode, `Some(false)` means WGP mode, `None` means the
/// caller should fall back to the per-architecture default.
///
/// Read once, not per call, because the same answer has to reach two places that
/// must agree: the module cache key (`zluda/src/impl/module.rs` folds `cumode`
/// into the serialised attributes) and the code generator, which reads it per
/// kernel and per partition. If the variable were re-read at each of those
/// points, a change in between would produce a binary whose target features no
/// longer match the key it was stored under, and a build-A binary would answer
/// for a build-B request.
///
/// An unrecognised value is reported rather than silently treated as "auto":
/// getting WGP mode while believing you asked for CU mode is otherwise only
/// discoverable by disassembling the result.
fn cumode_override() -> Option<bool> {
    static OVERRIDE: OnceLock<Option<bool>> = OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        let Ok(value) = std::env::var("ZLUDA_CUMODE") else {
            return None;
        };
        if value.eq_ignore_ascii_case("1")
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("cu")
        {
            Some(true)
        } else if value.eq_ignore_ascii_case("0")
            || value.eq_ignore_ascii_case("false")
            || value.eq_ignore_ascii_case("wgp")
        {
            Some(false)
        } else {
            eprintln!(
                "[zluda] ZLUDA_CUMODE={:?} is not one of 1/true/cu/0/false/wgp; \
                 falling back to the per-architecture default",
                value
            );
            None
        }
    })
}

/// Whether to compile for CU (wavefront) mode rather than WGP mode.
///
/// Note the polarity, which is easy to get backwards: in AMD LLVM the feature is
/// `FeatureCuMode` ("Enable CU wavefront execution mode",
/// `AMDGPU.td`), and the assembler writes `ProgInfo.WgpMode =
/// STM.isCuModeEnabled() ? 0 : 1` (`AMDGPUAsmPrinter.cpp`). So `+cumode` means
/// WGP mode is *off*, i.e. CU mode -- `true` here really is CU mode.
///
/// The default is WGP mode for RDNA (gfx10/gfx11/gfx12) and CU mode for
/// everything else, which is what the hardware supports: WGP mode only exists on
/// RDNA, and gfx9/CDNA have a single CU per work group processor.
pub fn is_cumode(gcn_arch: &str) -> bool {
    match cumode_override() {
        Some(value) => value,
        None => {
            !(gcn_arch.starts_with("gfx10")
                || gcn_arch.starts_with("gfx11")
                || gcn_arch.starts_with("gfx12"))
        }
    }
}

/// How many parts to cut a module into before generating code, from
/// `ZLUDA_CODEGEN_PARTS`.
///
/// One by default. Splitting changes the code that comes out -- the optimiser
/// sees less of the module at a time -- and it was briefly made to default to the
/// host's core count, which had three problems: the output then depended on a
/// property of the machine rather than of the input (so a shared cache could
/// serve one host's split to another), a 32-core host started 32 threads, 32
/// `LLVMContext`s and 32 `TargetMachine`s per module load, and the whole thing
/// was on for everyone without the measurement that the commit which introduced
/// it said it needed first. `ZLUDA_CODEGEN_PARTS=auto` asks for the core count
/// explicitly.
pub fn codegen_parts() -> u32 {
    /// A split is worth something only if code generation dominates, and each
    /// part costs a context, a target machine and a thread. Above a few dozen
    /// the oversubscription is the larger effect, and an unvalidated value out
    /// of the environment must not be able to ask for a million part files.
    const MAX_PARTS: u32 = 32;

    static PARTS: OnceLock<u32> = OnceLock::new();
    *PARTS.get_or_init(|| {
        let cores = || {
            std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1)
        };
        let requested = match std::env::var("ZLUDA_CODEGEN_PARTS") {
            Ok(text) if text.eq_ignore_ascii_case("auto") => cores(),
            Ok(text) => match text.trim().parse::<u32>() {
                Ok(parts) => parts,
                Err(_) => {
                    eprintln!(
                        "[zluda] ZLUDA_CODEGEN_PARTS={:?} is not a number or \"auto\"; \
                         not splitting (1 part)",
                        text
                    );
                    return 1;
                }
            },
            Err(_) => 1,
        };
        let parts = requested.clamp(1, MAX_PARTS);
        if parts != requested {
            eprintln!(
                "[zluda] ZLUDA_CODEGEN_PARTS={} clamped to {}",
                requested, parts
            );
        }
        parts
    })
}

fn make_target_machine(gcn_arch: &str) -> Result<TargetMachine, String> {
    let triple = c"amdgcn-amd-amdhsa";
    let cpu = CString::new(gcn_arch).map_err(|_| ("invalid gcn_arch").to_string())?;
    let features = if is_cumode(gcn_arch) {
        c"-wavefrontsize64,+cumode"
    } else {
        c"-wavefrontsize64,-cumode"
    };

    let mut target = unsafe { std::mem::zeroed() };
    let mut err = ptr::null_mut();
    let status = unsafe { LLVMGetTargetFromTriple(triple.as_ptr(), &mut target, &mut err) };
    if status != 0 {
        let message = Message::new(unsafe { CStr::from_ptr(err) });
        return Err(message.to_str().to_string());
    }
    Ok(TargetMachine::new(
        target,
        triple,
        &cpu,
        features,
        LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive,
        LLVMRelocMode::LLVMRelocDefault,
        LLVMCodeModel::LLVMCodeModelDefault,
    ))
}

fn run_optimizer(module: &Module, target_machine: &TargetMachine) -> Result<(), String> {
    let pb_options = PassBuilderOptions::new();
    let error = unsafe {
        LLVMRunPasses(
            module.get(),
            c"default<O3>".as_ptr(),
            target_machine.get(),
            pb_options.get(),
        )
    };
    if !error.is_null() {
        // `LLVMGetErrorMessage` hands back a heap string that only
        // `LLVMDisposeMessage` releases, and `Message::new` is the *non-owning*
        // constructor, so letting a `Message` go out of scope here does not free
        // it (the `Drop` that disposes is on the owning type in `utils.rs`).
        // Formatting into a `String` and disposing explicitly is what keeps this
        // from leaking one LLVM allocation per failed optimisation.
        let err_msg = unsafe { llvm_sys::error::LLVMGetErrorMessage(error) };
        if err_msg.is_null() {
            return Err("the LLVM optimiser failed with no message".to_string());
        }
        let text = unsafe { CStr::from_ptr(err_msg) }
            .to_string_lossy()
            .into_owned();
        unsafe { llvm_sys::core::LLVMDisposeMessage(err_msg) };
        return Err(text);
    }
    Ok(())
}

// Cuts an already optimised module up and generates code for each part on its
// own thread. Each worker builds its own context and target machine: an
// LLVMContext belongs to one thread, so nothing but bytes crosses between them.
//
// The module must be optimised first, whole. Splitting before that was tried
// and does not work: the helpers the parts share -- ZLUDA's mma, ockl's
// workitem queries -- are linkonce_odr, so the part that receives a definition
// nobody in that part calls drops it, and the parts that do call it are left
// with an undefined symbol at link time. Optimising first also means the code
// each kernel gets is the code it would have got anyway, since the AMDGPU
// backend works a function at a time regardless.
fn emit_objects_in_parallel(
    linked: &Module,
    parts: u32,
    gcn_arch: &str,
) -> Result<Vec<Vec<u8>>, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let prefix = dir.path().join("part_");
    let prefix_cstr = path_to_cstring(&prefix)?;

    let mut err = ptr::null_mut();
    let written = unsafe {
        crate::ffi::LLVMZludaSplitModule(linked.get(), parts, prefix_cstr.as_ptr(), &mut err)
    };
    if written == 0 {
        let detail = if err.is_null() {
            "no parts produced".to_string()
        } else {
            Message::new(unsafe { CStr::from_ptr(err) }).to_str().to_string()
        };
        return Err(format!("Failed to split module: {}", detail));
    }

    let bitcodes = (0..written)
        .map(|i| {
            let path = prefix.with_file_name(format!("part_{}.bc", i));
            fs::read(&path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
        })
        .collect::<Result<Vec<_>, _>>()?;

    std::thread::scope(|scope| {
        let handles = bitcodes
            .iter()
            .map(|bitcode| {
                scope.spawn(move || -> Result<Vec<u8>, String> {
                    let ctx = Context::new();
                    let module = Module::try_from_bitcode(&ctx, bitcode, None)
                        .ok_or_else(|| "Failed to parse a module part".to_string())?;
                    let target_machine = make_target_machine(gcn_arch)?;
                    Ok(target_machine
                        .emit_to_memory_buffer(&module, LLVMCodeGenFileType::LLVMObjectFile)?
                        .to_vec())
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "a code generation thread panicked".to_string())?
            })
            .collect::<Result<Vec<_>, String>>()
    })
}

pub fn compile(
    ctx: &Context,
    gcn_arch: &str,
    main: Module,
    ptx_impl: &[u8],
    attributes: Module,
    metadata: kernel_metadata::ModuleMetadataV1,
    metadata32: Option<kernel_metadata::ModuleMetadata32Bit>,
    compiler_hook: Option<&dyn Fn(&Vec<u8>, String)>,
) -> Result<Vec<u8>, String> {
    init_globals()?;

    // ZLUDA_TIME_PHASES prints how long each stage of this function took.
    // Translating one of the DLSS modules costs about twenty-five minutes on a
    // single thread, which sets the pace of every experiment against the
    // network; before making it parallel it is worth knowing whether the time
    // is in the optimiser or in the backend, because the two are made faster in
    // different ways.
    let timing = std::env::var_os("ZLUDA_TIME_PHASES").is_some();
    let mut mark = std::time::Instant::now();
    let mut phase = |name: &str| {
        if timing {
            eprintln!("[zluda] fase {name}: {:.1} s", mark.elapsed().as_secs_f64());
            mark = std::time::Instant::now();
        }
    };

    let linked = Module::new(ctx, c"llvm-link");

    let ptx_impl = load_module(ctx, ptx_impl, c"ptx_impl.bc")?;
    let ockl = load_module(ctx, OCKL_MODULE, c"ockl.bc")?;

    let oclc_constants = create_oclc_constants(ctx, gcn_arch)?;

    linked.link(main)?;
    linked.link(attributes)?;
    linked.link(oclc_constants)?;
    linked.link(ptx_impl)?;
    linked.link(ockl)?;

    linked.verify()?;
    phase("collegamento");

    if let Some(hook) = compiler_hook {
        // Run compiler hook on human-readable LLVM IR
        let message = linked.print_module_to_string();
        let data = message.to_bytes().to_vec();
        hook(&data, String::from("linked.ll"));
    }

    let target_machine = make_target_machine(gcn_arch)?;

    run_optimizer(&linked, &target_machine)?;
    phase("ottimizzazione O3");

    let parts = codegen_parts();
    let object_files: Vec<Vec<u8>> = if parts > 1 {
        let objects = emit_objects_in_parallel(&linked, parts, gcn_arch)?;
        phase(&format!("generazione codice su {} parti", objects.len()));
        objects
    } else {
        if let Some(hook) = compiler_hook {
            // Run compiler hook on optimized human-readable LLVM IR
            let message = linked.print_module_to_string();
            let data = message.to_bytes().to_vec();
            hook(&data, String::from("opt.ll"));

            // Running a disassembler would be a bit of a pain, so run codegen as assembly
            let assembly = target_machine
                .emit_to_memory_buffer(&linked.clone(), LLVMCodeGenFileType::LLVMAssemblyFile)?
                .to_vec();
            hook(&assembly, String::from("asm"))
        }

        let object = target_machine
            .emit_to_memory_buffer(&linked, LLVMCodeGenFileType::LLVMObjectFile)?
            .to_vec();
        phase("generazione codice");
        vec![object]
    };

    // The split path never runs the per-kernel dumps above, so say so rather than
    // letting a debugging aid disappear without explanation just because the
    // machine has more than one core.
    if parts > 1 && compiler_hook.is_some() {
        eprintln!(
            "[zluda] code generation was split into {} parts, so opt.ll and asm are not \
             written; set ZLUDA_CODEGEN_PARTS=1 to get them",
            object_files.len()
        );
    }

    // The first object is used as the model for the metadata sections below, and
    // `kernel_metadata::write_object` copies its whole ELF header rather than just
    // reading the fields it needs, so this is borrowed rather than cloned: the
    // objects can be tens of megabytes each.
    let object_file = &object_files[0];

    if let Some(hook) = compiler_hook {
        // Run compiler hook for object file
        hook(object_file, String::from("o"));
    }

    // The 32-bit metadata section describes every kernel in the *module* --
    // `convert_32bit_to_64bit` builds its argument map from all of them -- but
    // with the module split there is no single object that carries that whole
    // description, and each part's AMDGPU metadata note describes only its own
    // kernels. Rather than archive a module-wide map against partition 0 and
    // serve a view that silently lacks the kernels living in the other parts,
    // refuse the combination.
    if metadata32.is_some() && object_files.len() > 1 {
        return Err(format!(
            "this module needs the 32-bit metadata section, which describes every kernel at \
             once, but code generation was split into {} objects; set ZLUDA_CODEGEN_PARTS=1",
            object_files.len()
        ));
    }

    // One file per object: with the module split for parallel code generation
    // there is more than one, and the linker call below already takes a list.
    let object_paths = object_files
        .iter()
        .map(|bytes| {
            let path = NamedTempFile::with_prefix("zluda.o")
                .map_err(|e| format!("Failed to create temporary file: {}", e))?
                .into_temp_path();
            fs::write(&path, bytes)
                .map_err(|e| format!("Failed to write object file: {}", e))?;
            Ok(path)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut section_path = NamedTempFile::with_prefix("zluda_section")
        .map_err(|e| format!("Failed to create temporary file: {}", e))?;
    let executable_path = NamedTempFile::with_prefix("zluda.elf")
        .map_err(|e| format!("Failed to create temporary file: {}", e))?
        .into_temp_path();

    metadata
        .write_object(object_file, section_path.as_file_mut())
        .map_err(|e| format!("Failed to write metadata section: {}", e))?;
    let section32_path = if let Some(metadata32) = metadata32 {
        let mut section32_path = NamedTempFile::with_prefix("zluda32_section")
            .map_err(|e| format!("Failed to create temporary file: {}", e))?;
        metadata32
            .write_object(object_file, section32_path.as_file_mut())
            .map_err(|e| format!("Failed to write 32-bit metadata section: {}", e))?;
        Some(section32_path)
    } else {
        None
    };

    let mut input_cstrs = object_paths
        .iter()
        .map(|p| path_to_cstring(p))
        .collect::<Result<Vec<_>, String>>()?;
    input_cstrs.push(path_to_cstring(section_path.as_ref())?);
    if let Some(ref section32_path) = section32_path {
        input_cstrs.push(path_to_cstring(section32_path.as_ref())?);
    }
    let executable_path_cstr = path_to_cstring(&executable_path)?;

    let inputs = input_cstrs.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
    let inputs_len = inputs.len() as u32;

    let mut err = std::ptr::null_mut();
    let result = unsafe {
        LLVMZludaLinkWithLLD(
            inputs_len,
            inputs.as_ptr(),
            executable_path_cstr.as_ptr(),
            &mut err,
        )
    };
    if result != 0 {
        let message = Message::new(unsafe { CStr::from_ptr(err) });
        return Err(message.to_str().to_string());
    }

    let executable =
        fs::read(&executable_path).map_err(|_| ("Failed to read executable file").to_string())?;

    if let Some(hook) = compiler_hook {
        // Run compiler hook for final executable
        hook(&executable, String::from("elf"));
    }

    Ok(executable)
}

fn init_globals() -> Result<(), String> {
    static INIT_AMDGPU: OnceLock<Result<(), Message>> = OnceLock::new();
    INIT_AMDGPU
        .get_or_init(|| {
            let common_options = vec![
                // Uncomment for LLVM debug
                //c"-debug",
                // Uncomment to save passes
                // c"-print-before-all",
                // NOTE: passing an extra option here makes the parse fail before
                // any module compiles (LLVMZludaParseCommandLineOptions returns
                // false and every cuModuleLoadData then fails), so diagnostics in
                // the AMDGPU pipeline are switched on in the pass itself instead.
                // c"-zluda-mma-stats",
                c"llvm_zluda",
                //c"-debug-only=isel",
                c"-ignore-tti-inline-compatible",
                // c"-amdgpu-early-inline-all=true",
                c"-amdgpu-internalize-symbols",
                c"-amdhsa-code-object-version=5",
                //c"--pass-remarks-missed=.*inlin.*",
            ]
            .into_iter();
            /* Does not provide as much performance improvement on ROCm 7.2
            let opt_options = if cfg!(debug_assertions) {
                vec![]
            } else {
                vec![
                    // default inlining threshold times 10
                    c"-inline-threshold=2250",
                    c"-inlinehint-threshold=3250",
                ]
            };
            */
            let llvm_args_ptrs: Vec<*const i8> = common_options
                //.chain(opt_options)
                .map(|s| s.as_ptr())
                .collect();
            let mut err_msg = std::ptr::null_mut();
            let success = unsafe {
                LLVMZludaParseCommandLineOptions(
                    llvm_args_ptrs.len() as i32,
                    llvm_args_ptrs.as_ptr(),
                    &mut err_msg,
                )
            };
            if !success {
                return Err(Message::new(unsafe { CStr::from_ptr(err_msg) }));
            }
            unsafe { LLVMInitializeAMDGPUTargetInfo() };
            unsafe { LLVMInitializeAMDGPUTarget() };
            unsafe { LLVMInitializeAMDGPUTargetMC() };
            unsafe { LLVMInitializeAMDGPUAsmPrinter() };
            Ok(())
        })
        .as_ref()
        .map(|()| ())
        .map_err(|e| e.to_str().to_string())
}
