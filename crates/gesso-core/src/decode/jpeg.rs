// Author: Dustin Pilgrim
// License: MIT

use super::DecodedImage;

pub fn decode_jpeg(data: &[u8]) -> Result<DecodedImage, String> {
    use jpeg_decoder::Decoder;

    let mut dec = Decoder::new(data);
    let pixels = dec.decode().map_err(|e| e.to_string())?;
    let info = dec.info().ok_or_else(|| "jpeg missing info".to_string())?;

    let width = info.width as u32;
    let height = info.height as u32;

    if width == 0 || height == 0 {
        return Err("jpeg invalid dimensions".into());
    }

    let wh = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "jpeg dimensions overflow".to_string())?;

    // Output: XRGB8888
    let out_stride = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| "jpeg dimensions overflow".to_string())?;

    let out_len = out_stride
        .checked_mul(height as usize)
        .ok_or_else(|| "jpeg dimensions overflow".to_string())?;

    let mut out = vec![0u8; out_len];

    // jpeg-decoder returns either:
    // - grayscale: wh bytes
    // - rgb24:     wh*3 bytes
    match pixels.len() {
        n if n == wh => {
            // Grayscale
            for y in 0..height as usize {
                let src_off = y * (width as usize);
                let dst_off = y * out_stride;

                let src_row = &pixels[src_off..src_off + (width as usize)];
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

        n if n == wh * 3 => {
            // RGB24
            let src_stride = (width as usize) * 3;

            for y in 0..height as usize {
                let src_off = y * src_stride;
                let dst_off = y * out_stride;

                let src_row = &pixels[src_off..src_off + src_stride];
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

        n => {
            return Err(format!(
                "jpeg unexpected pixel buffer size: got {n}, expected {wh} or {}",
                wh * 3
            ));
        }
    }

    Ok(DecodedImage {
        width,
        height,
        stride: out_stride,
        pixels: out,
    })
}
