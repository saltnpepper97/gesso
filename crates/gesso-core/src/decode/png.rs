use super::DecodedImage;

pub fn decode_png(data: &[u8]) -> Result<DecodedImage, String> {
    use std::io::Cursor;

    let decoder = png::Decoder::new(Cursor::new(data));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;

    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "png could not determine output buffer size".to_string())?;

    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;

    let width = info.width;
    let height = info.height;

    if width == 0 || height == 0 {
        return Err("png invalid dimensions".into());
    }

    let bytes = &buf[..info.buffer_size()];

    // Output is always XRGB8888 (B,G,R,0)
    let out_stride = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| "png dimensions overflow".to_string())?;

    let out_len = out_stride
        .checked_mul(height as usize)
        .ok_or_else(|| "png dimensions overflow".to_string())?;

    let mut out = vec![0u8; out_len];

    use png::{BitDepth, ColorType};

    match (info.color_type, info.bit_depth) {
        (ColorType::Rgb, BitDepth::Eight) => {
            let src_stride = (width as usize)
                .checked_mul(3)
                .ok_or_else(|| "png dimensions overflow".to_string())?;

            // bytes: RGBRGB...
            for y in 0..height as usize {
                let src_off = y * src_stride;
                let dst_off = y * out_stride;

                let src_row = &bytes[src_off..src_off + src_stride];
                let dst_row = &mut out[dst_off..dst_off + out_stride];

                for x in 0..width as usize {
                    let si = x * 3;
                    let di = x * 4;

                    let r = src_row[si];
                    let g = src_row[si + 1];
                    let b = src_row[si + 2];

                    dst_row[di] = b;
                    dst_row[di + 1] = g;
                    dst_row[di + 2] = r;
                    dst_row[di + 3] = 0;
                }
            }
        }

        (ColorType::Rgba, BitDepth::Eight) => {
            let src_stride = out_stride; // width * 4

            // bytes: RGBARGBA...
            // Drop alpha (wallpapers don’t need it).
            for y in 0..height as usize {
                let src_off = y * src_stride;
                let dst_off = y * out_stride;

                let src_row = &bytes[src_off..src_off + src_stride];
                let dst_row = &mut out[dst_off..dst_off + out_stride];

                for x in 0..width as usize {
                    let si = x * 4;
                    let di = x * 4;

                    let r = src_row[si];
                    let g = src_row[si + 1];
                    let b = src_row[si + 2];

                    dst_row[di] = b;
                    dst_row[di + 1] = g;
                    dst_row[di + 2] = r;
                    dst_row[di + 3] = 0;
                }
            }
        }

        (ColorType::Grayscale, BitDepth::Eight) => {
            let src_stride = width as usize;

            // bytes: Y...
            for y in 0..height as usize {
                let src_off = y * src_stride;
                let dst_off = y * out_stride;

                let src_row = &bytes[src_off..src_off + src_stride];
                let dst_row = &mut out[dst_off..dst_off + out_stride];

                for x in 0..width as usize {
                    let v = src_row[x];
                    let di = x * 4;

                    dst_row[di] = v;
                    dst_row[di + 1] = v;
                    dst_row[di + 2] = v;
                    dst_row[di + 3] = 0;
                }
            }
        }

        other => {
            // Keep minimal on purpose. We can add:
            // - palette expansion
            // - grayscale+alpha
            // - 16-bit downshift
            return Err(format!("png unsupported format: {:?}", other));
        }
    }

    Ok(DecodedImage {
        width,
        height,
        stride: out_stride,
        pixels: out,
    })
}
