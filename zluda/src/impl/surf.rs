use hip_runtime_sys::*;
use std::collections::HashMap;
use std::mem;
use std::sync::Mutex;

// HIP only exposes surface creation with the runtime-style descriptor, while the
// CUDA driver API hands us the driver-style one, so it has to be translated.
// Only array resources are handled: a surface over linear or pitched memory would
// also need the channel format descriptor rebuilt, and nothing asks for that yet.
// HIP has no driver-style query for a surface object's resource descriptor, so the
// descriptor has to be remembered at creation time. Only the resource type and its
// handle are kept: those are the two fields object_create consumes, and the rest of
// HIP_RESOURCE_DESC is reserved, so the descriptor can be rebuilt exactly.
static SURFACE_DESCS: Mutex<Option<HashMap<usize, (u32, usize)>>> = Mutex::new(None);

pub(crate) unsafe fn object_create(
    p_surf_object: *mut hipSurfaceObject_t,
    p_res_desc: *const HIP_RESOURCE_DESC,
) -> hipError_t {
    let driver_desc = p_res_desc.as_ref().ok_or(hipErrorCode_t::InvalidValue)?;
    let mut runtime_desc: hipResourceDesc = mem::zeroed();
    if driver_desc.resType == HIPresourcetype::HIP_RESOURCE_TYPE_ARRAY {
        runtime_desc.resType = hipResourceType::hipResourceTypeArray;
        runtime_desc.res.array.array = driver_desc.res.array.hArray;
    } else if driver_desc.resType == HIPresourcetype::HIP_RESOURCE_TYPE_MIPMAPPED_ARRAY {
        runtime_desc.resType = hipResourceType::hipResourceTypeMipmappedArray;
        runtime_desc.res.mipmap.mipmap = driver_desc.res.mipmap.hMipmappedArray;
    } else {
        return Err(hipErrorCode_t::NotSupported);
    }
    hipCreateSurfaceObject(p_surf_object, &runtime_desc)?;
    let handle = match driver_desc.resType {
        HIPresourcetype::HIP_RESOURCE_TYPE_ARRAY => driver_desc.res.array.hArray as usize,
        _ => driver_desc.res.mipmap.hMipmappedArray as usize,
    };
    SURFACE_DESCS
        .lock()
        .map_err(|_| hipErrorCode_t::OperatingSystem)?
        .get_or_insert_with(HashMap::new)
        .insert(*p_surf_object as usize, (driver_desc.resType.0, handle));
    Ok(())
}

pub(crate) unsafe fn object_get_resource_desc(
    p_res_desc: *mut HIP_RESOURCE_DESC,
    surf_object: hipSurfaceObject_t,
) -> hipError_t {
    let (res_type, handle) = *SURFACE_DESCS
        .lock()
        .map_err(|_| hipErrorCode_t::OperatingSystem)?
        .as_ref()
        .and_then(|m| m.get(&(surf_object as usize)))
        .ok_or(hipErrorCode_t::InvalidValue)?;
    let desc = p_res_desc.as_mut().ok_or(hipErrorCode_t::InvalidValue)?;
    *desc = mem::zeroed();
    desc.resType = HIPresourcetype(res_type);
    if desc.resType == HIPresourcetype::HIP_RESOURCE_TYPE_ARRAY {
        desc.res.array.hArray = handle as hipArray_t;
    } else {
        desc.res.mipmap.hMipmappedArray = handle as hipMipmappedArray_t;
    }
    Ok(())
}

pub(crate) unsafe fn object_destroy(surf_object: hipSurfaceObject_t) -> hipError_t {
    if let Ok(mut descs) = SURFACE_DESCS.lock() {
        if let Some(descs) = descs.as_mut() {
            descs.remove(&(surf_object as usize));
        }
    }
    hipDestroySurfaceObject(surf_object)
}
