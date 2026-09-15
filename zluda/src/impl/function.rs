use cuda_types::cuda::{CUfunction, CUfunction_attribute, CUkernel};
use dark_api::FunctionArgInfo;
use hip_runtime_sys::*;
use std::mem;

pub(crate) struct Function {
    pub(crate) base: hipFunction_t,
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
) -> hipError_t {
    match cu_attrib {
        CUfunction_attribute::CU_FUNC_ATTRIBUTE_PTX_VERSION => {
            *pi = func.sm_version as i32;
            return Ok(());
        }
        CUfunction_attribute::CU_FUNC_ATTRIBUTE_BINARY_VERSION => {
            *pi = 120;
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

pub(crate) fn launch_kernel(
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
    unsafe {
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
    }
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
