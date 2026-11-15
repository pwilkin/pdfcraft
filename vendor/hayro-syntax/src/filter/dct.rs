use crate::object::Dict;
use crate::object::dict::keys::COLOR_TRANSFORM;
use crate::object::stream::{FilterResult, ImageColorSpace, ImageData, ImageDecodeParams};
use alloc::borrow::Cow;
use core::num::NonZeroU32;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::colorspace::ColorSpace::CMYK;
use zune_jpeg::zune_core::options::DecoderOptions;

pub(crate) fn decode(
    data: &[u8],
    params: &Dict<'_>,
    image_params: &ImageDecodeParams,
) -> Option<FilterResult<'static>> {
    if image_params.width > u16::MAX as u32 || image_params.height > u16::MAX as u32 {
        return None;
    }

    // Some PDFs have weird JPEGs where the JPEG metadata is completely wrong
    // (for example indicating that one of the dimensions is u16::MAX), but the
    // metadata in the PDF image dictionary is correct. Therefore, we first
    // validate the JPEG metadata and patch the data if any of the dimensions
    // are too large (if they are too small, they will just be padded later on).
    let data = maybe_patch_jpeg_dimensions(data, image_params)?;

    let options = DecoderOptions::default()
        .set_max_width(u16::MAX as usize)
        .set_max_height(u16::MAX as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&*data), options);
    decoder.decode_headers().ok()?;
    let (jpeg_width, jpeg_height) = decoder.dimensions()?;
    if !decoded_size_ok(jpeg_width, jpeg_height, image_params) {
        return None;
    }

    let color_transform = params.get::<u8>(COLOR_TRANSFORM);
    let input_color_space = decoder.input_colorspace().unwrap();

    let mut out_colorspace = if let Some(num_components) = image_params.num_components
        && !matches!(num_components, 1 | 3 | 4)
    {
        ColorSpace::MultiBand(NonZeroU32::new(num_components as u32)?)
    } else {
        match input_color_space {
            ColorSpace::YCbCr => {
                if color_transform.is_none_or(|c| c == 1) {
                    ColorSpace::RGB
                } else {
                    ColorSpace::YCbCr
                }
            }
            ColorSpace::RGB | ColorSpace::RGBA => ColorSpace::RGB,
            ColorSpace::Luma | ColorSpace::LumaA => ColorSpace::Luma,
            // TODO: Find test case with color transform on cmyk
            CMYK => CMYK,
            ColorSpace::YCCK => ColorSpace::YCCK,
            _ => ColorSpace::RGB,
        }
    };

    // In case image had APP14 marker, we might have to override the colorspace.
    if input_color_space == CMYK && decoder.info().unwrap().components == 3 {
        out_colorspace = ColorSpace::RGB;
    }

    decoder.set_options(DecoderOptions::default().jpeg_set_out_colorspace(out_colorspace));
    let mut decoded = decoder.decode().ok()?;

    if out_colorspace == ColorSpace::YCCK {
        // See <https://github.com/mozilla/pdf.js/blob/69595a29192b7704733404a42a2ebb537601117b/src/core/jpg.js#L1331>
        for c in decoded.chunks_mut(4) {
            let y = c[0] as f32;
            let cb = c[1] as f32;
            let cr = c[2] as f32;
            c[0] = (434.456 - y - 1.402 * cr) as u8;
            c[1] = (119.541 - y + 0.344 * cb + 0.714 * cr) as u8;
            c[2] = (481.816 - y - 1.772 * cb) as u8;
        }
    }

    let width = decoder.dimensions().unwrap().0 as u32;
    let height = decoder.dimensions().unwrap().1 as u32;

    let image_data = ImageData {
        alpha: None,
        color_space: match out_colorspace {
            ColorSpace::RGB | ColorSpace::YCbCr => Some(ImageColorSpace::Rgb),
            ColorSpace::Luma => Some(ImageColorSpace::Gray),
            ColorSpace::YCCK | CMYK => Some(ImageColorSpace::Cmyk),
            ColorSpace::MultiBand(_) => None,
            _ => None,
        },
        bits_per_component: 8,
        width,
        height,
    };

    Some(FilterResult {
        data: Cow::Owned(decoded),
        image_data: Some(image_data),
    })
}

fn maybe_patch_jpeg_dimensions<'a>(
    data: &'a [u8],
    image_params: &ImageDecodeParams,
) -> Option<Cow<'a, [u8]>> {
    let sof_offset = find_sof_marker(data)?;

    let height_offset = sof_offset.checked_add(5)?;
    let width_offset = sof_offset.checked_add(7)?;

    let jpeg_height = u16::from_be_bytes([
        *data.get(height_offset)?,
        *data.get(height_offset.checked_add(1)?)?,
    ]);
    let jpeg_width = u16::from_be_bytes([
        *data.get(width_offset)?,
        *data.get(width_offset.checked_add(1)?)?,
    ]);

    let jpeg_area = (jpeg_width as usize).checked_mul(jpeg_height as usize)?;
    let image_area = (image_params.width as usize).checked_mul(image_params.height as usize)?;
    let need_patch = jpeg_area > image_area;

    if !need_patch {
        return Some(Cow::Borrowed(data));
    }

    let target_w = (image_params.width as u16).to_be_bytes();
    let target_h = (image_params.height as u16).to_be_bytes();

    let mut patched = data.to_vec();
    patched[height_offset..height_offset.checked_add(2)?].copy_from_slice(&target_h);
    patched[width_offset..width_offset.checked_add(2)?].copy_from_slice(&target_w);

    Some(Cow::Owned(patched))
}

fn find_sof_marker(data: &[u8]) -> Option<usize> {
    let mut i = 0_usize;

    while i.checked_add(1).is_some_and(|next| next < data.len()) {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }

        let marker = data[i + 1];

        // Note: Not sure if 100% correct/robust, is AI-generated.
        match marker {
            // All SOF markers carry dimensions: SOF0–SOF15, excluding
            // 0xC4 (DHT), 0xC8 (JPG), 0xCC (DAC) which are not frame markers.
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                return Some(i);
            }
            // Skip padding bytes (0xFF followed by 0xFF).
            0xFF => {
                i += 1;

                continue;
            }
            // SOI (0xD8), EOI (0xD9), TEM (0x01) and stuffed byte (0x00)
            // are standalone markers with no payload.
            0xD8 | 0xD9 | 0x01 | 0x00 => {
                i += 2;

                continue;
            }
            // All other markers have a 2-byte length field — skip over them.
            _ => {
                let len_start = i.checked_add(2)?;
                let len_end = i.checked_add(3)?;
                let seg_len =
                    u16::from_be_bytes([*data.get(len_start)?, *data.get(len_end)?]) as usize;

                i = i.checked_add(2)?.checked_add(seg_len)?;
            }
        }
    }

    None
}

/// PdfCraft patch (48): the most pixels a JPEG image may decode to, as `image_size_ok` allows.
const MAX_DECODED_PIXELS: u64 = 1 << 28;

/// PdfCraft patch (48): whether the frame the decoder read, `width` × `height`, may be decoded:
/// no larger than the image dictionary's size (which [`maybe_patch_jpeg_dimensions`] makes it),
/// and at most [`MAX_DECODED_PIXELS`]. That patch finds the frame header with its own scanner,
/// which may read the markers differently from the decoder: it takes `FF 01` (TEM) as standalone
/// while zune-jpeg skips a length after it, so a 1 × 1 frame header inside that segment was
/// checked while the decoder read a 65535 × 65535 one after it, and aborted on a 12.9 GB
/// allocation. Checking the decoder's own header holds whichever way the markers are read.
fn decoded_size_ok(width: usize, height: usize, image_params: &ImageDecodeParams) -> bool {
    let (Ok(width), Ok(height)) = (u64::try_from(width), u64::try_from(height)) else {
        return false;
    };
    let pixels = width.saturating_mul(height);
    let image_pixels = u64::from(image_params.width).saturating_mul(u64::from(image_params.height));
    width > 0 && height > 0 && pixels <= image_pixels && pixels <= MAX_DECODED_PIXELS
}

#[cfg(test)]
mod tests {
    use super::{MAX_DECODED_PIXELS, decode, decoded_size_ok};
    use crate::object::Dict;
    use crate::object::stream::ImageDecodeParams;
    use alloc::vec::Vec;

    /// A baseline JPEG of `components` components whose frame is `width` × `height`, written for
    /// these tests: one Huffman code per table (DC difference 0, AC end-of-block) and all-zero
    /// scan data, so every sample is 128 (mid-grey). With `hidden`, a 1 × 1 frame header sits
    /// inside an `FF 01` (TEM) segment that has a length, before the real one.
    fn grey_jpeg(width: u16, height: u16, components: u8, hidden: bool) -> Vec<u8> {
        let mut j = alloc::vec![0xff, 0xd8];
        if hidden {
            j.extend_from_slice(&[0xff, 0x01, 0x00, 0x0b]);
            j.extend_from_slice(&[0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00, 0x01]);
        }
        j.extend_from_slice(&[0xff, 0xc0, 0x00, 8 + 3 * components, 0x08]);
        j.extend_from_slice(&height.to_be_bytes());
        j.extend_from_slice(&width.to_be_bytes());
        j.push(components);
        for id in 1..=components {
            j.extend_from_slice(&[id, 0x11, 0x00]);
        }
        j.extend_from_slice(&[0xff, 0xdb, 0x00, 0x43, 0x00]);
        j.extend_from_slice(&[1; 64]);
        for class in [0x00, 0x10] {
            j.extend_from_slice(&[0xff, 0xc4, 0x00, 0x14, class, 1]);
            j.extend_from_slice(&[0; 16]);
        }
        j.extend_from_slice(&[0xff, 0xda, 0x00, 6 + 2 * components, components]);
        for id in 1..=components {
            j.extend_from_slice(&[id, 0x00]);
        }
        j.extend_from_slice(&[0x00, 0x3f, 0x00]);
        j.extend_from_slice(&[0; 64]);
        j.extend_from_slice(&[0xff, 0xd9]);
        j
    }

    fn params(width: u32, height: u32) -> ImageDecodeParams {
        ImageDecodeParams { width, height, ..ImageDecodeParams::default() }
    }

    /// PdfCraft patch (48): the size check reads the frame header that zune-jpeg read, not the
    /// 1 × 1 one hidden in a TEM segment that the dimension patch's scanner found. Decoding the
    /// 65535 × 65535 frame aborted the process on a 12.9 GB allocation (three components), or
    /// took 4.3 GB (one).
    #[test]
    fn a_frame_larger_than_the_image_behind_a_tem_segment_is_refused_before_decoding() {
        for components in [1, 3] {
            assert!(decode(&grey_jpeg(65535, 65535, components, true), &Dict::empty(), &params(1, 1)).is_none());
            // The same layout at the image's own size still decodes.
            let small = decode(&grey_jpeg(8, 8, components, true), &Dict::empty(), &params(8, 8)).unwrap();
            let image = small.image_data.unwrap();
            assert_eq!((image.width, image.height), (8, 8));
            assert_eq!(small.data.len(), 64 * usize::from(components));
            assert!(small.data.iter().all(|&v| v.abs_diff(128) <= 1), "{components}: {:?}", &small.data[..3]);
            let plain = decode(&grey_jpeg(8, 8, components, false), &Dict::empty(), &params(8, 8)).unwrap();
            assert_eq!(plain.data, small.data);
        }
    }

    #[test]
    fn the_decoded_frame_is_no_larger_than_the_image_or_the_pixel_limit() {
        assert!(decoded_size_ok(8, 8, &params(8, 8)));
        assert!(decoded_size_ok(4, 8, &params(8, 8)), "a smaller frame is padded later");
        assert!(!decoded_size_ok(9, 8, &params(8, 8)));
        assert!(!decoded_size_ok(0, 8, &params(8, 8)));
        assert!(decoded_size_ok(1 << 14, 1 << 14, &params(1 << 14, 1 << 14)));
        assert!(!decoded_size_ok(1 << 14, (1 << 14) + 1, &params(1 << 15, 1 << 15)));
        assert!(!decoded_size_ok(65535, 65535, &params(65535, 65535)));
        assert_eq!(MAX_DECODED_PIXELS, 1 << 28);
    }
}
