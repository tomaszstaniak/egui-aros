//! ASL file and drawer requesters that return absolute paths.
//!
//! ASL returns the drawer as typed or browsed to: empty (the current directory) or relative.
//! A program started from Workbench runs in its own drawer, so such a path names a different
//! file there; the drawer is resolved with `Lock` and `NameFromLock` before it is returned.
//! Paths are passed to and from DOS byte for byte, as Rust's std does; titles and patterns are
//! converted to ISO 8859-1, the requester's charset.

use crate::WindowHandle;

/// What the requester selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// One file to open.
    Open,
    /// One or more files to open.
    OpenMany,
    /// One file name to save to.
    Save,
    /// A drawer.
    Drawer,
}

/// Shows an ASL requester over `window` (or on its own if the window is closed) and returns the
/// chosen absolute paths; empty when cancelled. `dir`, `file` and `pattern` may be empty.
pub fn request(window: &WindowHandle, select: Select, title: &str, dir: &str, file: &str, pattern: &str) -> Vec<String> {
    #[cfg(target_os = "aros")]
    {
        use std::ffi::{c_char, CString};
        let cbytes = |b: Vec<u8>| CString::new(b.into_iter().filter(|&c| c != 0).collect::<Vec<u8>>()).unwrap_or_default();
        let (title, dir, file, pattern) =
            (cbytes(to_latin1(title)), cbytes(dir.as_bytes().to_vec()), cbytes(file.as_bytes().to_vec()), cbytes(to_latin1(pattern)));
        let (save, mode) = match select {
            Select::Open => (0, 0),
            Select::OpenMany => (0, 1),
            Select::Save => (1, 0),
            Select::Drawer => (0, 2),
        };
        let mut out = vec![0u8; 16 * 1024];
        let win = window.raw().unwrap_or(std::ptr::null_mut());
        // SAFETY: every string is NUL-terminated and outlives the call; `out` holds `outlen`
        // bytes; `win` is the open window or NULL.
        let n = unsafe {
            crate::sys::ea_file_request(
                win,
                save,
                mode,
                title.as_ptr(),
                dir.as_ptr(),
                file.as_ptr(),
                pattern.as_ptr(),
                out.as_mut_ptr() as *mut c_char,
                out.len() as i32,
            )
        };
        if n <= 0 {
            return Vec::new();
        }
        let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
        out[..end].split(|&b| b == b'\n').filter(|p| !p.is_empty()).map(path_from_bytes).collect()
    }
    #[cfg(not(target_os = "aros"))]
    {
        let _ = (window, select, title, dir, file, pattern);
        Vec::new()
    }
}

/// UTF-8 to ISO 8859-1; common punctuation outside it gets its plain equivalent, anything else
/// becomes `?`.
pub fn to_latin1(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c as u32 {
            n if n < 256 => out.push(n as u8),
            0x2010..=0x2015 | 0x2212 => out.push(b'-'),
            0x2018 | 0x2019 | 0x201a => out.push(b'\''),
            0x201c | 0x201d | 0x201e => out.push(b'"'),
            0x2026 => out.extend_from_slice(b"..."),
            _ => out.push(b'?'),
        }
    }
    out
}

/// A path from DOS: UTF-8 if it is valid UTF-8 (as Rust's std wrote it), else ISO 8859-1.
pub fn path_from_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_round_trip_and_fallbacks() {
        assert_eq!(to_latin1("Zażółć – \u{2026}"), b"Za?\xf3?? - ...".to_vec());
        assert_eq!(path_from_bytes(b"RAM:caf\xe9"), "RAM:café");
        assert_eq!(path_from_bytes("RAM:zażółć".as_bytes()), "RAM:zażółć");
    }
}
