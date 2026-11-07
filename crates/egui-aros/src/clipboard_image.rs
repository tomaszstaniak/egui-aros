//! Pictures on the system clipboard (clipboard.device unit 0): read through datatypes, written as
//! a 24-bit IFF ILBM. The implementation is aros-gui-sys' `clipboard` (patch 0004), which
//! egui-winit uses too for Cmd/Ctrl+V (`Event::PasteImage`); this module keeps egui-aros' names.
//!
//! Ownership: [`clipboard_image`] returns pixels in a `Vec` allocated after the size check;
//! nothing of the datatype object outlives the call. [`set_clipboard_image`] copies from the
//! caller's slice. Neither changes what is on the clipboard when it fails, and reading never
//! changes it.

pub use aros_gui_sys::clipboard::{encode_ilbm, ClipboardImage, MAX_BYTES, MAX_SIDE};

/// The picture on the clipboard.
pub fn clipboard_image() -> ClipboardImage {
    aros_gui_sys::clipboard::read_picture()
}

/// Puts `rgba` (straight RGBA8, `stride` bytes per row, `width * 4` at least) on the clipboard as a
/// 24-bit ILBM. Alpha is not written: a 24-bit ILBM has none, and the colour bytes are written as
/// they are (a caller that wants transparent areas shown on a background composites first).
pub fn set_clipboard_image(width: u32, height: u32, stride: usize, rgba: &[u8]) -> Result<(), String> {
    aros_gui_sys::clipboard::write_picture(width, height, stride, rgba)
}

/// Writes a FORM ILBM with exactly these BMHD and BODY payloads, unchecked: for tests and tools
/// that need a damaged or unusual picture on the clipboard. Use [`set_clipboard_image`] otherwise.
pub fn write_ilbm_payloads(bmhd: &[u8; 20], body: &[u8]) -> Result<(), String> {
    aros_gui_sys::clipboard::write_ilbm_payloads(bmhd, body)
}
