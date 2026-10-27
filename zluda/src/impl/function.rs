use crate::r#impl::driver;
use cuda_types::cuda::{CUerror, CUfunction, CUfunction_attribute, CUkernel};
use dark_api::FunctionArgInfo;
use hip_runtime_sys::*;
use std::mem;

pub(crate) struct Function {
    pub(crate) base: hipFunction_t,
    pub(crate) name: std::ffi::CString,
    pub(crate) sm_version: u32,
    pub(crate) explicit_args_size_align: Option<Vec<FunctionArgInfo>>,
}

impl<'a, E: zluda_common::CudaErrorType> zluda_common::FromCuda<'a, CUfunction, E>
    for &'a Function
{
    fn from_cuda(cu_func: &'a CUfunction) -> Result<Self, E> {
        Ok(unsafe { mem::transmute(*cu_func) })
    }
}

impl<'a, E: zluda_common::CudaErrorType> zluda_common::FromCuda<'a, CUkernel, E> for &'a Function {
    fn from_cuda(cu_func: &'a CUkernel) -> Result<Self, E> {
        Ok(unsafe { mem::transmute(*cu_func) })
    }
}

impl<'a, E: zluda_common::CudaErrorType> zluda_common::FromCuda<'a, *mut CUfunction, E>
    for &'a mut &'a Function
{
    fn from_cuda(cu_func: &'a *mut CUfunction) -> Result<Self, E> {
        let cu_func = unsafe { cu_func.as_mut() }.ok_or_else(|| E::INVALID_VALUE)?;
        Ok(unsafe { mem::transmute(cu_func) })
    }
}

impl<'a, E: zluda_common::CudaErrorType> zluda_common::FromCuda<'a, *mut CUkernel, E>
    for &'a mut &'a Function
{
    fn from_cuda(cu_func: &'a *mut CUkernel) -> Result<Self, E> {
        let cu_func = unsafe { cu_func.as_mut() }.ok_or_else(|| E::INVALID_VALUE)?;
        Ok(unsafe { mem::transmute(cu_func) })
    }
}

pub(crate) fn get_attribute(
    pi: &mut i32,
    cu_attrib: CUfunction_attribute,
    func: &Function,
) -> Result<(), CUerror> {
    match cu_attrib {
        CUfunction_attribute::CU_FUNC_ATTRIBUTE_PTX_VERSION => {
            *pi = func.sm_version as i32;
            return Ok(());
        }
        CUfunction_attribute::CU_FUNC_ATTRIBUTE_BINARY_VERSION => {
            let (major_cap, minor_cap) = driver::global_state()?.compute_capability;
            *pi = major_cap * 10 + minor_cap;
            return Ok(());
        }
        CUfunction_attribute::CU_FUNC_ATTRIBUTE_CLUSTER_SIZE_MUST_BE_SET
        | CUfunction_attribute::CU_FUNC_ATTRIBUTE_REQUIRED_CLUSTER_WIDTH
        | CUfunction_attribute::CU_FUNC_ATTRIBUTE_REQUIRED_CLUSTER_HEIGHT
        | CUfunction_attribute::CU_FUNC_ATTRIBUTE_REQUIRED_CLUSTER_DEPTH
        | CUfunction_attribute::CU_FUNC_ATTRIBUTE_NON_PORTABLE_CLUSTER_SIZE_ALLOWED
        | CUfunction_attribute::CU_FUNC_ATTRIBUTE_CLUSTER_SCHEDULING_POLICY_PREFERENCE => {
            *pi = 0;
            return Ok(());
        }
        _ => {}
    }
    unsafe { hipFuncGetAttribute(pi, mem::transmute(cu_attrib), func.base) }?;
    if cu_attrib == CUfunction_attribute::CU_FUNC_ATTRIBUTE_NUM_REGS {
        *pi = (*pi).max(1);
    }
    Ok(())
}


// ZLUDA_DEBUG_BUFFERS=1: after every launch, a summary of each allocation the kernel's arguments point
// into (how much of it is not zero, the mean magnitude read as half floats), to find the first kernel
// whose output is wrong.
unsafe fn debug_buffers(arguments: &[u8], grid: (u32, u32, u32), block: (u32, u32, u32)) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let number = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut seen: Vec<(usize, usize)> = Vec::new();
    if let Ok(global_state) = super::driver::global_state() {
        if let Ok(allocations) = global_state.allocations.lock() {
            for word in arguments.chunks_exact(8) {
                let value = u64::from_le_bytes(word.try_into().unwrap()) as usize;
                if value < 0x10000 {
                    continue;
                }
                if let Some((offset, info)) = allocations.get_offset_and_info(value) {
                    let base = value - offset;
                    if !seen.iter().any(|(b, _)| *b == base) {
                        seen.push((base, info.size));
                    }
                }
            }
        }
    }
    hipDeviceSynchronize();
    let mut line = format!(
        "[zluda buf] #{} grid {},{},{} block {},{},{}:",
        number, grid.0, grid.1, grid.2, block.0, block.1, block.2
    );
    for (base, size) in seen {
        let bytes = size.min(8 << 20) & !1;
        if bytes == 0 {
            continue;
        }
        let mut data = vec![0u8; bytes];
        if hipMemcpyDtoH(data.as_mut_ptr().cast(), hipDeviceptr_t(base as *mut _), bytes) != hipError_t::Success {
            continue;
        }
        let mut nonzero = 0usize;
        let mut magnitude = 0f64;
        let words = bytes / 2;
        for w in data.chunks_exact(2) {
            let h = u16::from_le_bytes([w[0], w[1]]);
            if h != 0 {
                nonzero += 1;
                let exp = ((h >> 10) & 31) as i32;
                if exp != 31 {
                    let mant = (h & 1023) as f64;
                    magnitude += if exp == 0 { mant / 1024.0 * 2f64.powi(-14) } else { (1.0 + mant / 1024.0) * 2f64.powi(exp - 15) };
                }
            }
        }
        line.push_str(&format!(
            " [{:x}+{}K nz={:.1}% mag={:.4}]",
            base & 0xffffff,
            size >> 10,
            100.0 * nonzero as f64 / words as f64,
            magnitude / words as f64
        ));
    }
    eprintln!("{}", line);
}

pub(crate) unsafe fn launch_kernel(
    f: &Function,
    grid_dim_x: ::core::ffi::c_uint,
    grid_dim_y: ::core::ffi::c_uint,
    grid_dim_z: ::core::ffi::c_uint,
    block_dim_x: ::core::ffi::c_uint,
    block_dim_y: ::core::ffi::c_uint,
    block_dim_z: ::core::ffi::c_uint,
    shared_mem_bytes: ::core::ffi::c_uint,
    stream: hipStream_t,
    kernel_params: *mut *mut ::core::ffi::c_void,
    extra: *mut *mut ::core::ffi::c_void,
) -> hipError_t {
    // ZLUDA_FLAT_WORK_GROUP_SIZE promises the backend that no workgroup is
    // larger than this, which lets it give a kernel far more registers: the
    // DLSS network's hot kernels go from 96 registers and 610 spilled to 256
    // and 29. It is a promise and not a hint, and translation happens long
    // before any launch is seen, so nothing can check it there.
    //
    // Breaking it does not corrupt anything: the dispatch is refused and the
    // driver answers 719, which on its own says nothing about why -- so it is
    // said here.
    {
        static PROMISED: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
        let promised = PROMISED.get_or_init(|| {
            std::env::var("ZLUDA_FLAT_WORK_GROUP_SIZE")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
        });
        if let Some(limit) = *promised {
            let threads = block_dim_x
                .saturating_mul(block_dim_y)
                .saturating_mul(block_dim_z);
            if threads > limit {
                eprintln!(
                    "[zluda] this kernel was translated with ZLUDA_FLAT_WORK_GROUP_SIZE={limit} and is being launched with {threads} threads per group ({block_dim_x}x{block_dim_y}x{block_dim_z}). The backend was promised the smaller number and gave the kernel registers on that basis, so the driver will refuse the launch. Raise the variable to at least {threads}, or leave it unset."
                );
            }
        }
    }

    // The `extra` form packs every argument into one buffer and describes it
    // with a marker list, instead of passing an array of pointers. CUDA and HIP
    // agree on the markers -- BUFFER_POINTER is 1 and BUFFER_SIZE is 2 in both
    // -- but not on the terminator: CUDA ends the list with a null pointer,
    // HIP with 0x03. Forwarding a CUDA list unchanged would leave HIP scanning
    // past the end, so the list is rebuilt with HIP's terminator.
    //
    // This is not a corner: DLSS launches every one of its kernels this way,
    // and refusing the form made cuLaunchKernel answer NOT_SUPPORTED.
    let mut translated;
    let extra = if extra.is_null() {
        extra
    } else {
        // A marker and its value come in pairs. The bound is a guard against a
        // list that is not terminated at all rather than a real limit; the
        // defined markers only allow two pairs.
        const MAX_ENTRIES: usize = 16;
        translated = Vec::with_capacity(MAX_ENTRIES + 1);
        unsafe {
            let mut i = 0;
            while i < MAX_ENTRIES {
                let marker = *extra.add(i);
                if marker.is_null() {
                    break;
                }
                translated.push(marker);
                translated.push(*extra.add(i + 1));
                i += 2;
            }
            if i >= MAX_ENTRIES {
                return hipError_t::ErrorInvalidValue;
            }
        }
        // HIP_LAUNCH_PARAM_END. It is a macro in hip_runtime_api.h, so bindgen
        // does not carry it into hip_runtime-sys and it has to be spelled out.
        translated.push(0x03 as *mut ::core::ffi::c_void);
        translated.as_mut_ptr()
    };
    // ZLUDA_KERNEL_TIMES=1: each launch is waited for and its time, launch included, is printed
    // with the kernel's name, to find the kernel that makes a network slower.
    let timing = std::env::var_os("ZLUDA_KERNEL_TIMES")
        .is_some()
        .then(|| {
            unsafe { hipDeviceSynchronize() };
            std::time::Instant::now()
        });
    let result = unsafe {
        hipModuleLaunchKernel(
            f.base,
            grid_dim_x,
            grid_dim_y,
            grid_dim_z,
            block_dim_x,
            block_dim_y,
            block_dim_z,
            shared_mem_bytes,
            stream,
            kernel_params,
            extra,
        )
    };
    if let Some(start) = timing {
        unsafe { hipDeviceSynchronize() };
        eprintln!(
            "[zluda time] {} grid {},{},{} block {},{},{} {} us",
            f.name.to_string_lossy(),
            grid_dim_x,
            grid_dim_y,
            grid_dim_z,
            block_dim_x,
            block_dim_y,
            block_dim_z,
            start.elapsed().as_micros()
        );
    }
    if std::env::var_os("ZLUDA_DEBUG_BUFFERS").is_some() {
        let mut arguments: &[u8] = &[];
        if !extra.is_null() {
            unsafe {
                let mut i = 0;
                let mut data: *const u8 = std::ptr::null();
                let mut length = 0usize;
                while i < 16 {
                    let marker = *extra.add(i) as usize;
                    if marker == 0 || marker == 3 {
                        break;
                    }
                    let value = *extra.add(i + 1);
                    if marker == 1 {
                        data = value as *const u8;
                    } else if marker == 2 {
                        length = *(value as *const usize);
                    }
                    i += 2;
                }
                if !data.is_null() {
                    arguments = std::slice::from_raw_parts(data, length);
                }
            }
        }
        unsafe {
            debug_buffers(
                arguments,
                (grid_dim_x, grid_dim_y, grid_dim_z),
                (block_dim_x, block_dim_y, block_dim_z),
            );
        }
    }
    if std::env::var_os("ZLUDA_DEBUG_ARRAYS").is_some() {
        unsafe {
            hipDeviceSynchronize();
            eprint!(
                "[zluda launch] grid {},{},{} block {},{},{} -> {:?}  ",
                grid_dim_x, grid_dim_y, grid_dim_z, block_dim_x, block_dim_y, block_dim_z, result
            );
            super::array::debug_report();
        }
    }
    result
}

pub(crate) unsafe fn set_attribute(
    func: &Function,
    attribute: CUfunction_attribute,
    value: i32,
) -> hipError_t {
    match attribute {
        CUfunction_attribute::CU_FUNC_ATTRIBUTE_PTX_VERSION
        | CUfunction_attribute::CU_FUNC_ATTRIBUTE_BINARY_VERSION => {
            return hipError_t::ErrorNotSupported;
        }
        _ => {}
    }
    hipFuncSetAttribute(func.base.0.cast(), hipFuncAttribute(attribute.0), value)
}

pub(crate) unsafe fn set_cache_config(
    _func: &Function,
    _config: cuda_types::cuda::CUfunc_cache,
) -> hipError_t {
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::tests::CudaApi;
    use cuda_macros::test_cuda;
    use cuda_types::cuda::{CUdevice_attribute, CUfunction_attribute};
    use std::mem;

    #[test_cuda]
    fn check_versions(api: impl CudaApi) {
        api.cuInit(0);
        let mut ctx = unsafe { mem::zeroed() };
        api.cuCtxCreate_v2(&mut ctx, 0, 0);
        let mut major = 0;
        api.cuDeviceGetAttribute(
            &mut major,
            CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR,
            0,
        );
        let mut minor = 0;
        api.cuDeviceGetAttribute(
            &mut minor,
            CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR,
            0,
        );
        let ptx = c".version 8.7
        .target sm_60
        .address_size 64

        .entry foobar() { ret; }
        ";

        let mut module = unsafe { mem::zeroed() };
        api.cuModuleLoadData(&mut module, ptx.as_ptr().cast());
        let mut func = unsafe { mem::zeroed() };
        api.cuModuleGetFunction(&mut func, module, c"foobar".as_ptr().cast());
        let mut binary_version = 0;
        api.cuFuncGetAttribute(
            &mut binary_version,
            CUfunction_attribute::CU_FUNC_ATTRIBUTE_BINARY_VERSION,
            func,
        );
        let mut ptx_version = 0;
        api.cuFuncGetAttribute(
            &mut ptx_version,
            CUfunction_attribute::CU_FUNC_ATTRIBUTE_PTX_VERSION,
            func,
        );
        assert_eq!(binary_version, major * 10 + minor);
        assert_eq!(ptx_version, 60);
    }
}
