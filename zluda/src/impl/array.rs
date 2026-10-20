use hip_runtime_sys::*;

// CUDA formats HIP has no name for. 0x50 is CU_AD_FORMAT_UNORM_INT_101010_2 (R10G10B10A2, four
// bytes per texel), which the ray reconstruction network keeps its downsampled history in. It
// becomes four 16 bit unsigned channels: twice the bytes, but a read gives the same normalised
// float and a write rounds to a finer step than ten bits did, so no value the original could
// hold is lost.
fn hip_format(format: hipArray_Format) -> hipArray_Format {
    match format.0 {
        0x50 => hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT16,
        // CU_AD_FORMAT_UNORM_INT8X1..X4, UNORM_INT16X1..X4, SNORM_INT8X1..X4, SNORM_INT16X1..X4:
        // the same integers HIP has, with the normalisation left to the texture that reads them.
        // The channel count rides in NumChannels as usual.
        0xc0..=0xc2 => hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT8,
        0xc3..=0xc5 => hipArray_Format::HIP_AD_FORMAT_UNSIGNED_INT16,
        0xc6..=0xc8 => hipArray_Format::HIP_AD_FORMAT_SIGNED_INT8,
        0xc9..=0xcb => hipArray_Format::HIP_AD_FORMAT_SIGNED_INT16,
        _ => format,
    }
}

// A new array holds whatever the memory held before. NVIDIA's driver hands out zeroed pages (the
// operating system scrubs them), and what DLSS keeps in its arrays is used as if it were: with
// the leftovers of another process in them, the same input gave a picture at full brightness in one
// run and at half in the next, and the frames after the first did not make it better. cuMemAlloc
// is zeroed already (memory.rs); the arrays are now too. ZLUDA_UNINITIALIZED_ARRAYS=1 skips it.
unsafe fn zero_fill_2d(array: hipArray_t, width: usize, height: usize, format: hipArray_Format, channels: u32) {
    if std::env::var_os("ZLUDA_UNINITIALIZED_ARRAYS").is_some() || array.is_null() {
        return;
    }
    let per_channel = match format.0 {
        0x01 | 0x08 => 1,
        0x02 | 0x09 | 0x10 => 2,
        _ => 4,
    };
    let row = width * per_channel * channels.max(1) as usize;
    let rows = height.max(1);
    let zeros = vec![0u8; row * rows];
    let _ = hipMemcpy2DToArray(
        array,
        0,
        0,
        zeros.as_ptr().cast(),
        row,
        row,
        rows,
        hipMemcpyKind::hipMemcpyHostToDevice,
    );
}

// ZLUDA_DEBUG_ARRAYS=1: the mean of the first channel of every array the program made, after each
// synchronisation, to see which of them a kernel has written.
static DEBUG_ARRAYS: std::sync::Mutex<Vec<(usize, usize, usize, u32, u32)>> = std::sync::Mutex::new(Vec::new());

pub(crate) unsafe fn debug_report() {
    if std::env::var_os("ZLUDA_DEBUG_ARRAYS").is_none() {
        return;
    }
    let arrays = match DEBUG_ARRAYS.lock() {
        Ok(list) => list.clone(),
        Err(_) => return,
    };
    let mut line = String::from("[zluda arrays]");
    for (handle, width, height, format, channels) in arrays {
        if width != 1 && (width < 480 || height < 4) {
            continue;
        }
        let texel = match format {
            0x10 => 2,
            0x20 => 4,
            _ => continue,
        } * channels as usize;
        let row = width * texel;
        let mut data = vec![0u8; row * height];
        if hipMemcpy2DFromArray(
            data.as_mut_ptr().cast(),
            row,
            handle as hipArray_const_t,
            0,
            0,
            row,
            height,
            hipMemcpyKind::hipMemcpyDeviceToHost,
        ) != hipError_t::Success
        {
            continue;
        }
        let mut sum = 0f64;
        let mut count = 0f64;
        for y in (0..height).step_by(8) {
            for x in (0..width).step_by(8) {
                let at = y * row + x * texel;
                let v = if format == 0x10 {
                    let h = u16::from_le_bytes([data[at], data[at + 1]]);
                    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
                    let exp = ((h >> 10) & 31) as i32;
                    let mant = (h & 1023) as f64;
                    if exp == 0 {
                        sign * mant / 1024.0 * 2f64.powi(-14)
                    } else if exp == 31 {
                        0.0
                    } else {
                        sign * (1.0 + mant / 1024.0) * 2f64.powi(exp - 15)
                    }
                } else {
                    f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as f64
                };
                sum += v;
                count += 1.0;
            }
        }
        line.push_str(&format!(" {}x{}x{}:{:.5}", width, height, channels, sum / count));
    }
    eprintln!("{}", line);
}

pub(crate) unsafe fn create_v2(
    handle: *mut hipArray_t,
    desc: *const HIP_ARRAY_DESCRIPTOR,
) -> hipError_t {
    // Through the 3D entry point with a depth of 0, which is the same 2D array: ROCm's own
    // hipArrayCreate refuses four channel float formats ("Invalid Image format for channel
    // Order:4277 Type:0") that hipArray3DCreate accepts.
    let d = &*desc;
    let desc3d = HIP_ARRAY3D_DESCRIPTOR {
        Width: d.Width,
        Height: d.Height,
        Depth: 0,
        Format: hip_format(d.Format),
        NumChannels: d.NumChannels,
        Flags: 0,
    };
    let result = hipArray3DCreate(handle, &desc3d);
    if result.is_ok() {
        zero_fill_2d(*handle, d.Width, d.Height, desc3d.Format, d.NumChannels);
    }
    if result.is_ok() && std::env::var_os("ZLUDA_DEBUG_ARRAYS").is_some() {
        if let Ok(mut list) = DEBUG_ARRAYS.lock() {
            list.push((*handle as usize, d.Width, d.Height, desc3d.Format.0, d.NumChannels));
        }
    }
    if result.is_ok() && std::env::var_os("ZLUDA_DEBUG_TEX").is_some() {
        eprintln!(
            "[zluda tex] array {:?} created: {}x{} format {:#x} channels {}",
            *handle, d.Width, d.Height, d.Format.0, d.NumChannels
        );
    }
    if result.is_err() && std::env::var_os("ZLUDA_DEBUG_COMPILE").is_some() {
        eprintln!(
            "[zluda] hipArrayCreate refused: {}x{} format {:#x} channels {} -> {:?}",
            d.Width, d.Height, d.Format.0, d.NumChannels, result
        );
    }
    result
}

pub(crate) unsafe fn destroy(array: hipArray_t) -> hipError_t {
    hipArrayDestroy(array)
}

pub(crate) unsafe fn _3d_create_v2(
    handle: *mut hipArray_t,
    desc: *const HIP_ARRAY3D_DESCRIPTOR,
) -> hipError_t {
    let mut mapped = *desc;
    mapped.Format = hip_format(mapped.Format);
    let result = hipArray3DCreate(handle, &mapped);
    if result.is_ok() && mapped.Depth == 0 {
        zero_fill_2d(*handle, mapped.Width, mapped.Height, mapped.Format, mapped.NumChannels);
    }
    // A refused array is otherwise reported only as the caller's own generic error.
    if result.is_err() && std::env::var_os("ZLUDA_DEBUG_COMPILE").is_some() {
        let d = &*desc;
        eprintln!(
            "[zluda] hipArray3DCreate refused: {}x{}x{} format {:#x} channels {} flags {:#x} -> {:?}",
            d.Width, d.Height, d.Depth, d.Format.0, d.NumChannels, d.Flags, result
        );
    }
    result
}

pub(crate) unsafe fn get_descriptor_v2(
    p_array_descriptor: *mut HIP_ARRAY_DESCRIPTOR,
    array: hipArray_t,
) -> hipError_t {
    let result = hipArrayGetDescriptor(p_array_descriptor, array);
    if std::env::var_os("ZLUDA_DEBUG_TEX").is_some() {
        match p_array_descriptor.as_ref() {
            Some(d) => eprintln!(
                "[zluda tex] cuArrayGetDescriptor({:?}) -> {:?}: {}x{} format {:#x} channels {}",
                array, result, d.Width, d.Height, d.Format.0, d.NumChannels
            ),
            None => eprintln!("[zluda tex] cuArrayGetDescriptor({:?}) -> {:?}", array, result),
        }
    }
    result
}
