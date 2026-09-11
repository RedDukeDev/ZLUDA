use hip_runtime_sys::*;
use std::collections::HashMap;
use std::mem;
use std::ptr;
use std::sync::Mutex;

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
// So a surface object is given one: the sampler of the texture object over the
// same array, or a plain one if no texture object exists yet. Both directions
// are covered, because the program may create the surface first or the texture
// first and neither order can be assumed -- a surface created before its
// texture is revisited when the texture appears, and a surface created after it
// takes the texture's sampler immediately. Without that, whether a surface ends
// up with the sampling the program asked for or with a point-sampling default
// would depend on the order the program happened to call in.
const SAMPLER_OFFSET: usize = 48; // twelve dwords of image descriptor
const SAMPLER_BYTES: usize = 16;
const PAGE_BYTES: usize = SAMPLER_OFFSET + SAMPLER_BYTES;

/// How much of an object's page the device actually reads as the image
/// descriptor.
///
/// `get_image_and_sampler` casts the object address to `v8s32*`, which is 32
/// bytes; the sampler begins at byte 48. Bytes 32..47 are read by nothing on
/// this path.
const IMAGE_DESCRIPTOR_BYTES: usize = 32;

/// Everything this module remembers about the objects the program has made.
///
/// One lock for all of it, so that no path ever holds two of them and there is no
/// lock ordering to get wrong. The maps are keyed by device handles and the
/// entries are small.
#[derive(Default)]
struct Registry {
    /// Surface object -> the descriptor it was created with, for the
    /// driver-style query HIP does not answer.
    surface_descs: HashMap<usize, HIP_RESOURCE_DESC>,
    /// Resource handle -> surface objects over it.
    surfaces: Vec<(usize, usize)>,
    /// Resource handle -> texture objects over it. Texture objects carry their
    /// own descriptors in `tex.rs`; what is kept here is only the association
    /// that lets a surface object find a sampler.
    textures: Vec<(usize, usize)>,
}

// SAFETY: the pointers inside these descriptors are opaque device handles handed
// out by the driver (an `hipArray*` is a handle, not host memory this process
// owns), and they are already passed between threads as plain `usize` in the
// lists above. Every access to the registry is serialised by the mutex, and the
// handles stay valid until the object they name is destroyed, at which point the
// entry is removed.
unsafe impl Send for Registry {}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(body: impl FnOnce(&mut Registry) -> T) -> Option<T> {
    // A poisoned lock is recovered rather than propagated: the registry is a
    // plain collection of handles with no invariant that a panic can break, and
    // refusing to work after one would turn an unrelated panic into a permanently
    // broken surface path.
    let mut guard = match REGISTRY.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    Some(body(guard.get_or_insert_with(Registry::default)))
}

unsafe fn read_object_page(object: usize, into: &mut [u8; PAGE_BYTES]) -> bool {
    hipMemcpyDtoH(
        into.as_mut_ptr().cast(),
        hipDeviceptr_t(object as *mut _),
        into.len(),
    ) == hipError_t::Success
}

/// Whether two objects over the same array describe the same image.
///
/// Only the bytes the device reads are compared, and byte 14 is compared with its
/// low nibble masked off: that nibble holds the resource access permission on
/// RDNA (a texture object is read-only 0xb0, a surface object read-write 0xbf),
/// while the high nibble holds the format and resource type that must match.
///
/// The comparison deliberately stops at byte 32. Matching bytes 32..47 as well
/// would be asserting something the fetch path never reads and this code cannot
/// justify, and the cost of being wrong is not a failed copy but no copy at all:
/// the surface keeps HIP's own bytes, which is precisely the non-determinism
/// being fixed here, announced by one line on stderr that is easy to miss.
fn describes_same_image(a: &[u8; PAGE_BYTES], b: &[u8; PAGE_BYTES]) -> bool {
    a[..14] == b[..14]
        && (a[14] & 0xf0) == (b[14] & 0xf0)
        && a[15..IMAGE_DESCRIPTOR_BYTES] == b[15..IMAGE_DESCRIPTOR_BYTES]
}

/// Copies `texture`'s sampler into every surface object over the same array.
///
/// The whole operation runs under the registry lock. The alternative -- snapshot
/// the surface list, release the lock, then write -- leaves a window in which
/// `object_destroy` can free the device page being written, and the write's
/// failure is indistinguishable from success. The work under the lock is two
/// 64-byte reads and one 16-byte write per surface.
fn adopt_sampler_locked(registry: &Registry, array: usize, texture: usize) {
    if array == 0 || texture == 0 {
        return;
    }
    let surfaces: Vec<usize> = registry
        .surfaces
        .iter()
        .filter(|(a, _)| *a == array)
        .map(|(_, object)| *object)
        .collect();
    if surfaces.is_empty() {
        // Not an error: the texture may simply have been created before any
        // surface over its array, and `object_create` revisits this from the
        // other side when one appears. Silent because that is the normal case.
        return;
    }
    let mut from = [0u8; PAGE_BYTES];
    unsafe {
        if !read_object_page(texture, &mut from) {
            eprintln!(
                "[zluda] could not read the sampler out of texture object {:#x}, so the \
                 surface objects over array {:#x} keep whatever HIP left at that offset",
                texture, array
            );
            return;
        }
    }
    for object in surfaces {
        let mut onto = [0u8; PAGE_BYTES];
        unsafe {
            if !read_object_page(object, &mut onto) {
                eprintln!(
                    "[zluda] could not read the page of surface object {:#x}, so it keeps \
                     whatever HIP left at the sampler offset",
                    object
                );
                continue;
            }
        }
        if !describes_same_image(&onto, &from) {
            // Reported every time rather than once: "it worked for the first
            // surface and not for the rest" is a state this has to be able to
            // express, and a one-shot message cannot.
            eprintln!(
                "[zluda] texture object {:#x} and surface object {:#x} are over array {:#x} \
                 but do not agree on their image descriptor, so the surface cannot be given a \
                 sampler. Texture: {:02x?}. Surface: {:02x?}.",
                texture,
                object,
                array,
                &from[..IMAGE_DESCRIPTOR_BYTES],
                &onto[..IMAGE_DESCRIPTOR_BYTES],
            );
            continue;
        }
        let result = unsafe {
            hipMemcpyHtoD(
                hipDeviceptr_t((object + SAMPLER_OFFSET) as *mut _),
                from[SAMPLER_OFFSET..].as_ptr() as *mut _,
                SAMPLER_BYTES,
            )
        };
        // Reported rather than discarded: a write that did not land leaves the
        // surface sampling with whatever HIP put at that offset, which is the
        // non-determinism this exists to remove, and it is otherwise invisible.
        if let hipError_t::Err(code) = result {
            eprintln!(
                "[zluda] could not write a sampler into surface object {:#x} ({:?}), so it keeps \
                 whatever HIP left at that offset",
                object, code
            );
        }
    }
}

/// Puts `texture`'s sampler onto every surface object over the same array.
pub(crate) unsafe fn adopt_sampler(array: usize, texture: usize) {
    with_registry(|registry| adopt_sampler_locked(registry, array, texture));
}

/// Records a texture object so that surfaces created later can take its sampler.
pub(crate) fn register_texture(array: usize, texture: usize) {
    if array == 0 || texture == 0 {
        return;
    }
    with_registry(|registry| registry.textures.push((array, texture)));
}

pub(crate) fn unregister_texture(texture: usize) {
    with_registry(|registry| {
        registry.textures.retain(|(_, object)| *object != texture);
    });
}

/// A sampler built by HIP itself rather than by spelling out its bits here: a
/// throwaway texture object over the same resource is made, its sampler taken,
/// and the object released.
///
/// This is only the fallback for a surface whose program has not made a texture
/// object yet, and it is a point-sampling, clamp-to-edge sampler: defined, but
/// not necessarily the sampling the program asked for. It is replaced by the real
/// one as soon as a texture object over the same array exists -- see
/// `register_texture` and `object_create`.
unsafe fn give_plain_sampler(registry: &Registry, res_desc: &HIP_RESOURCE_DESC, array: usize) {
    let mut tex_desc: HIP_TEXTURE_DESC = mem::zeroed();
    tex_desc.addressMode = [HIPaddress_mode::HIP_TR_ADDRESS_MODE_CLAMP; 3];
    tex_desc.filterMode = HIPfilter_mode::HIP_TR_FILTER_MODE_POINT;
    tex_desc.mipmapFilterMode = HIPfilter_mode::HIP_TR_FILTER_MODE_POINT;
    let mut texture: hipTextureObject_t = ptr::null_mut();
    if hipTexObjectCreate(&mut texture, res_desc, &tex_desc, ptr::null()) != hipError_t::Success {
        eprintln!(
            "[zluda] could not build a fallback sampler for the surface over array {:#x}, so it \
             keeps whatever HIP left at the sampler offset",
            array
        );
        return;
    }
    adopt_sampler_locked(registry, array, texture as usize);
    // Copying the 16 sampler bytes by value, above, is what makes destroying the
    // object they came from legal. If that ever becomes a pointer, this is a
    // use-after-free.
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
    // The full descriptor is kept, not just its type and handle: the
    // driver-style query has to give back what CUDA promises, including the
    // linear and pitched fields that `mem::zeroed` would otherwise leave null.
    with_registry(|registry| {
        registry
            .surface_descs
            .insert(*p_surf_object as usize, *driver_desc);
        registry.surfaces.push((handle, *p_surf_object as usize));
        // If the program made the texture object first -- and it may -- the
        // surface takes that sampler now instead of being left with the plain
        // one forever.
        match registry
            .textures
            .iter()
            .find(|(a, _)| *a == handle)
            .map(|(_, texture)| *texture)
        {
            Some(texture) => adopt_sampler_locked(registry, handle, texture),
            None => give_plain_sampler(registry, driver_desc, handle),
        }
    });
    Ok(())
}

pub(crate) unsafe fn object_get_resource_desc(
    p_res_desc: *mut HIP_RESOURCE_DESC,
    surf_object: hipSurfaceObject_t,
) -> hipError_t {
    let stored = with_registry(|registry| registry.surface_descs.get(&(surf_object as usize)).copied())
        .flatten()
        .ok_or(hipErrorCode_t::InvalidValue)?;
    let desc = p_res_desc.as_mut().ok_or(hipErrorCode_t::InvalidValue)?;
    *desc = stored;
    Ok(())
}

pub(crate) unsafe fn object_destroy(surf_object: hipSurfaceObject_t) -> hipError_t {
    // Removed from the registry before the object is destroyed, so that an
    // adoption running concurrently cannot find and write to a page that is about
    // to be freed.
    with_registry(|registry| {
        registry.surfaces.retain(|(_, object)| *object != surf_object as usize);
        registry.surface_descs.remove(&(surf_object as usize));
    });
    hipDestroySurfaceObject(surf_object)
}
