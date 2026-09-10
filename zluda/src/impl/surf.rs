use hip_runtime_sys::*;
use std::collections::HashMap;
use std::mem;
use std::ptr;
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

// A surface object has no sampler, and a `tex` through one needs it anyway.
//
// The device side takes the image descriptor from the object's own address and
// the sampler from a fixed offset into that same page (`get_image_and_sampler`
// in `ptx/lib/zluda_ptx_impl.cpp`). A texture object carries a sampler there. A
// surface object does not: HIP uses the space for bookkeeping of its own, and
// what sits in it is a host pointer.
//
// Nothing stops a program from handing a surface object to a `tex`
// instruction, and DLSS does exactly that for the picture its network reads.
// The fetch then sampled with four words of heap address -- different in every
// process, unchanging within one, settled the moment the object is made. That
// is why the network produced one of three pictures for the same input, and
// why nothing in device memory, in the arguments, or in the hardware could be
// found to differ: the sampler was never anywhere those were looked for.
//
// So a surface object is given one. A plain sampler at creation, so a `tex`
// through a surface is at least defined on its own; then the sampler of the
// texture object over the same array, as soon as the program makes one, since
// that is the sampling the program actually asked for.
//
// Nothing is written on faith: the two pages must agree on the whole image
// descriptor first. If they do not, the layout is not what this assumes and
// the object is left alone.
const SAMPLER_OFFSET: usize = 48; // twelve dwords of image descriptor
const SAMPLER_BYTES: usize = 16;

static SURFACES: Mutex<Option<Vec<(usize, usize)>>> = Mutex::new(None);

unsafe fn read_object_page(object: usize, into: &mut [u8]) -> bool {
    hipMemcpyDtoH(
        into.as_mut_ptr().cast(),
        hipDeviceptr_t(object as *mut _),
        into.len(),
    ) == hipError_t::Success
}

// Put `texture`'s sampler onto every surface object over the same array.
pub(crate) unsafe fn adopt_sampler(array: usize, texture: usize) {
    if array == 0 || texture == 0 {
        return;
    }
    let surfaces: Vec<usize> = match SURFACES.lock() {
        Ok(map) => map
            .as_ref()
            .map(|list| {
                list.iter()
                    .filter(|(a, _)| *a == array)
                    .map(|(_, object)| *object)
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => return,
    };
    if surfaces.is_empty() {
        return;
    }
    let mut from = [0u8; SAMPLER_OFFSET + SAMPLER_BYTES];
    if !read_object_page(texture, &mut from) {
        return;
    }
    for object in surfaces {
        let mut onto = [0u8; SAMPLER_OFFSET + SAMPLER_BYTES];
        if !read_object_page(object, &mut onto) {
            continue;
        }
        // The same array through two objects has to describe the same image;
        // anything else means the page is not laid out the way this expects.
        //
        // Said out loud, and once, because the failure is otherwise invisible:
        // nothing breaks here, the sampler simply never gets written, and what
        // comes back is the fault this exists to prevent -- a `tex` sampling
        // with whatever HIP keeps at that offset, which reads as a network that
        // works some of the time and not others.
        //
        // On RDNA (GFX10/GFX11), the hardware image descriptor is 8 DWORDs (32 bytes),
        // whereas legacy GCN used 12 DWORDs (48 bytes). In modern Windows ROCm / HIP,
        // the trailing bytes of the 48-byte region in surface objects contain runtime
        // bookkeeping. Allow match if either the full 48-byte region matches or the
        // core 32-byte hardware image descriptor matches:
        let agrees_48 = onto[..SAMPLER_OFFSET] == from[..SAMPLER_OFFSET];
        let agrees_32 = onto[..32] == from[..32];

        if !agrees_48 && !agrees_32 {
            static COMPLAINED: std::sync::Once = std::sync::Once::new();
            COMPLAINED.call_once(|| {
                eprintln!(
                    "[zluda] a texture object and a surface object over the same array do not agree on their image descriptor (neither 48B nor 32B match), so the surface cannot be given a sampler. See the note in zluda/src/impl/surf.rs."
                );
            });
            continue;
        }

        // Determine sampler source offset: if from[48..64] contains the sampler, use 48;
        // if from[32..48] contains the sampler, use 32. Default to SAMPLER_OFFSET (48).
        let src_sampler_offset = if from[SAMPLER_OFFSET..SAMPLER_OFFSET + SAMPLER_BYTES]
            .iter()
            .any(|&b| b != 0)
        {
            SAMPLER_OFFSET
        } else if from[32..32 + SAMPLER_BYTES].iter().any(|&b| b != 0) {
            32
        } else {
            SAMPLER_OFFSET
        };

        let _ = hipMemcpyHtoD(
            hipDeviceptr_t((object + SAMPLER_OFFSET) as *mut _),
            from[src_sampler_offset..].as_ptr() as *mut _,
            SAMPLER_BYTES,
        );
    }
}

// A sampler built by HIP itself rather than by spelling out its bits here: a
// throwaway texture object over the same resource is made, its sampler taken,
// and the object released.
unsafe fn give_plain_sampler(res_desc: &HIP_RESOURCE_DESC, array: usize) {
    let mut tex_desc: HIP_TEXTURE_DESC = mem::zeroed();
    tex_desc.addressMode = [HIPaddress_mode::HIP_TR_ADDRESS_MODE_CLAMP; 3];
    tex_desc.filterMode = HIPfilter_mode::HIP_TR_FILTER_MODE_POINT;
    tex_desc.mipmapFilterMode = HIPfilter_mode::HIP_TR_FILTER_MODE_POINT;
    let mut texture: hipTextureObject_t = ptr::null_mut();
    if hipTexObjectCreate(&mut texture, res_desc, &tex_desc, ptr::null())
        != hipError_t::Success
    {
        return;
    }
    adopt_sampler(array, texture as usize);
    let _ = hipTexObjectDestroy(texture);
}

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
    if let Ok(mut list) = SURFACES.lock() {
        list.get_or_insert_with(Vec::new)
            .push((handle, *p_surf_object as usize));
    }
    give_plain_sampler(driver_desc, handle);
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
    if let Ok(mut list) = SURFACES.lock() {
        if let Some(list) = list.as_mut() {
            list.retain(|(_, object)| *object != surf_object as usize);
        }
    }
    if let Ok(mut descs) = SURFACE_DESCS.lock() {
        if let Some(descs) = descs.as_mut() {
            descs.remove(&(surf_object as usize));
        }
    }
    hipDestroySurfaceObject(surf_object)
}
