//! Pictures on the system clipboard (clipboard.device unit 0).
//!
//! Reading goes through datatypes.library (`DTST_CLIPBOARD`, `PDTM_READPIXELARRAY` in
//! `PBPAFMT_RGBA`), so every picture format a datatype can read from the clipboard arrives the
//! same way; AROS programs normally put IFF ILBM there. Writing puts a 24-bit IFF ILBM there
//! (BMHD and a ByteRun1 BODY, no CMAP), which picture.datatype, MultiView and the ILBM readers of
//! other programs read; datatypes' `DTM_COPY` is not needed (picture.datatype has none).
//!
//! Ownership: [`clipboard_image`] returns pixels in a `Vec` this crate allocated; nothing of the
//! datatype object outlives the call. [`set_clipboard_image`] copies from the caller's slice.
//! Neither changes what is on the clipboard when it fails, and reading never changes it.

/// Largest width or height read or written. Larger pictures are refused, not scaled.
pub const MAX_SIDE: u32 = 16384;
/// Largest picture read, in RGBA bytes (64 Mpixel).
pub const MAX_BYTES: usize = 256 << 20;

/// What [`clipboard_image`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardImage {
    /// No picture: the clipboard is empty or holds text.
    None,
    /// Straight (not premultiplied) RGBA8, `width * 4` bytes per row, top row first. A picture
    /// without alpha has 255 in every alpha byte.
    Pixels { width: u32, height: u32, rgba: Vec<u8> },
    /// A picture is there but cannot be used, and why (no datatype for it, a damaged or cut
    /// stream, too large, out of memory).
    Unreadable(String),
}

/// The picture on the clipboard.
pub fn clipboard_image() -> ClipboardImage {
    #[cfg(target_os = "aros")]
    return native::read();
    #[cfg(not(target_os = "aros"))]
    ClipboardImage::None
}

/// Puts `rgba` (straight RGBA8, `stride` bytes per row, `width * 4` at least) on the clipboard as a
/// 24-bit ILBM. Alpha is not written: a 24-bit ILBM has none, and the colour bytes are written as
/// they are (a caller that wants transparent areas shown on a background composites first).
pub fn set_clipboard_image(width: u32, height: u32, stride: usize, rgba: &[u8]) -> Result<(), String> {
    let (bmhd, body) = encode_ilbm(width, height, stride, rgba)?;
    #[cfg(target_os = "aros")]
    return native::write(&bmhd, &body);
    #[cfg(not(target_os = "aros"))]
    {
        let _ = (bmhd, body);
        Err("no system clipboard on this platform".into())
    }
}

/// Writes a FORM ILBM with exactly these BMHD and BODY payloads, unchecked: for tests and tools
/// that need a damaged or unusual picture on the clipboard. Use [`set_clipboard_image`] otherwise.
pub fn write_ilbm_payloads(bmhd: &[u8; 20], body: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "aros")]
    return native::write(bmhd, body);
    #[cfg(not(target_os = "aros"))]
    {
        let _ = (bmhd, body);
        Err("no system clipboard on this platform".into())
    }
}

/// Checks a picture's size against the limits; the byte count of its RGBA pixels.
fn checked_size(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 {
        return Err(format!("the picture has no pixels ({width}x{height})"));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("the picture is {width}x{height}; at most {MAX_SIDE} pixels a side are supported"));
    }
    let bytes = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
    match bytes {
        Some(n) if n <= MAX_BYTES => Ok(n),
        _ => Err(format!("the picture is {width}x{height}, more than {} MiB of pixels", MAX_BYTES >> 20)),
    }
}

/// The BMHD payload (20 bytes) and the ByteRun1 BODY of a 24-bit ILBM for straight RGBA8.
pub fn encode_ilbm(width: u32, height: u32, stride: usize, rgba: &[u8]) -> Result<([u8; 20], Vec<u8>), String> {
    checked_size(width, height)?;
    let (w, h) = (width as usize, height as usize);
    if stride < w * 4 {
        return Err(format!("row stride {stride} is less than {} bytes", w * 4));
    }
    let needed = stride.checked_mul(h - 1).and_then(|n| n.checked_add(w * 4)).ok_or("picture too large")?;
    if rgba.len() < needed {
        return Err(format!("{} bytes of pixels for a {width}x{height} picture that needs {needed}", rgba.len()));
    }
    let mut bmhd = [0u8; 20];
    bmhd[0..2].copy_from_slice(&(width as u16).to_be_bytes());
    bmhd[2..4].copy_from_slice(&(height as u16).to_be_bytes());
    // x, y = 0; nPlanes 24; masking 0 (none); compression 1 (ByteRun1); pad; transparentColor 0.
    bmhd[8] = 24;
    bmhd[9] = 0;
    bmhd[10] = 1;
    // Square pixels; page size = picture size.
    bmhd[14] = 1;
    bmhd[15] = 1;
    bmhd[16..18].copy_from_slice(&(width as u16).to_be_bytes());
    bmhd[18..20].copy_from_slice(&(height as u16).to_be_bytes());

    // Each plane row is padded to a whole number of 16-bit words.
    let row_bytes = w.div_ceil(16) * 2;
    let mut plane = vec![0u8; row_bytes];
    let mut body = Vec::with_capacity(h * 24 * row_bytes / 2);
    for y in 0..h {
        let row = &rgba[y * stride..y * stride + w * 4];
        // Planes 0-7 are red's bits from the lowest, 8-15 green's, 16-23 blue's.
        for p in 0..24 {
            let (channel, bit) = (p / 8, p % 8);
            plane.fill(0);
            for x in 0..w {
                if row[x * 4 + channel] >> bit & 1 != 0 {
                    plane[x / 8] |= 0x80 >> (x % 8);
                }
            }
            byterun1(&plane, &mut body);
        }
    }
    Ok((bmhd, body))
}

/// ByteRun1 (PackBits): runs of 3..128 equal bytes as (257 - n, byte), others as literals of up to
/// 128 bytes as (n - 1, bytes...).
fn byterun1(src: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < src.len() {
        let mut run = 1;
        while i + run < src.len() && run < 128 && src[i + run] == src[i] {
            run += 1;
        }
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(src[i]);
            i += run;
            continue;
        }
        // A literal stretch up to the next run of three or more.
        let start = i;
        while i < src.len() && i - start < 128 {
            if i + 2 < src.len() && src[i] == src[i + 1] && src[i] == src[i + 2] {
                break;
            }
            i += 1;
        }
        out.push((i - start - 1) as u8);
        out.extend_from_slice(&src[start..i]);
    }
}

#[cfg(target_os = "aros")]
mod native {
    use super::{checked_size, ClipboardImage};
    use std::ffi::c_char;

    extern "C" {
        fn ea_clip_image_open(kind: *mut i32, width: *mut u32, height: *mut u32, err: *mut c_char, cap: i32) -> *mut std::ffi::c_void;
        fn ea_clip_image_read(handle: *mut std::ffi::c_void, rgba: *mut u8, stride: u32) -> i32;
        fn ea_clip_image_close(handle: *mut std::ffi::c_void);
        fn ea_clip_ilbm_write(bmhd: *const u8, body: *const u8, body_len: u32, err: *mut c_char, cap: i32) -> i32;
    }

    // Kinds ea_clip_image_open reports.
    const EMPTY: i32 = 0;
    const TEXT: i32 = 1;
    const PICTURE: i32 = 2;

    fn message(err: &[u8]) -> String {
        let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
        aros_gui_sys::charset::system().decode(&err[..end])
    }

    pub fn read() -> ClipboardImage {
        let (mut kind, mut w, mut h) = (EMPTY, 0u32, 0u32);
        let mut err = [0u8; 160];
        // SAFETY: out-parameters and a buffer of the stated size.
        let handle = unsafe { ea_clip_image_open(&mut kind, &mut w, &mut h, err.as_mut_ptr().cast(), err.len() as i32) };
        if handle.is_null() {
            return match kind {
                EMPTY | TEXT => ClipboardImage::None,
                _ => ClipboardImage::Unreadable(message(&err)),
            };
        }
        debug_assert_eq!(kind, PICTURE);
        let result = (|| {
            let bytes = checked_size(w, h)?;
            let mut rgba = Vec::new();
            rgba.try_reserve_exact(bytes).map_err(|_| format!("not enough memory for a {w}x{h} picture"))?;
            rgba.resize(bytes, 0);
            // SAFETY: `rgba` holds height rows of width * 4 bytes; the handle is open.
            if unsafe { ea_clip_image_read(handle, rgba.as_mut_ptr(), w * 4) } == 0 {
                return Err("the picture's datatype could not give its pixels".to_string());
            }
            Ok(ClipboardImage::Pixels { width: w, height: h, rgba })
        })();
        // SAFETY: opened above, closed once.
        unsafe { ea_clip_image_close(handle) };
        result.unwrap_or_else(ClipboardImage::Unreadable)
    }

    pub fn write(bmhd: &[u8; 20], body: &[u8]) -> Result<(), String> {
        let len = u32::try_from(body.len()).map_err(|_| "picture too large for the clipboard".to_string())?;
        let mut err = [0u8; 160];
        // SAFETY: both buffers outlive the call; err has the stated size.
        if unsafe { ea_clip_ilbm_write(bmhd.as_ptr(), body.as_ptr(), len, err.as_mut_ptr().cast(), err.len() as i32) } == 0 {
            Ok(())
        } else {
            Err(message(&err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain ILBM reader for the tests: BMHD + ByteRun1 BODY, 24 planes, back to RGB.
    fn decode(bmhd: &[u8; 20], body: &[u8]) -> (u32, u32, Vec<[u8; 3]>) {
        let w = u16::from_be_bytes([bmhd[0], bmhd[1]]) as usize;
        let h = u16::from_be_bytes([bmhd[2], bmhd[3]]) as usize;
        assert_eq!((bmhd[8], bmhd[9], bmhd[10]), (24, 0, 1));
        let row_bytes = w.div_ceil(16) * 2;
        let mut pos = 0;
        let mut unpack = || {
            let mut out = Vec::with_capacity(row_bytes);
            while out.len() < row_bytes {
                let n = body[pos] as i8;
                pos += 1;
                if n >= 0 {
                    out.extend_from_slice(&body[pos..pos + n as usize + 1]);
                    pos += n as usize + 1;
                } else if n != -128 {
                    out.extend(std::iter::repeat_n(body[pos], (1 - n as isize) as usize));
                    pos += 1;
                }
            }
            assert_eq!(out.len(), row_bytes, "a ByteRun1 row overran its plane");
            out
        };
        let mut px = vec![[0u8; 3]; w * h];
        for y in 0..h {
            for p in 0..24 {
                let plane = unpack();
                for x in 0..w {
                    if plane[x / 8] & (0x80 >> (x % 8)) != 0 {
                        px[y * w + x][p / 8] |= 1 << (p % 8);
                    }
                }
            }
        }
        (w as u32, h as u32, px)
    }

    fn picture(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                // Gradients, a noisy channel and varying alpha.
                v.extend_from_slice(&[(x * 7) as u8, (y * 13) as u8, ((x * 31) ^ (y * 17)) as u8, (x + y) as u8]);
            }
        }
        v
    }

    #[test]
    fn odd_width_round_trips_pixel_exact() {
        for (w, h) in [(1, 1), (17, 5), (33, 9), (255, 3), (300, 2)] {
            let rgba = picture(w, h);
            let (bmhd, body) = encode_ilbm(w, h, w as usize * 4, &rgba).unwrap();
            let (dw, dh, px) = decode(&bmhd, &body);
            assert_eq!((dw, dh), (w, h));
            for (i, p) in px.iter().enumerate() {
                assert_eq!(p, &rgba[i * 4..i * 4 + 3], "pixel {i} of {w}x{h}");
            }
        }
    }

    #[test]
    fn stride_padding_is_ignored() {
        let (w, h) = (5u32, 3u32);
        let tight = picture(w, h);
        let stride = 32;
        let mut padded = vec![0xEEu8; stride * h as usize];
        for y in 0..h as usize {
            padded[y * stride..y * stride + 20].copy_from_slice(&tight[y * 20..y * 20 + 20]);
        }
        assert_eq!(encode_ilbm(w, h, stride, &padded).unwrap(), encode_ilbm(w, h, 20, &tight).unwrap());
    }

    #[test]
    fn byterun1_runs_and_literals() {
        let mut out = Vec::new();
        byterun1(&[0; 200], &mut out);
        assert_eq!(out, vec![0x81, 0, 0xB9, 0]); // 128 + 72
        out.clear();
        byterun1(&[1, 2, 3, 3, 3, 3, 4], &mut out);
        assert_eq!(out, vec![1, 1, 2, 0xFD, 3, 0, 4]);
        out.clear();
        let lit: Vec<u8> = (0..130).map(|i| i as u8).collect();
        byterun1(&lit, &mut out);
        assert_eq!(out[0], 127);
        assert_eq!(out[129], 1);
    }

    #[test]
    fn limits_and_bad_input_are_refused() {
        assert!(encode_ilbm(0, 4, 0, &[]).is_err());
        assert!(encode_ilbm(MAX_SIDE + 1, 1, (MAX_SIDE as usize + 1) * 4, &[]).is_err());
        assert!(encode_ilbm(4, 4, 15, &[0; 64]).is_err(), "stride below width*4");
        assert!(encode_ilbm(4, 4, 16, &[0; 63]).is_err(), "short buffer");
        assert!(checked_size(MAX_SIDE, MAX_SIDE).is_err(), "over MAX_BYTES");
        assert_eq!(checked_size(8192, 8192), Ok(8192 * 8192 * 4));
    }

    #[test]
    fn bmhd_fields() {
        let (b, _) = encode_ilbm(640, 480, 2560, &vec![0; 640 * 480 * 4]).unwrap();
        assert_eq!(&b[0..4], &[2, 128, 1, 224]);
        assert_eq!(&b[8..11], &[24, 0, 1]);
        assert_eq!(&b[14..20], &[1, 1, 2, 128, 1, 224]);
    }
}
