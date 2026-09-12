use cuda_types::cuda::{CUfunction, CUfunction_attribute, CUkernel};
use dark_api::FunctionArgInfo;
use hip_runtime_sys::*;
use std::io::Write;
use std::mem;

pub(crate) struct Function {
    pub(crate) base: hipFunction_t,
    pub(crate) sm_version: u32,
    pub(crate) explicit_args_size_align: Option<Vec<FunctionArgInfo>>,
    // The module symbol this was created from, kept for launch-timing output.
    pub(crate) name: String,
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
) -> hipError_t {    // The `extra` form packs every argument into one buffer and describes it
    // with a marker list, instead of passing an array of pointers. CUDA and HIP
    // agree on the markers -- BUFFER_POINTER is 1 and BUFFER_SIZE is 2 in both
    // -- but not on the terminator: CUDA ends the list with a null pointer,
    // HIP with 0x03. Forwarding a CUDA list unchanged would leave HIP scanning
    // past the end, so the list is rebuilt with HIP's terminator.
    //
    // This is not a corner: DLSS launches every one of its kernels this way,
    // and refusing the form made cuLaunchKernel answer NOT_SUPPORTED.
    let mut translated = [std::ptr::null_mut(); 17];
    let extra = if extra.is_null() {
        extra
    } else {
        // A marker and its value come in pairs. The bound is a guard against a
        // list that is not terminated at all rather than a real limit; the
        // defined markers only allow two pairs.
        const MAX_ENTRIES: usize = 16;
        let mut count = 0;
        unsafe {
            let mut i = 0;
            while i < MAX_ENTRIES {
                let marker = *extra.add(i);
                if marker.is_null() {
                    break;
                }
                translated[count] = marker;
                translated[count + 1] = *extra.add(i + 1);
                count += 2;
                i += 2;
            }
            if i >= MAX_ENTRIES {
                return hipError_t::ErrorInvalidValue;
            }
        }
        // HIP_LAUNCH_PARAM_END. It is a macro in hip_runtime_api.h, so bindgen
        // does not carry it into hip_runtime-sys and it has to be spelled out.
        translated[count] = 0x03 as *mut ::core::ffi::c_void;
        translated.as_mut_ptr()
    };
    if launch_timing_enabled() {
        return unsafe { timed_launch(
            f,
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
        ) };
    }
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

// ZLUDA_LAUNCH_TIMING=1: bracket every launch with an event pair on the same
// stream and report the GPU duration of the kernel alone. A diagnostic for
// workloads where a frame's time is spread over many small launches and the
// question is which kernel eats it: this answers per kernel, at the cost of a
// host round trip per launch (the events sync before the elapsed time is
// read), which is why it is off unless asked for.
//
// One event pair is reused for the whole process: launches on one stream are
// serialized anyway, so a live start/stop pair is never read while another
// launch is in flight.
fn launch_timing_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ZLUDA_LAUNCH_TIMING")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

unsafe fn timed_launch(
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
    use std::sync::OnceLock;
    // Raw HIP handles are not Send/Sync by their type, but the event pair is
    // only ever touched from the thread that created it after one-time
    // initialization; wrap it so the static can hold it.
    struct EventPair(hipEvent_t, hipEvent_t);
    unsafe impl Send for EventPair {}
    unsafe impl Sync for EventPair {}
    static EVENTS: OnceLock<EventPair> = OnceLock::new();
    let &EventPair(start, stop) = EVENTS.get_or_init(|| {
        let mut start: hipEvent_t = std::ptr::null_mut();
        let mut stop: hipEvent_t = std::ptr::null_mut();
        if hipEventCreateWithFlags(&mut start, 0) != hipError_t::Success {
            return EventPair(std::ptr::null_mut(), std::ptr::null_mut());
        }
        if hipEventCreateWithFlags(&mut stop, 0) != hipError_t::Success {
            return EventPair(start, std::ptr::null_mut());
        }
        EventPair(start, stop)
    });
    if start.is_null() || stop.is_null() {
        // Events unavailable: fall through to a plain launch rather than
        // failing the application over a diagnostic.
        return hipModuleLaunchKernel(
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
        );
    }
    hipEventRecord(start, stream);
    let result = hipModuleLaunchKernel(
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
    );
    hipEventRecord(stop, stream);
    hipEventSynchronize(stop);
    let mut ms = 0f32;
    if hipEventElapsedTime(&mut ms, start, stop) == hipError_t::Success && result == hipError_t::Success {
        let _ = writeln!(
            std::io::stderr(),
            "[zluda-launch] {} f={:p} grid={}x{}x{} block={}x{}x{} gpu_ms={:.3}",
            f.name,
            f.base.0,
            grid_dim_x,
            grid_dim_y,
            grid_dim_z,
            block_dim_x,
            block_dim_y,
            block_dim_z,
            ms
        );
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
