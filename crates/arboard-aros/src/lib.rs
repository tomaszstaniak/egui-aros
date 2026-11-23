//! The part of the [arboard](https://crates.io/crates/arboard) 3 API that the Craft programs use,
//! for AROS: text and pictures on the system clipboard (unit 0), through aros-gui-sys (as
//! egui-aros does). HTML is put
//! on the clipboard as its plain-text alternative, the form other AROS programs read; a file
//! list is not available ([`Error::ClipboardNotSupported`]).

use std::borrow::Cow;
use std::path::Path;

/// A picture as rows of RGBA pixels, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData<'a> {
    pub width: usize,
    pub height: usize,
    pub bytes: Cow<'a, [u8]>,
}

impl ImageData<'_> {
    pub fn to_owned_img(&self) -> ImageData<'static> {
        ImageData { width: self.width, height: self.height, bytes: Cow::Owned(self.bytes.to_vec()) }
    }
}

/// arboard's errors.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The clipboard holds nothing in the asked-for form.
    ContentNotAvailable,
    /// This form cannot be put on or read from the AROS clipboard.
    ClipboardNotSupported,
    /// The clipboard is in use.
    ClipboardOccupied,
    /// The data could not be converted to or from the clipboard's form.
    ConversionFailure,
    Unknown { description: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ContentNotAvailable => f.write_str("The clipboard contents were not available in the requested format or the clipboard is empty."),
            Error::ClipboardNotSupported => f.write_str("The selected clipboard is not supported with the current system configuration."),
            Error::ClipboardOccupied => f.write_str("The native clipboard is not accessible due to being held by an other party."),
            Error::ConversionFailure => f.write_str("The image or the text that was about the be transferred to/from the clipboard could not be converted to the appropriate format."),
            Error::Unknown { description } => write!(f, "Unknown error while interacting with the clipboard: {description}"),
        }
    }
}

impl std::error::Error for Error {}

/// The system clipboard. Holds nothing: every call reads or writes the clipboard device.
#[derive(Debug, Default)]
pub struct Clipboard(());

impl Clipboard {
    pub fn new() -> Result<Self, Error> {
        Ok(Self(()))
    }

    pub fn get_text(&mut self) -> Result<String, Error> {
        #[cfg(target_os = "aros")]
        return aros_gui_sys::clipboard::read_text().ok_or(Error::ContentNotAvailable);
        #[cfg(not(target_os = "aros"))]
        Err(Error::ClipboardNotSupported)
    }

    pub fn set_text<'a, T: Into<Cow<'a, str>>>(&mut self, text: T) -> Result<(), Error> {
        put_text(&text.into())
    }

    pub fn get_image(&mut self) -> Result<ImageData<'static>, Error> {
        get_image()
    }

    pub fn set_image(&mut self, image: ImageData<'_>) -> Result<(), Error> {
        put_image(&image)
    }

    pub fn clear(&mut self) -> Result<(), Error> {
        put_text("")
    }

    pub fn get(&mut self) -> Get<'_> {
        Get(self)
    }

    pub fn set(&mut self) -> Set<'_> {
        Set(self)
    }
}

/// [`Clipboard::get`].
pub struct Get<'c>(&'c mut Clipboard);

impl Get<'_> {
    pub fn text(self) -> Result<String, Error> {
        self.0.get_text()
    }

    pub fn image(self) -> Result<ImageData<'static>, Error> {
        get_image()
    }

    /// HTML is not kept on the AROS clipboard.
    pub fn html(self) -> Result<String, Error> {
        Err(Error::ContentNotAvailable)
    }

    pub fn file_list(self) -> Result<Vec<std::path::PathBuf>, Error> {
        Err(Error::ClipboardNotSupported)
    }
}

/// [`Clipboard::set`].
pub struct Set<'c>(&'c mut Clipboard);

impl Set<'_> {
    pub fn text<'a, T: Into<Cow<'a, str>>>(self, text: T) -> Result<(), Error> {
        put_text(&text.into())
    }

    /// The plain-text alternative goes on the clipboard (HTML itself has no AROS clipboard form);
    /// without one, nothing does.
    pub fn html<'a, T: Into<Cow<'a, str>>>(self, _html: T, alt_text: Option<T>) -> Result<(), Error> {
        match alt_text {
            Some(text) => put_text(&text.into()),
            None => Err(Error::ClipboardNotSupported),
        }
    }

    pub fn image(self, image: ImageData<'_>) -> Result<(), Error> {
        put_image(&image)
    }

    pub fn file_list(self, _paths: &[impl AsRef<Path>]) -> Result<(), Error> {
        Err(Error::ClipboardNotSupported)
    }
}

fn put_text(text: &str) -> Result<(), Error> {
    #[cfg(target_os = "aros")]
    let written = aros_gui_sys::clipboard::write_text(text);
    #[cfg(not(target_os = "aros"))]
    let written = { let _ = text; false };
    if written { Ok(()) } else { Err(Error::Unknown { description: "the clipboard device could not be written".into() }) }
}

fn get_image() -> Result<ImageData<'static>, Error> {
    use aros_gui_sys::clipboard::ClipboardImage;
    match aros_gui_sys::clipboard::read_picture() {
        ClipboardImage::None => Err(Error::ContentNotAvailable),
        ClipboardImage::Pixels { width, height, rgba } => {
            Ok(ImageData { width: width as usize, height: height as usize, bytes: Cow::Owned(rgba) })
        }
        ClipboardImage::Unreadable(why) => Err(Error::Unknown { description: why }),
    }
}

fn put_image(image: &ImageData<'_>) -> Result<(), Error> {
    let (w, h) = (u32::try_from(image.width).map_err(|_| Error::ConversionFailure)?, u32::try_from(image.height).map_err(|_| Error::ConversionFailure)?);
    if w == 0 || h == 0 || image.width.checked_mul(image.height).and_then(|p| p.checked_mul(4)) != Some(image.bytes.len()) {
        return Err(Error::ConversionFailure);
    }
    aros_gui_sys::clipboard::write_picture(w, h, image.width * 4, &image.bytes).map_err(|description| Error::Unknown { description })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_whose_size_does_not_match_its_pixels_is_refused() {
        let bad = ImageData { width: 2, height: 2, bytes: Cow::Owned(vec![0; 15]) };
        assert!(matches!(Clipboard::new().unwrap().set_image(bad), Err(Error::ConversionFailure)));
    }

    #[test]
    fn html_without_a_text_alternative_and_file_lists_are_not_supported() {
        let mut cb = Clipboard::new().unwrap();
        assert!(matches!(cb.set().html("<b>x</b>", None), Err(Error::ClipboardNotSupported)));
        assert!(matches!(cb.set().file_list(&["a.pdf"]), Err(Error::ClipboardNotSupported)));
    }
}
