//! Minimal RFB (VNC) client protocol, RFC 6143.
//!
//! Supports protocol 3.3 / 3.7 / 3.8, "None" and "VNC Authentication"
//! security, and the Tight, CopyRect and Raw encodings (plus DesktopSize
//! and LastRect).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::tight::TightDecoder;
use des::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};

const ENC_RAW: i32 = 0;
const ENC_COPYRECT: i32 = 1;
const ENC_TIGHT: i32 = 7;
const ENC_DESKTOP_SIZE: i32 = -223;
const ENC_LAST_RECT: i32 = -224;
/// Pseudo-encodings asking Tight servers for JPEG quality 7 (of 9) and zlib level 6.
const ENC_JPEG_QUALITY_7: i32 = -32 + 7;
const ENC_COMPRESS_LEVEL_6: i32 = -256 + 6;

/// Authentication outcome the UI reacts to (by asking for a password).
#[derive(Debug)]
pub enum AuthError {
    /// The server needs a password and none was given.
    PasswordRequired,
    /// The server rejected the password.
    Rejected(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AuthError::PasswordRequired => write!(f, "password required"),
            AuthError::Rejected(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Framebuffer shared between the network thread and the UI, stored as RGBA.
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub name: String,
    /// Set by the network thread whenever pixels change.
    pub dirty: bool,
}

impl Framebuffer {
    fn new(width: usize, height: usize, name: String) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * 4],
            name,
            dirty: true,
        }
    }
}

/// Write half of the connection, used by the UI to send input events.
#[derive(Clone)]
pub struct Sender {
    stream: Arc<Mutex<TcpStream>>,
}

impl Sender {
    fn send(&self, buf: &[u8]) {
        if let Ok(mut s) = self.stream.lock() {
            let _ = s.write_all(buf);
        }
    }

    /// Closes the connection; the reader thread then ends with an error.
    pub fn shutdown(&self) {
        if let Ok(s) = self.stream.lock() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    /// PointerEvent: `buttons` is a bitmask (1 = left, 2 = middle, 4 = right, 8/16 = wheel).
    pub fn pointer(&self, buttons: u8, x: u16, y: u16) {
        let mut b = vec![5, buttons];
        b.extend_from_slice(&x.to_be_bytes());
        b.extend_from_slice(&y.to_be_bytes());
        self.send(&b);
    }

    /// KeyEvent with an X11 keysym.
    pub fn key(&self, keysym: u32, down: bool) {
        let mut b = vec![4, down as u8, 0, 0];
        b.extend_from_slice(&keysym.to_be_bytes());
        self.send(&b);
    }

    pub fn request_update(&self, incremental: bool, w: u16, h: u16) {
        let mut b = vec![3, incremental as u8, 0, 0, 0, 0];
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        self.send(&b);
    }
}

pub struct Connection {
    pub fb: Arc<Mutex<Framebuffer>>,
    pub sender: Sender,
    reader: TcpStream,
    tight: TightDecoder,
}

fn read_u8(s: &mut impl Read) -> Result<u8> {
    let mut b = [0; 1];
    s.read_exact(&mut b)?;
    Ok(b[0])
}
fn read_u16(s: &mut impl Read) -> Result<u16> {
    let mut b = [0; 2];
    s.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}
fn read_u32(s: &mut impl Read) -> Result<u32> {
    let mut b = [0; 4];
    s.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}
fn read_vec(s: &mut impl Read, n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0; n];
    s.read_exact(&mut v)?;
    Ok(v)
}
fn read_reason(s: &mut impl Read) -> Result<String> {
    let len = read_u32(s)? as usize;
    Ok(String::from_utf8_lossy(&read_vec(s, len)?).into_owned())
}

/// VNC Authentication: DES-encrypt the 16-byte challenge with the password
/// (max 8 chars) as key, each key byte bit-reversed.
fn vnc_auth_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, p) in key.iter_mut().zip(password.bytes()) {
        *k = p.reverse_bits();
    }
    let cipher = des::Des::new(GenericArray::from_slice(&key));
    let mut out = *challenge;
    for chunk in out.chunks_mut(8) {
        cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
    }
    out
}

impl Connection {
    pub fn connect(addr: &str, password: &str) -> Result<Self> {
        let sock = addr
            .to_socket_addrs()
            .with_context(|| format!("cannot resolve {addr}"))?
            .next()
            .with_context(|| format!("cannot resolve {addr}"))?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(10))
            .with_context(|| format!("cannot connect to {addr}"))?;
        s.set_nodelay(true)?;
        // Generous timeout for the handshake only; cleared once connected.
        s.set_read_timeout(Some(Duration::from_secs(15)))?;

        // ProtocolVersion handshake.
        let ver = read_vec(&mut s, 12)?;
        let ver = String::from_utf8_lossy(&ver);
        if !ver.starts_with("RFB ") {
            bail!("not a VNC server (got {ver:?})");
        }
        let minor: u32 = ver[8..11].parse().unwrap_or(3);
        let minor = match minor {
            m if m >= 8 => 8,
            7 => 7,
            _ => 3,
        };
        s.write_all(format!("RFB 003.{minor:03}\n").as_bytes())?;

        // Security negotiation.
        let sec_type = if minor >= 7 {
            let n = read_u8(&mut s)?;
            if n == 0 {
                bail!("server refused connection: {}", read_reason(&mut s)?);
            }
            let types = read_vec(&mut s, n as usize)?;
            let chosen = if types.contains(&2) && !password.is_empty() {
                2
            } else if types.contains(&1) {
                1
            } else if types.contains(&2) {
                bail!(AuthError::PasswordRequired);
            } else {
                bail!("no supported security type (server offers {types:?})");
            };
            s.write_all(&[chosen])?;
            chosen
        } else {
            let t = read_u32(&mut s)?;
            if t == 0 {
                bail!("server refused connection: {}", read_reason(&mut s)?);
            }
            t as u8
        };

        if sec_type == 2 {
            if password.is_empty() {
                bail!(AuthError::PasswordRequired);
            }
            let mut challenge = [0u8; 16];
            s.read_exact(&mut challenge)?;
            s.write_all(&vnc_auth_response(password, &challenge))?;
        }
        if sec_type == 2 || minor >= 8 {
            if read_u32(&mut s)? != 0 {
                let reason = if minor >= 8 {
                    read_reason(&mut s)?
                } else {
                    "authentication failed".into()
                };
                bail!(AuthError::Rejected(reason));
            }
        }

        // ClientInit (shared = true) / ServerInit.
        s.write_all(&[1])?;
        let width = read_u16(&mut s)? as usize;
        let height = read_u16(&mut s)? as usize;
        let _server_pf = read_vec(&mut s, 16)?;
        let name = read_reason(&mut s)?;

        // SetPixelFormat: 32bpp, depth 24, little endian, true colour,
        // max 255 per channel, shifts R=16 G=8 B=0 -> bytes on the wire are B,G,R,X.
        let mut pf = vec![
            0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
        ];
        // SetEncodings.
        // In order of preference.
        let encodings = [
            ENC_TIGHT,
            ENC_COPYRECT,
            ENC_RAW,
            ENC_DESKTOP_SIZE,
            ENC_LAST_RECT,
            ENC_JPEG_QUALITY_7,
            ENC_COMPRESS_LEVEL_6,
        ];
        pf.extend_from_slice(&[2, 0]);
        pf.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
        for e in encodings {
            pf.extend_from_slice(&e.to_be_bytes());
        }
        s.write_all(&pf)?;

        s.set_read_timeout(None)?;
        let reader = s.try_clone()?;
        let sender = Sender {
            stream: Arc::new(Mutex::new(s)),
        };
        sender.request_update(false, width as u16, height as u16);

        Ok(Self {
            fb: Arc::new(Mutex::new(Framebuffer::new(width, height, name))),
            sender,
            reader,
            tight: TightDecoder::default(),
        })
    }

    /// Reads server messages until the connection closes. Run on its own thread.
    pub fn run(mut self, on_update: impl Fn()) -> Result<()> {
        let s = &mut std::io::BufReader::with_capacity(1 << 16, &self.reader);
        loop {
            match read_u8(s)? {
                0 => {
                    // FramebufferUpdate
                    read_u8(s)?;
                    let n = read_u16(s)?;
                    for _ in 0..n {
                        let x = read_u16(s)? as usize;
                        let y = read_u16(s)? as usize;
                        let w = read_u16(s)? as usize;
                        let h = read_u16(s)? as usize;
                        let enc = read_u32(s)? as i32;
                        match enc {
                            ENC_RAW => {
                                let data = read_vec(s, w * h * 4)?;
                                let mut fb = self.fb.lock().unwrap();
                                blit_raw(&mut fb, x, y, w, h, &data);
                            }
                            ENC_COPYRECT => {
                                let sx = read_u16(s)? as usize;
                                let sy = read_u16(s)? as usize;
                                let mut fb = self.fb.lock().unwrap();
                                copy_rect(&mut fb, sx, sy, x, y, w, h);
                            }
                            ENC_TIGHT => {
                                let mut fb = self.fb.lock().unwrap();
                                self.tight.decode(s, &mut fb, x, y, w, h)?;
                            }
                            ENC_LAST_RECT => break,
                            ENC_DESKTOP_SIZE => {
                                let mut fb = self.fb.lock().unwrap();
                                let name = std::mem::take(&mut fb.name);
                                *fb = Framebuffer::new(w, h, name);
                            }
                            other => bail!("server sent unsupported encoding {other}"),
                        }
                    }
                    let (w, h) = {
                        let mut fb = self.fb.lock().unwrap();
                        fb.dirty = true;
                        (fb.width as u16, fb.height as u16)
                    };
                    on_update();
                    self.sender.request_update(true, w, h);
                }
                1 => {
                    // SetColourMapEntries (unused with true colour, but must be consumed)
                    read_u8(s)?;
                    read_u16(s)?;
                    let n = read_u16(s)? as usize;
                    read_vec(s, n * 6)?;
                }
                2 => {} // Bell
                3 => {
                    // ServerCutText
                    read_vec(s, 3)?;
                    let len = read_u32(s)? as usize;
                    read_vec(s, len)?;
                }
                t => bail!("unknown server message type {t}"),
            }
        }
    }
}

fn blit_raw(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize, data: &[u8]) {
    for row in 0..h {
        let dy = y + row;
        if dy >= fb.height {
            break;
        }
        for col in 0..w {
            let dx = x + col;
            if dx >= fb.width {
                break;
            }
            let src = (row * w + col) * 4;
            let dst = (dy * fb.width + dx) * 4;
            fb.pixels[dst] = data[src + 2];
            fb.pixels[dst + 1] = data[src + 1];
            fb.pixels[dst + 2] = data[src];
            fb.pixels[dst + 3] = 255;
        }
    }
}

fn copy_rect(fb: &mut Framebuffer, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
    let w = w
        .min(fb.width.saturating_sub(x))
        .min(fb.width.saturating_sub(sx));
    let h = h
        .min(fb.height.saturating_sub(y))
        .min(fb.height.saturating_sub(sy));
    let stride = fb.width * 4;
    let src: Vec<u8> = (0..h)
        .flat_map(|r| {
            let o = (sy + r) * stride + sx * 4;
            fb.pixels[o..o + w * 4].to_vec()
        })
        .collect();
    for r in 0..h {
        let o = (y + r) * stride + x * 4;
        fb.pixels[o..o + w * 4].copy_from_slice(&src[r * w * 4..(r + 1) * w * 4]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vnc_auth_matches_known_vector() {
        // Key "password" (bit-reversed: 0e86ceceeef64e26), all-zero challenge;
        // expected value from `openssl enc -des-ecb`.
        let out = vnc_auth_response("password", &[0; 16]);
        let expected = [0xff, 0x97, 0x50, 0x2e, 0x94, 0x22, 0xf0, 0x89];
        assert_eq!(&out[..8], &expected);
        assert_eq!(&out[8..], &expected);
    }
}
