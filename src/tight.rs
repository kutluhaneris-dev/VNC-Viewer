//! Tight encoding (type 7), as sent by TightVNC, TigerVNC and libvncserver.
//!
//! Each rectangle is either a solid fill, a JPEG image, or "basic" data that
//! may be palette-indexed or gradient-filtered and is zlib-compressed on one
//! of four persistent streams. Pixels ("TPIXEL") are 3 bytes R, G, B because
//! we ask for a 32bpp / depth 24 pixel format.

use std::io::Read;

use anyhow::{Context, Result, bail};
use flate2::{Decompress, FlushDecompress, Status};

use crate::rfb::Framebuffer;

const FILL: u8 = 0x08;
const JPEG: u8 = 0x09;
const EXPLICIT_FILTER: u8 = 0x04;

const FILTER_COPY: u8 = 0;
const FILTER_PALETTE: u8 = 1;
const FILTER_GRADIENT: u8 = 2;

/// Data shorter than this is sent without zlib.
const MIN_TO_COMPRESS: usize = 12;

pub struct TightDecoder {
    streams: [Decompress; 4],
}

impl Default for TightDecoder {
    fn default() -> Self {
        Self {
            streams: std::array::from_fn(|_| Decompress::new(true)),
        }
    }
}

fn read_u8(s: &mut impl Read) -> Result<u8> {
    let mut b = [0; 1];
    s.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_vec(s: &mut impl Read, n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0; n];
    s.read_exact(&mut v)?;
    Ok(v)
}

/// 1–3 bytes, 7 bits each, low bits first; the high bit means "more follows".
fn read_compact_len(s: &mut impl Read) -> Result<usize> {
    let mut len = 0usize;
    for i in 0..3 {
        let b = read_u8(s)?;
        if i < 2 {
            len |= ((b & 0x7f) as usize) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(len);
            }
        } else {
            len |= (b as usize) << 14;
        }
    }
    Ok(len)
}

impl TightDecoder {
    pub fn decode(
        &mut self,
        s: &mut impl Read,
        fb: &mut Framebuffer,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) -> Result<()> {
        let ctl = read_u8(s)?;
        for (i, stream) in self.streams.iter_mut().enumerate() {
            if ctl & (1 << i) != 0 {
                stream.reset(true);
            }
        }
        let kind = ctl >> 4;

        if kind == FILL {
            let px = read_vec(s, 3)?;
            fill(fb, x, y, w, h, [px[0], px[1], px[2]]);
            return Ok(());
        }
        if kind == JPEG {
            let len = read_compact_len(s)?;
            let data = read_vec(s, len)?;
            let mut dec = jpeg_decoder::Decoder::new(&data[..]);
            let pixels = dec.decode().context("bad JPEG rectangle")?;
            let info = dec.info().context("bad JPEG rectangle")?;
            let rgb = match info.pixel_format {
                jpeg_decoder::PixelFormat::RGB24 => pixels,
                jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&l| [l, l, l]).collect(),
                other => bail!("unsupported JPEG pixel format {other:?}"),
            };
            if info.width as usize != w || info.height as usize != h {
                bail!(
                    "JPEG size {}x{} does not match rectangle {w}x{h}",
                    info.width,
                    info.height
                );
            }
            put_rgb(fb, x, y, w, h, &rgb);
            return Ok(());
        }
        if kind > JPEG {
            bail!("invalid Tight compression control {ctl:#04x}");
        }

        // Basic compression.
        let stream = (kind & 0x03) as usize;
        let filter = if kind & EXPLICIT_FILTER != 0 {
            read_u8(s)?
        } else {
            FILTER_COPY
        };
        match filter {
            FILTER_COPY => {
                let rgb = self.read_data(s, stream, w * h * 3)?;
                put_rgb(fb, x, y, w, h, &rgb);
            }
            FILTER_GRADIENT => {
                let mut rgb = self.read_data(s, stream, w * h * 3)?;
                undo_gradient(&mut rgb, w, h);
                put_rgb(fb, x, y, w, h, &rgb);
            }
            FILTER_PALETTE => {
                let n = read_u8(s)? as usize + 1;
                let palette = read_vec(s, n * 3)?;
                let row_bytes = if n == 2 { w.div_ceil(8) } else { w };
                let idx = self.read_data(s, stream, row_bytes * h)?;
                let mut rgb = Vec::with_capacity(w * h * 3);
                for row in 0..h {
                    for col in 0..w {
                        let i = if n == 2 {
                            (idx[row * row_bytes + col / 8] >> (7 - col % 8)) & 1
                        } else {
                            idx[row * row_bytes + col]
                        } as usize;
                        let i = i.min(n - 1);
                        rgb.extend_from_slice(&palette[i * 3..i * 3 + 3]);
                    }
                }
                put_rgb(fb, x, y, w, h, &rgb);
            }
            f => bail!("unknown Tight filter {f}"),
        }
        Ok(())
    }

    /// Reads `size` bytes of rectangle data, inflating them when the server compressed them.
    fn read_data(&mut self, s: &mut impl Read, stream: usize, size: usize) -> Result<Vec<u8>> {
        if size < MIN_TO_COMPRESS {
            return read_vec(s, size);
        }
        let len = read_compact_len(s)?;
        let input = read_vec(s, len)?;
        let z = &mut self.streams[stream];
        let mut out = vec![0u8; size];
        let (mut inp, mut outp) = (0usize, 0usize);
        while outp < size {
            let (in0, out0) = (z.total_in(), z.total_out());
            let status = z
                .decompress(&input[inp..], &mut out[outp..], FlushDecompress::Sync)
                .context("corrupt zlib data in Tight rectangle")?;
            let (read, wrote) = (
                (z.total_in() - in0) as usize,
                (z.total_out() - out0) as usize,
            );
            inp += read;
            outp += wrote;
            if read == 0 && wrote == 0 || status == Status::StreamEnd {
                break;
            }
        }
        if outp < size {
            bail!("Tight rectangle ended early ({outp} of {size} bytes)");
        }
        Ok(out)
    }
}

/// Gradient filter: each byte was stored as the difference from
/// clamp(left + up - upleft), per colour channel.
fn undo_gradient(rgb: &mut [u8], w: usize, h: usize) {
    let stride = w * 3;
    for row in 0..h {
        for col in 0..w {
            for c in 0..3 {
                let at = |r: usize, k: usize| rgb[r * stride + k * 3 + c] as i32;
                let left = if col > 0 { at(row, col - 1) } else { 0 };
                let up = if row > 0 { at(row - 1, col) } else { 0 };
                let upleft = if row > 0 && col > 0 {
                    at(row - 1, col - 1)
                } else {
                    0
                };
                let predicted = (left + up - upleft).clamp(0, 255) as u8;
                let i = row * stride + col * 3 + c;
                rgb[i] = rgb[i].wrapping_add(predicted);
            }
        }
    }
}

fn fill(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, [r, g, b]: [u8; 3]) {
    for row in y..(y + h).min(fb.height) {
        for col in x..(x + w).min(fb.width) {
            let i = (row * fb.width + col) * 4;
            fb.pixels[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
}

fn put_rgb(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, rgb: &[u8]) {
    for row in 0..h.min(fb.height.saturating_sub(y)) {
        for col in 0..w.min(fb.width.saturating_sub(x)) {
            let s = (row * w + col) * 3;
            let d = ((y + row) * fb.width + x + col) * 4;
            fb.pixels[d..d + 4].copy_from_slice(&[rgb[s], rgb[s + 1], rgb[s + 2], 255]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_len() {
        assert_eq!(read_compact_len(&mut &[0x05][..]).unwrap(), 5);
        assert_eq!(read_compact_len(&mut &[0x90, 0x4e][..]).unwrap(), 10000);
        assert_eq!(
            read_compact_len(&mut &[0xff, 0xff, 0x3f][..]).unwrap(),
            0x0f_ffff
        );
    }

    #[test]
    fn gradient_roundtrip() {
        // A smooth 3x2 image, encoded with the same predictor, must decode back.
        let img: Vec<u8> = (0..18).map(|i| (i * 7) as u8).collect();
        let (w, h) = (3, 2);
        let mut enc = img.clone();
        for row in 0..h {
            for col in 0..w {
                for c in 0..3 {
                    let at = |r: usize, k: usize| img[r * w * 3 + k * 3 + c] as i32;
                    let left = if col > 0 { at(row, col - 1) } else { 0 };
                    let up = if row > 0 { at(row - 1, col) } else { 0 };
                    let ul = if row > 0 && col > 0 {
                        at(row - 1, col - 1)
                    } else {
                        0
                    };
                    let p = (left + up - ul).clamp(0, 255) as u8;
                    let i = row * w * 3 + col * 3 + c;
                    enc[i] = img[i].wrapping_sub(p);
                }
            }
        }
        undo_gradient(&mut enc, w, h);
        assert_eq!(enc, img);
    }
}
