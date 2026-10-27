use crate::r#impl::{context, hipfix};
use cuda_types::cuda::{CUerror, CUsurfObject, CUsurfref};
use hip_runtime_sys::*;
use std::collections::HashMap;
use std::mem::{self, offset_of};
use std::ptr;
use std::sync::Mutex;
use zluda_common::FromCuda;

#[repr(C)]
pub(crate) struct SurfRefData {
    pub(crate) base: *mut textureReference,
    pub(crate) devptr: hipDeviceptr_t,
}

unsafe impl Send for SurfRefData {}
unsafe impl Sync for SurfRefData {}

impl<'a, E: zluda_common::CudaErrorType> zluda_common::FromCuda<'a, CUsurfref, E>
    for &'a SurfRefData
{
    fn from_cuda(surf_ref: &'a CUsurfref) -> Result<Self, E> {
        if surf_ref.is_null() {
            return Err(E::INVALID_VALUE);
        }
        Ok(unsafe { mem::transmute(*surf_ref) })
    }
}

impl<'a, E: zluda_common::CudaErrorType> zluda_common::FromCuda<'a, *mut CUsurfref, E>
    for &'a mut &'a SurfRefData
{
    fn from_cuda(surf_ref: &'a *mut CUsurfref) -> Result<Self, E> {
        let surf_ref = unsafe { surf_ref.as_mut() }.ok_or_else(|| E::INVALID_VALUE)?;
        Ok(unsafe { mem::transmute(surf_ref) })
    }
}

fn convert_resource_desc(desc: &HIP_RESOURCE_DESC) -> Result<hipResourceDesc, hipErrorCode_t> {
    let (type_, desc) = match desc.resType {
        hipResourcetype::HIP_RESOURCE_TYPE_ARRAY => (
            hipResourceType::hipResourceTypeArray,
            hipResourceDesc__bindgen_ty_1 {
                array: hipResourceDesc__bindgen_ty_1__bindgen_ty_1 {
                    array: unsafe { desc.res.array.hArray },
                },
            },
        ),
        hipResourcetype::HIP_RESOURCE_TYPE_MIPMAPPED_ARRAY => (
            hipResourceType::hipResourceTypeMipmappedArray,
            hipResourceDesc__bindgen_ty_1 {
                mipmap: hipResourceDesc__bindgen_ty_1__bindgen_ty_2 {
                    mipmap: unsafe { desc.res.mipmap.hMipmappedArray },
                },
            },
        ),
        hipResourcetype::HIP_RESOURCE_TYPE_LINEAR => {
            let linear = unsafe { desc.res.linear };
            let format_kind = format_kind(linear.format)?;
            let channel_bits = channel_bits(linear.format)?;
            let desc = hipChannelFormatDesc {
                x: channel_bits,
                y: if linear.numChannels > 1 {
                    channel_bits
                } else {
                    0
                },
                z: if linear.numChannels > 2 {
                    channel_bits
                } else {
                    0
                },
                w: if linear.numChannels > 3 {
                    channel_bits
                } else {
                    0
                },
                f: format_kind,
            };
            (
                hipResourceType::hipResourceTypeLinear,
                hipResourceDesc__bindgen_ty_1 {
                    linear: hipResourceDesc__bindgen_ty_1__bindgen_ty_3 {
                        devPtr: linear.devPtr.0,
                        desc,
                        sizeInBytes: linear.sizeInBytes,
                    },
                },
            )
        }
        hipResourcetype::HIP_RESOURCE_TYPE_PITCH2D => {
            let pitch2d = unsafe { desc.res.pitch2D };
            let format_kind = format_kind(pitch2d.format)?;
            let channel_bits = channel_bits(pitch2d.format)?;
            let desc = hipChannelFormatDesc {
                x: channel_bits,
                y: if pitch2d.numChannels > 1 {
                    channel_bits
                } else {
                    0
                },
                z: if pitch2d.numChannels > 2 {
                    channel_bits
                } else {
                    0
                },
                w: if pitch2d.numChannels > 3 {
                    channel_bits
                } else {
                    0
                },
                f: format_kind,
            };
            (
                hipResourceType::hipResourceTypePitch2D,
                hipResourceDesc__bindgen_ty_1 {
                    pitch2D: hipResourceDesc__bindgen_ty_1__bindgen_ty_4 {
                        devPtr: pitch2d.devPtr.0,
                        desc,
                        width: pitch2d.width,
                        height: pitch2d.height,
                        pitchInBytes: pitch2d.pitchInBytes,
                    },
                },
            )
        }
        _ => return Err(hipErrorCode_t::InvalidValue),
    };

    Ok(hipResourceDesc {
        resType: type_,
        res: desc,
    })
}

fn format_kind(format: hipArray_Format) -> Result<hipChannelFormatKind, hipErrorCode_t> {
    Ok(match format {
        hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT8
        | hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT16
        | hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT32 => {
            hipChannelFormatKind::hipChannelFormatKindUnsigned
        }
        hipArray_Format::HIP_AD_FORMAT_SIGNED_INT8
        | hipArray_Format::HIP_AD_FORMAT_SIGNED_INT16
        | hipArray_Format::HIP_AD_FORMAT_SIGNED_INT32 => {
            hipChannelFormatKind::hipChannelFormatKindSigned
        }
        hipArray_Format::HIP_AD_FORMAT_HALF | hipArray_Format::HIP_AD_FORMAT_FLOAT => {
            hipChannelFormatKind::hipChannelFormatKindFloat
        }
        _ => return Err(hipErrorCode_t::InvalidValue),
    })
}

fn channel_bits(format: hipArray_Format) -> Result<i32, hipErrorCode_t> {
    Ok(match format {
        hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT8
        | hipArray_Format::HIP_AD_FORMAT_SIGNED_INT8 => 8,
        hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT16
        | hipArray_Format::HIP_AD_FORMAT_SIGNED_INT16
        | hipArray_Format::HIP_AD_FORMAT_HALF => 16,
        hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT32
        | hipArray_Format::HIP_AD_FORMAT_SIGNED_INT32
        | hipArray_Format::HIP_AD_FORMAT_FLOAT => 32,
        _ => return Err(hipErrorCode_t::InvalidValue),
    })
}

fn num_channels_for_array(array: hipArray_t) -> Result<(hipArray_Format, u32), hipErrorCode_t> {
    unsafe {
        let mut desc = std::mem::zeroed();
        hipArrayGetDescriptor(&mut desc, array)?;
        Ok((desc.Format, desc.NumChannels))
    }
}

fn num_channels_for_mipmapped_array(
    array: hipMipmappedArray_t,
) -> Result<(hipArray_Format, u32), hipErrorCode_t> {
    unsafe {
        let mut level_array = std::ptr::null_mut();
        hipGetMipmappedArrayLevel(&mut level_array, array, 0)?;
        num_channels_for_array(level_array)
    }
}

fn num_channels_for_resource_desc(
    desc: &HIP_RESOURCE_DESC,
) -> Result<(hipArray_Format, u32), hipErrorCode_t> {
    match desc.resType {
        hipResourcetype::HIP_RESOURCE_TYPE_ARRAY => unsafe {
            num_channels_for_array(desc.res.array.hArray)
        },
        hipResourcetype::HIP_RESOURCE_TYPE_MIPMAPPED_ARRAY => unsafe {
            num_channels_for_mipmapped_array(desc.res.mipmap.hMipmappedArray)
        },
        hipResourcetype::HIP_RESOURCE_TYPE_LINEAR => {
            Ok(unsafe { (desc.res.linear.format, desc.res.linear.numChannels) })
        }
        hipResourcetype::HIP_RESOURCE_TYPE_PITCH2D => {
            Ok(unsafe { (desc.res.pitch2D.format, desc.res.pitch2D.numChannels) })
        }
        _ => Err(hipErrorCode_t::InvalidValue),
    }
}

fn pixel_size_from_format_channels(
    format: hipArray_Format,
    num_channels: u32,
) -> Result<u32, hipErrorCode_t> {
    let bits_per_channel = channel_bits(format)? as u32;
    Ok((bits_per_channel * num_channels) / 8)
}

// The resource type and handle each surface object was made over, by the address HIP gave it (the
// handle a program holds also carries the pixel size in its low bits: see to_cuda).
// HIP has no driver-style query for a surface object's resource descriptor, so it is remembered at
// creation. Only the resource type and its handle are kept: those are the two fields a program asks
// about, and the rest of HIP_RESOURCE_DESC is reserved. A surface over linear or pitched memory
// keeps its type and a null handle.
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
    if std::env::var_os("ZLUDA_NO_ADOPT").is_some() {
        return;
    }
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
        // works some of the time and not others. If a future ROCm moves the
        // sampler, this line is the only warning there will be.
        //
        // Except for what only concerns the mip chain. A surface over a level of a
        // mipmapped array says "all the levels" (LAST_LEVEL and the fields around it,
        // and the last words of the page) where a texture over the same level says
        // "this one". The image is the same and the sampler does not depend on it.
        // Taking that difference for a different layout left the surface of every
        // image DLSS creates over a mipmapped array (the shared textures of a
        // Direct3D host) without a sampler: the network then read one texel of its
        // input everywhere. So what is compared is what identifies the image: its
        // base address, its size and its kind.
        let same_image = |a: &[u8], b: &[u8]| -> bool {
            // per dword of the descriptor: the bits that have to be the same
            const IDENTITY: [u32; 8] = [
                !0,         // base address
                0x0000_00FF, // base address, high bits
                !0,         // width and height
                0xF000_0000, // the kind of image (the rest is channel selection, mip range, tiling)
                !0,
                !0,
                !0,
                !0,
            ];
            IDENTITY.iter().enumerate().all(|(index, mask)| {
                let at = |bytes: &[u8]| {
                    u32::from_le_bytes([
                        bytes[index * 4],
                        bytes[index * 4 + 1],
                        bytes[index * 4 + 2],
                        bytes[index * 4 + 3],
                    ])
                };
                at(a) & mask == at(b) & mask
            })
        };
        if !same_image(&onto[..SAMPLER_OFFSET], &from[..SAMPLER_OFFSET]) {
            static COMPLAINED: std::sync::Once = std::sync::Once::new();
            COMPLAINED.call_once(|| {
                let words = |bytes: &[u8]| -> String {
                    bytes
                        .chunks(4)
                        .map(|w| format!("{:08X}", u32::from_le_bytes([w[0], w[1], w[2], w[3]])))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                eprintln!("[zluda] texture page: {} | surface page: {}", words(&from), words(&onto));
                eprintln!(
                    "[zluda] a texture object and a surface object over the same array do not agree on their image descriptor, so the surface cannot be given a sampler. The offset this assumes for it (byte {}) is probably no longer right for this version of ROCm. A tex through a surface object will sample with whatever happens to sit there. See the note in zluda/src/impl/surf.rs.",
                    SAMPLER_OFFSET
                );
            });
            continue;
        }
        if std::env::var_os("ZLUDA_DEBUG_TEX").is_some() {
            eprintln!(
                "[zluda tex] surface {:#x} over array {:#x} takes the sampler of texture {:#x}: {:08X} {:08X} {:08X} {:08X}",
                object,
                array,
                texture,
                u32::from_le_bytes(from[SAMPLER_OFFSET..SAMPLER_OFFSET + 4].try_into().unwrap()),
                u32::from_le_bytes(from[SAMPLER_OFFSET + 4..SAMPLER_OFFSET + 8].try_into().unwrap()),
                u32::from_le_bytes(from[SAMPLER_OFFSET + 8..SAMPLER_OFFSET + 12].try_into().unwrap()),
                u32::from_le_bytes(from[SAMPLER_OFFSET + 12..SAMPLER_OFFSET + 16].try_into().unwrap()),
            );
        }
        let _ = hipMemcpyHtoD(
            hipDeviceptr_t((object + SAMPLER_OFFSET) as *mut _),
            from[SAMPLER_OFFSET..].as_ptr() as *mut _,
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
    p_tex_object: &mut CUsurfObject,
    p_res_desc: &HIP_RESOURCE_DESC,
) -> hipError_t {
    let resource_desc = convert_resource_desc(p_res_desc)?;
    let (format, num_channels) = num_channels_for_resource_desc(p_res_desc)?;
    let pixel_size = pixel_size_from_format_channels(format, num_channels)?;
    let mut hip_surfobj = std::mem::zeroed();
    hipCreateSurfaceObject(&mut hip_surfobj, &resource_desc)?;
    *p_tex_object = to_cuda(hip_surfobj, pixel_size)?;
    let object = hip_surfobj as usize;
    let handle = match p_res_desc.resType {
        HIPresourcetype::HIP_RESOURCE_TYPE_ARRAY => p_res_desc.res.array.hArray as usize,
        HIPresourcetype::HIP_RESOURCE_TYPE_MIPMAPPED_ARRAY => {
            p_res_desc.res.mipmap.hMipmappedArray as usize
        }
        _ => 0,
    };
    SURFACE_DESCS
        .lock()
        .map_err(|_| hipErrorCode_t::OperatingSystem)?
        .get_or_insert_with(HashMap::new)
        .insert(object, (p_res_desc.resType.0, handle));
    // Only an array has a texture object that could share its sampler.
    if handle != 0 {
        if let Ok(mut list) = SURFACES.lock() {
            list.get_or_insert_with(Vec::new).push((handle, object));
        }
        if std::env::var_os("ZLUDA_DEBUG_TEX").is_some() {
            eprintln!(
                "[zluda tex] surface {:#x} (handle {:#x}) created over array {:#x}",
                object, *p_tex_object, handle
            );
        }
        give_plain_sampler(p_res_desc, handle);
        // A texture object over the array that was made before this surface has already come and
        // gone through adopt_sampler, which only reaches the surfaces that existed then: this one
        // would keep the plain sampler above, where one that came first gets the program's. Which of
        // the two a program makes first differs from image to image of the same network, and with
        // it the picture (half the brightness where the history is read through a surface whose
        // sampler is the plain one).
        if std::env::var_os("ZLUDA_DEBUG_TEX").is_some() {
            let mut page = [0u8; 64];
            if read_object_page(object, &mut page) {
                let words: Vec<String> = page
                    .chunks(4)
                    .map(|w| format!("{:08X}", u32::from_le_bytes([w[0], w[1], w[2], w[3]])))
                    .collect();
                eprintln!("[zluda tex] surface page: {}", words.join(" "));
            }
        }
        if let Some(texture) = super::tex::last_texture_over(handle) {
            adopt_sampler(handle, texture);
        }
    }
    Ok(())
}

pub(crate) unsafe fn object_get_resource_desc(
    p_res_desc: *mut HIP_RESOURCE_DESC,
    surf_object: CUsurfObject,
) -> hipError_t {
    let (res_type, handle) = *SURFACE_DESCS
        .lock()
        .map_err(|_| hipErrorCode_t::OperatingSystem)?
        .as_ref()
        .and_then(|m| m.get(&(to_hip(surf_object) as usize)))
        .ok_or(hipErrorCode_t::InvalidValue)?;
    let desc = p_res_desc.as_mut().ok_or(hipErrorCode_t::InvalidValue)?;
    *desc = mem::zeroed();
    desc.resType = HIPresourcetype(res_type);
    match desc.resType {
        HIPresourcetype::HIP_RESOURCE_TYPE_ARRAY => desc.res.array.hArray = handle as hipArray_t,
        HIPresourcetype::HIP_RESOURCE_TYPE_MIPMAPPED_ARRAY => {
            desc.res.mipmap.hMipmappedArray = handle as hipMipmappedArray_t
        }
        _ => {}
    }
    Ok(())
}

pub(crate) unsafe fn object_destroy(tex_object: CUsurfObject) -> hipError_t {
    let object = to_hip(tex_object);
    if let Ok(mut list) = SURFACES.lock() {
        if let Some(list) = list.as_mut() {
            list.retain(|(_, o)| *o != object as usize);
        }
    }
    if let Ok(mut descs) = SURFACE_DESCS.lock() {
        if let Some(descs) = descs.as_mut() {
            descs.remove(&(object as usize));
        }
    }
    hipDestroySurfaceObject(object)
}

const RESERVED_SURFACE_BOTTOM_BITS: u32 = 3;

fn to_cuda(hip: hipSurfaceObject_t, pixel_size: u32) -> Result<CUsurfObject, hipErrorCode_t> {
    let hip = hip as u64;
    let bottom_bits = extract_bottom_bits::<RESERVED_SURFACE_BOTTOM_BITS>(hip);
    if bottom_bits != 0 {
        return Err(hipErrorCode_t::Unknown);
    }
    let shift_size = pixel_size.ilog2() as u64;
    Ok(set_bottom_bits::<RESERVED_SURFACE_BOTTOM_BITS>(
        hip, shift_size,
    ))
}

fn to_hip(cuda: CUsurfObject) -> hipSurfaceObject_t {
    ((cuda >> RESERVED_SURFACE_BOTTOM_BITS) << RESERVED_SURFACE_BOTTOM_BITS) as _
}

fn extract_bottom_bits<const N: u32>(x: u64) -> u64 {
    x & ((1 << N) - 1)
}

fn set_bottom_bits<const N: u32>(x: u64, bits: u64) -> u64 {
    x | bits
}

pub(crate) unsafe fn ref_set_array(
    surf_ref: &SurfRefData,
    array: hipArray_t,
    mut flags: ::core::ffi::c_uint,
) -> Result<(), CUerror> {
    flags |= hipfix::HIP_TRSA_OVERRIDE_FORMAT;
    super::tex::ref_set_array(surf_ref.base, array, flags)?;
    if let Some(texref) = surf_ref.base.as_ref() {
        if !texref.textureObject.is_null() {
            let pixel_size =
                pixel_size_from_format_channels(texref.format, texref.numChannels as u32)?;
            let mut cuda_texobj = to_cuda(texref.textureObject as _, pixel_size)?;
            let cu_context = context::get_current_context()?;
            let context: &context::Context = FromCuda::<_, CUerror>::from_cuda(&cu_context)?;
            hipMemcpyHtoDAsync(
                hipDeviceptr_t(
                    surf_ref
                        .devptr
                        .0
                        .add(offset_of!(textureReference, textureObject)),
                ),
                std::ptr::from_mut(&mut cuda_texobj).cast(),
                std::mem::size_of_val(&cuda_texobj),
                context.alloc_stream,
            )?;
            hipStreamSynchronize(context.alloc_stream)?;
        }
    }
    Ok(())
}
