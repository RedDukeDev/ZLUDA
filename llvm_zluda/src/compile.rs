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

extern "C" {
    // Forces CombineMMA's wrapper opening on (1) or off (0), or hands the
    // decision back to the ZLUDA_MMA_OPEN environment variable (-1). Defined in
    // the fork's CombineMMA.cpp; used by the selective build to compile a
    // module both ways.
    fn zludaSetMMAOpen(on: std::ffi::c_int);
}

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

// Whether this process is building for a generic family target. Read from the
// same variable that chooses the target, because the LLVM options that have to
// agree with it are parsed once for the process, before any module is seen.
fn targeting_generic_family() -> bool {
    static GENERIC: OnceLock<bool> = OnceLock::new();
    *GENERIC.get_or_init(|| {
        std::env::var("ZLUDA_TARGET_ARCH")
            .map(|arch| arch.contains("generic"))
            .unwrap_or(false)
    })
}

fn get_isa_version_from_gcn_arch(gcn_arch: &str) -> Result<u32, String> {
    // A generic target names a whole family rather than one part -- one binary
    // that loads on every GPU in it. The device libraries and this project's own
    // bitcode branch on __oclc_ISA_version, and the FP8 path picks its emulation
    // by it, so a family gets the version of its first member: that is the same
    // choice every member of the family would have made.
    if let Some(family) = match gcn_arch {
        "gfx10-3-generic" => Some(10300),
        "gfx11-generic" => Some(11000),
        "gfx12-generic" => Some(12000),
        _ => None,
    } {
        return Ok(family);
    }
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
    // Must agree with -amdhsa-code-object-version above: 500 is version 5,
    // 600 is version 6, which the generic family targets require.
    add_constant(
        ctx,
        &module,
        c"__oclc_ABI_version",
        if targeting_generic_family() { 600 } else { 500 },
    );
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

fn make_target_machine(gcn_arch: &str) -> Result<TargetMachine, String> {
    let triple = c"amdgcn-amd-amdhsa";
    let cpu = CString::new(gcn_arch).map_err(|_| ("invalid gcn_arch").to_string())?;
    let features = c"-wavefrontsize64,+cumode";

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
        let err_msg = unsafe { llvm_sys::error::LLVMGetErrorMessage(error) };
        let message = Message::new(unsafe { CStr::from_ptr(err_msg) });
        return Err(message.to_str().to_string());
    }
    Ok(())
}

// How many parts to cut the module into before generating code, from
// ZLUDA_CODEGEN_PARTS. Absent or below two keeps the whole module in one piece,
// which is what every build did before this existed.
//
// Off by default on purpose: splitting narrows what the optimiser can see, so
// the code that comes out is not the same code. Whether that costs anything at
// run time has to be measured on the network, not assumed, and until it has
// been the fast path stays opt-in.
fn codegen_parts() -> u32 {
    match std::env::var("ZLUDA_CODEGEN_PARTS").ok().as_deref() {
        Some("auto") => std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
        Some(text) => text.parse().unwrap_or(1),
        None => 1,
    }
}

// Optimises a module and generates its code, the way the ordinary path does,
// packaged so it can be run twice on two copies of a module -- once with the
// matrix-multiply fusion on and once off -- when the selective build asks for
// it. The debug hook is left out: it is a single-module aid and does not fit a
// path that builds two.
fn optimize_and_emit(
    module: &Module,
    gcn_arch: &str,
    parts: u32,
) -> Result<Vec<Vec<u8>>, String> {
    let target_machine = make_target_machine(gcn_arch)?;
    run_optimizer(module, &target_machine)?;
    if parts > 1 {
        emit_objects_in_parallel(module, parts, gcn_arch)
    } else {
        let object = target_machine
            .emit_to_memory_buffer(module, LLVMCodeGenFileType::LLVMObjectFile)?
            .to_vec();
        Ok(vec![object])
    }
}

// Whether to build each module both ways and keep the fused one only where it
// pays, and the per-kernel code-size ratio that decides "pays". Driven by
// ZLUDA_SELECTIVE_FUSE: absent, empty or "0" turns it off (the default, since
// it doubles translation time); any other value turns it on, and a value that
// parses as a ratio above one sets the threshold, otherwise it is the default
// below.
//
// The default, 2.13, is where the network's kernels separate: the spatial
// kernels fusion speeds up grow by at most 2.08x and the transformer ones it
// slows down by 2.18x and more. That gap is narrow, and object size is only a
// proxy for the register pressure that actually decides it, so the threshold is
// not a sharp line -- but it does not have to be. Fusion is bit-for-bit
// identical either way, so a wrong call costs a little speed and never
// correctness, and the shipped cache is measured before it goes out. Whole
// networks that separate elsewhere can pass their own ratio.
fn selective_fuse_threshold() -> Option<f64> {
    let value = std::env::var("ZLUDA_SELECTIVE_FUSE").ok()?;
    if value.is_empty() || value == "0" {
        return None;
    }
    Some(value.parse().ok().filter(|r: &f64| *r > 1.0).unwrap_or(2.13))
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
            eprintln!("[zluda] phase {name}: {:.1} s", mark.elapsed().as_secs_f64());
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
    phase("link");

    if let Some(hook) = compiler_hook {
        // Run compiler hook on human-readable LLVM IR
        let message = linked.print_module_to_string();
        let data = message.to_bytes().to_vec();
        hook(&data, String::from("linked.ll"));
    }

    let parts = codegen_parts();
    let object_files: Vec<Vec<u8>> = if let Some(threshold) = selective_fuse_threshold() {
        // Fuse the matrix multiplies only where it pays.
        //
        // CombineMMA can fuse two of NVIDIA's m16n8k16 multiplies that share
        // their A operand into one AMD 16x16x16 -- the FP8 path issues each
        // multiply half-utilised, so this halves them, and the matrix multiply
        // is the great majority of these kernels' time. But to fuse it has to
        // open the noinline wrapper the intrinsic hides in, and that removes a
        // scheduling barrier: on some kernels the register pressure then
        // explodes into spill traffic that costs more than the fusion saves.
        // Measured across the network, the spatial (tinlayout) kernels win by
        // about 20% and the transformer (vit) kernels lose by far more.
        //
        // So each module is built both ways and the fused result kept only
        // when it did not balloon. Kernel code size is the proxy for the spill
        // traffic that decides it: the kernels fusion speeds up grow by up to
        // about 2.08x, the ones it slows down by 2.18x and more, and the
        // threshold sits between. The comparison is per module, which is enough
        // because the network's modules are homogeneous -- each is all spatial
        // or all transformer kernels -- and it is taken from the worst kernel
        // rather than the module total, which an average over the many kernels
        // that carry no multiplies would otherwise bury.
        //
        // This doubles a module's translation time, so it is off unless asked
        // for: it is meant for building the shipped cache, where the cost is
        // paid once. CombineMMA takes the fusion flag through zludaSetMMAOpen,
        // a direct setter rather than the environment, because on Windows a
        // std::env::set_var here does not reach the C runtime's getenv in the
        // pass. The optimiser runs single-threaded, so setting it around each
        // attempt is safe as long as compile() is not entered concurrently
        // within one process -- and it is not, modules translate one at a time
        // or in separate processes under the precompile tool.
        let fused_module = linked.clone();

        unsafe { zludaSetMMAOpen(0) };
        let unfused = optimize_and_emit(&linked, gcn_arch, parts)?;
        phase("optimisation and codegen, unfused");

        unsafe { zludaSetMMAOpen(1) };
        let fused = optimize_and_emit(&fused_module, gcn_arch, parts);
        unsafe { zludaSetMMAOpen(-1) };
        let fused = fused?;
        phase("optimisation and codegen, fused");

        // The decision is per kernel, taken from the worst one, not from the
        // module's total size. A module holds tens of kernels and only some
        // carry FP8 multiplies; averaging over all of them buries the few that
        // balloon -- the transformer module's total grows only 1.28x while its
        // qkv kernel alone grows 1.94x. Since the network's modules are
        // homogeneous -- all spatial or all transformer -- the worst kernel
        // decides the module, and the whole module is kept fused or not.
        let sizes = |objects: &Vec<Vec<u8>>| {
            let mut map = std::collections::HashMap::<String, u64>::new();
            for object in objects {
                for (name, size) in kernel_metadata::kernel_code_sizes(object) {
                    *map.entry(name).or_insert(0) += size;
                }
            }
            map
        };
        let un_sizes = sizes(&unfused);
        let fu_sizes = sizes(&fused);
        // Ignore kernels too small to trust a ratio from, and kernels fusion
        // did not touch (identical size): the signal is in the ones it grew.
        let mut worst = 1.0f64;
        let mut worst_kernel = String::new();
        for (name, &un_size) in &un_sizes {
            let fu_size = fu_sizes.get(name).copied().unwrap_or(un_size);
            if un_size < 4096 || fu_size <= un_size {
                continue;
            }
            let ratio = fu_size as f64 / un_size as f64;
            if ratio > worst {
                worst = ratio;
                worst_kernel = name.clone();
            }
        }
        let keep_fused = worst < threshold;
        eprintln!(
            "[zluda] fusion: worst kernel {:.2}x ({}) -> {}",
            worst,
            if worst_kernel.is_empty() {
                "no kernel fused"
            } else {
                &worst_kernel
            },
            if keep_fused { "keeping fused" } else { "keeping unfused" }
        );
        if keep_fused {
            fused
        } else {
            unfused
        }
    } else {
        let target_machine = make_target_machine(gcn_arch)?;

        run_optimizer(&linked, &target_machine)?;
        phase("O3 optimisation");

        if parts > 1 {
            let objects = emit_objects_in_parallel(&linked, parts, gcn_arch)?;
            phase(&format!("codegen in {} parts", objects.len()));
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
            phase("codegen");
            vec![object]
        }
    };

    // Any of the objects serves as the model for the metadata sections below:
    // all that is read from it is the ELF header, which they share.
    let object_file = object_files[0].clone();

    if let Some(hook) = compiler_hook {
        // Run compiler hook for object file
        hook(&object_file, String::from("o"));
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
        .write_object(&object_file, section_path.as_file_mut())
        .map_err(|e| format!("Failed to write metadata section: {}", e))?;
    let section32_path = if let Some(metadata32) = metadata32 {
        let mut section32_path = NamedTempFile::with_prefix("zluda32_section")
            .map_err(|e| format!("Failed to create temporary file: {}", e))?;
        metadata32
            .write_object(&object_file, section32_path.as_file_mut())
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
                c"llvm_zluda",
                //c"-debug-only=isel",
                c"-ignore-tti-inline-compatible",
                // c"-amdgpu-early-inline-all=true",
                c"-amdgpu-internalize-symbols",
                // Code object version 5 unless a generic family target is
                // asked for: those are refused below version 6, which is what
                // carries the load-time resolution that lets one binary serve a
                // whole family. These options are parsed once for the process,
                // before any module names its target, so the choice is read
                // from the same variable that picks the target.
                if targeting_generic_family() {
                    c"-amdhsa-code-object-version=6"
                } else {
                    c"-amdhsa-code-object-version=5"
                },
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
