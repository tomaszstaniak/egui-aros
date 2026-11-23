//! The part of the [rfd](https://crates.io/crates/rfd) 0.17 API that the Craft programs use, for
//! AROS: [`AsyncFileDialog`] and [`FileDialog`] with titles, filters, a start drawer and a file
//! name, answered by an ASL requester over the program's window ([`egui_aros::window`]).
//!
//! The requester opens when `pick_file`, `pick_files`, `pick_folder` or `save_file` is called,
//! on the calling thread, and the returned future is already complete: rfd's own contract on
//! macOS, where the panel shows when the future is created. Paths come back in the `-nix` form
//! the programs run with (`/Work/a.pdf`); the requester itself speaks DOS names.
//!
//! ASL has one pattern per requester: it lists the files of every filter added (case does not
//! matter to it). A filter of `*` lists everything.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A chosen file or drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHandle(PathBuf);

impl FileHandle {
    pub fn wrap(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn file_name(&self) -> String {
        self.0.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn inner(&self) -> &Path {
        &self.0
    }

    pub async fn read(&self) -> Vec<u8> {
        std::fs::read(&self.0).unwrap_or_default()
    }

    pub async fn write(&self, data: &[u8]) -> std::io::Result<()> {
        std::fs::write(&self.0, data)
    }
}

impl From<PathBuf> for FileHandle {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

impl From<FileHandle> for PathBuf {
    fn from(h: FileHandle) -> Self {
        h.0
    }
}

/// A future that is complete when it is made (see the crate documentation).
pub struct Ready<T>(Option<T>);

impl<T: Unpin> Future for Ready<T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<T> {
        Poll::Ready(self.0.take().expect("Ready polled after completion"))
    }
}

#[derive(Clone, Debug, Default)]
struct Setup {
    title: String,
    filters: Vec<(String, Vec<String>)>,
    directory: Option<PathBuf>,
    file_name: String,
}

impl Setup {
    fn title_or(&self, default: &str) -> String {
        if self.title.is_empty() { default.to_owned() } else { self.title.clone() }
    }

    fn show(&self, select: egui_aros::asl::Select, default_title: &str) -> Vec<FileHandle> {
        let pattern = if matches!(select, egui_aros::asl::Select::Drawer) { String::new() } else { pattern(&self.filters) };
        let dir = self.directory.as_deref().and_then(|d| d.to_str()).map(dos_name).unwrap_or_default();
        let Some(window) = egui_aros::window() else {
            log("no open window for the requester");
            return Vec::new();
        };
        egui_aros::asl::request(&window, select, &self.title_or(default_title), &dir, &self.file_name, &pattern)
            .into_iter()
            .filter_map(|dos| match egui_aros::startup::dos_to_nix(&dos) {
                Ok(p) => Some(FileHandle(PathBuf::from(p))),
                Err(e) => {
                    log(&format!("{dos}: {e}"));
                    None
                }
            })
            .collect()
    }
}

fn log(message: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "rfd (AROS): {message}");
}

/// A `-nix` drawer name as DOS names it; DOS names and relative names unchanged.
fn dos_name(dir: &str) -> String {
    if dir.starts_with('/') { egui_aros::startup::nix_to_dos(dir).unwrap_or_default() } else { dir.to_owned() }
}

/// One AmigaDOS pattern for all the filters' extensions: `#?.(pdf|png)`. Extensions may be given
/// as the any-case globs some programs build for case-sensitive pickers (`[pP][dD][fF]`); ASL
/// ignores case, so they are folded back. Empty (every file) when there is no filter or one of
/// them is `*`.
fn pattern(filters: &[(String, Vec<String>)]) -> String {
    let mut exts: Vec<String> = Vec::new();
    for (_, list) in filters {
        for e in list {
            let e = fold_any_case(e.trim().trim_start_matches("*.").trim_start_matches('.'));
            if e.is_empty() || e == "*" {
                return String::new();
            }
            if !e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                continue;
            }
            if !exts.contains(&e) {
                exts.push(e);
            }
        }
    }
    if exts.is_empty() {
        return String::new();
    }
    // asl.library copies the pattern into a 257-byte buffer without a length check: past that,
    // every file is listed rather than risk it.
    let p = format!("#?.({})", exts.join("|"));
    if p.len() < 250 { p } else { String::new() }
}

/// `[pP][dD][fF]` as `pdf`.
fn fold_any_case(glob: &str) -> String {
    let mut out = String::new();
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            let mut class = String::new();
            for d in chars.by_ref() {
                if d == ']' {
                    break;
                }
                class.push(d);
            }
            out.extend(class.chars().next().map(|c| c.to_ascii_lowercase()));
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

macro_rules! builder {
    ($name:ident) => {
        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Setup::default())
            }

            #[must_use]
            pub fn set_title(mut self, title: impl Into<String>) -> Self {
                self.0.title = title.into();
                self
            }

            #[must_use]
            pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
                self.0.filters.push((name.into(), extensions.iter().map(ToString::to_string).collect()));
                self
            }

            #[must_use]
            pub fn set_directory<P: AsRef<Path>>(mut self, path: P) -> Self {
                self.0.directory = Some(path.as_ref().to_path_buf());
                self
            }

            #[must_use]
            pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
                self.0.file_name = file_name.into();
                self
            }

            /// The requester always opens over the program's window.
            #[must_use]
            pub fn set_parent<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle>(self, _parent: &W) -> Self {
                self
            }

            /// ASL can always make a new drawer.
            #[must_use]
            pub fn set_can_create_directories(self, _can: bool) -> Self {
                self
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

/// rfd's asynchronous dialog; the requester opens when a `pick_*` or `save_file` is called.
#[derive(Clone, Debug)]
pub struct AsyncFileDialog(Setup);
builder!(AsyncFileDialog);

impl AsyncFileDialog {
    pub fn pick_file(self) -> Ready<Option<FileHandle>> {
        Ready(Some(self.0.show(egui_aros::asl::Select::Open, "Open").into_iter().next()))
    }

    pub fn pick_files(self) -> Ready<Option<Vec<FileHandle>>> {
        let files = self.0.show(egui_aros::asl::Select::OpenMany, "Open");
        Ready(Some((!files.is_empty()).then_some(files)))
    }

    pub fn pick_folder(self) -> Ready<Option<FileHandle>> {
        Ready(Some(self.0.show(egui_aros::asl::Select::Drawer, "Choose a Drawer").into_iter().next()))
    }

    pub fn pick_folders(self) -> Ready<Option<Vec<FileHandle>>> {
        let dirs = self.0.show(egui_aros::asl::Select::Drawer, "Choose a Drawer");
        Ready(Some((!dirs.is_empty()).then_some(dirs)))
    }

    pub fn save_file(self) -> Ready<Option<FileHandle>> {
        Ready(Some(self.0.show(egui_aros::asl::Select::Save, "Save As").into_iter().next()))
    }
}

/// rfd's blocking dialog.
#[derive(Clone, Debug)]
pub struct FileDialog(Setup);
builder!(FileDialog);

impl FileDialog {
    pub fn pick_file(self) -> Option<PathBuf> {
        self.0.show(egui_aros::asl::Select::Open, "Open").into_iter().next().map(PathBuf::from)
    }

    pub fn pick_files(self) -> Option<Vec<PathBuf>> {
        let files: Vec<PathBuf> = self.0.show(egui_aros::asl::Select::OpenMany, "Open").into_iter().map(PathBuf::from).collect();
        (!files.is_empty()).then_some(files)
    }

    pub fn pick_folder(self) -> Option<PathBuf> {
        self.0.show(egui_aros::asl::Select::Drawer, "Choose a Drawer").into_iter().next().map(PathBuf::from)
    }

    pub fn save_file(self) -> Option<PathBuf> {
        self.0.show(egui_aros::asl::Select::Save, "Save As").into_iter().next().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(list: &[&str]) -> (String, Vec<String>) {
        ("x".into(), list.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn one_pattern_for_every_filter() {
        assert_eq!(pattern(&[f(&["pdf"]), f(&["png", "jpg"])]), "#?.(pdf|png|jpg)");
        assert_eq!(pattern(&[f(&["PDF"]), f(&["pdf"])]), "#?.(pdf)");
    }

    #[test]
    fn any_case_globs_are_folded_back() {
        assert_eq!(pattern(&[f(&["[pP][dD][fF]", "[xX][fF][dD][fF]"])]), "#?.(pdf|xfdf)");
        assert_eq!(fold_any_case("[pP]7[mM]"), "p7m");
    }

    #[test]
    fn no_filter_or_a_star_lists_every_file() {
        assert_eq!(pattern(&[]), "");
        assert_eq!(pattern(&[f(&["pdf"]), f(&["*"])]), "");
        assert_eq!(pattern(&[f(&["*.pdf"])]), "#?.(pdf)");
    }

    #[test]
    fn a_pattern_too_long_for_asl_lists_every_file() {
        let many: Vec<String> = (0..80).map(|i| format!("ext{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(pattern(&[f(&many)]), "");
    }

    #[test]
    fn without_a_window_nothing_is_chosen() {
        assert_eq!(AsyncFileDialog::new().set_title("t").add_filter("PDF", &["pdf"]).set_file_name("a.pdf").pick_file().0, Some(None));
        assert_eq!(FileDialog::new().pick_files(), None);
    }
}
