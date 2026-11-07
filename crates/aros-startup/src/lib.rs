//! Program start on AROS (no GUI dependency, so command-line programs use it too): Workbench start data (ToolTypes, documents), the program's drawer, DOS
//! names as -nix paths, and DOS requesters off. From the VectorCraft AROS port, shared by the
//! Craft programs. On other systems every function is a plain fallback, so programs build and
//! their tests run on the host.
use std::path::PathBuf;

#[cfg(target_os = "aros")]
mod native {
    use std::ffi::{CString, c_char};
    unsafe extern "C" {
        pub fn ea_startup_workbench() -> i32;
        pub fn ea_startup_quiet_dos();
        pub fn ea_startup_wb_file(index: i32, out: *mut c_char, cap: i32) -> i32;
        pub fn ea_startup_tooltype(key: *const c_char, out: *mut c_char, cap: i32) -> i32;
        pub fn ea_startup_progdir(out: *mut c_char, cap: i32) -> i32;
        pub fn ea_startup_absolute(name: *const c_char, out: *mut c_char, cap: i32) -> i32;
        pub fn ea_startup_error(title: *const c_char, message: *const c_char);
        pub fn ea_startup_run_with_stack(size: std::ffi::c_ulong, f: extern "C" fn(*mut std::ffi::c_void), arg: *mut std::ffi::c_void) -> i32;
    }
    pub fn cstr(s: &str) -> Result<CString, String> {
        CString::new(s).map_err(|_| "path or setting contains NUL".into())
    }
    pub fn read(call: impl FnOnce(*mut c_char, i32) -> i32) -> Result<Option<String>, String> {
        let mut bytes = vec![0u8; 8192];
        let result = call(bytes.as_mut_ptr().cast(), bytes.len() as i32);
        if result == 0 {
            return Ok(None);
        }
        if result < 0 {
            return Err("AROS could not resolve the startup data or the path (missing drawer, unreadable icon, or path too long)".into());
        }
        let end = bytes.iter().position(|&b| b == 0).ok_or("unterminated DOS result")?;
        bytes.truncate(end);
        String::from_utf8(bytes).map(Some).map_err(|e| {
            // Names are bytes on AROS (often ISO 8859-1); show the name so the user knows which.
            let name: String = e.into_bytes().iter().map(|&b| b as char).collect();
            format!("the name \"{name}\" is not UTF-8 text; this program needs UTF-8 file and drawer names")
        })
    }
}

/// Make DOS answer a missing volume with an error instead of a requester, for this process and
/// the threads it starts. Call first thing in `main`.
pub fn quiet_dos() {
    #[cfg(target_os = "aros")]
    // SAFETY: writes the calling process's own pr_WindowPtr on its own task.
    unsafe {
        native::ea_startup_quiet_dos();
    }
}

/// Runs `f` on a stack of at least `bytes`, on this task, and returns its result. An AROS program
/// starts on its caller's stack (40,960 bytes in an AROS One Shell, unless `Stack` raised it), too
/// small for a GUI's main loop; a Workbench start uses the icon's stack size. When the current
/// stack is large enough `f` runs directly, else on a new stack through exec's `NewStackSwap`
/// (the same task, so windows and signals stay where they are). A panic in `f` is caught on the
/// new stack and resumed after the switch back. If the stack cannot be allocated, `f` runs on
/// the current one and a warning is printed.
pub fn run_with_stack<R, F: FnOnce() -> R>(bytes: usize, f: F) -> R {
    #[cfg(target_os = "aros")]
    {
        struct Call<F, R> {
            f: Option<F>,
            out: Option<std::thread::Result<R>>,
        }
        extern "C" fn trampoline<F: FnOnce() -> R, R>(arg: *mut std::ffi::c_void) {
            // SAFETY: `arg` is the `Call` below, alive and not otherwise used during the call.
            let call = unsafe { &mut *(arg as *mut Call<F, R>) };
            if let Some(f) = call.f.take() {
                call.out = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
            }
        }
        let mut call: Call<F, R> = Call { f: Some(f), out: None };
        // SAFETY: the trampoline matches the C signature and gets a pointer to `call`, which
        // outlives the call; the C side runs it once, synchronously.
        let rc = unsafe {
            native::ea_startup_run_with_stack(bytes as std::ffi::c_ulong, trampoline::<F, R>, &mut call as *mut Call<F, R> as *mut std::ffi::c_void)
        };
        if rc != 0 {
            eprintln!("no memory for a {bytes}-byte stack; running on the current stack");
        }
        match call.out {
            Some(Ok(r)) => return r,
            Some(Err(panic)) => std::panic::resume_unwind(panic),
            None => {}
        }
        // Not run (no memory for the stack): run here.
        return (call.f.take().expect("not run"))();
    }
    #[cfg(not(target_os = "aros"))]
    {
        let _ = bytes;
        f()
    }
}

/// Sets the environment variable `name` to `value` unless it is set already (a program's default
/// settings folder, say, that the user may override). Call in `main` before any other thread
/// starts.
pub fn set_default_env(name: &str, value: &std::path::Path) {
    if std::env::var_os(name).is_none() {
        // SAFETY: called on the main thread before the program starts any other thread, so no
        // thread reads the environment concurrently.
        unsafe { std::env::set_var(name, value) };
    }
}

#[derive(Default, Debug)]
pub struct Startup {
    pub workbench: bool,
    pub tooltypes: Vec<String>,
    pub files: Vec<String>,
}

/// Workbench start data: the requested ToolTypes (`KEY=value`) and the documents passed with the
/// program, as -nix paths. A Shell start returns the default (no ToolTypes, no files).
pub fn startup(tooltype_keys: &[&str]) -> Result<Startup, String> {
    #[cfg(target_os = "aros")]
    {
        // SAFETY: read-only CRT global, while main still owns the startup lifetime.
        if unsafe { native::ea_startup_workbench() } == 0 {
            return Ok(Startup::default());
        }
        let mut out = Startup { workbench: true, ..Startup::default() };
        for key in tooltype_keys {
            let c = native::cstr(key)?;
            // SAFETY: valid strings and owned writable output, copied before DiskObject is freed.
            if let Some(value) = native::read(|p, n| unsafe { native::ea_startup_tooltype(c.as_ptr(), p, n) })? {
                out.tooltypes.push(format!("{key}={value}"));
            }
        }
        for i in 1..1024 {
            // SAFETY: C checks index against the borrowed WBStartup argument array.
            match native::read(|p, n| unsafe { native::ea_startup_wb_file(i, p, n) })? {
                Some(path) => out.files.push(dos_to_nix(&path)?),
                None => return Ok(out),
            }
        }
        Err("too many Workbench documents".into())
    }
    #[cfg(not(target_os = "aros"))]
    {
        let _ = tooltype_keys;
        Ok(Startup::default())
    }
}

/// The program's drawer as an absolute -nix path (`/Volume/Drawer`).
pub fn program_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "aros")]
    {
        // SAFETY: owned writable output buffer.
        native::read(|p, n| unsafe { native::ea_startup_progdir(p, n) })?
            .ok_or_else(|| "the program has no drawer".to_string())
            .and_then(|p| dos_to_nix(&p))
            .map(PathBuf::from)
    }
    #[cfg(not(target_os = "aros"))]
    std::env::current_exe().map_err(|e| e.to_string())?.parent().map(PathBuf::from).ok_or_else(|| "the program has no folder".into())
}

/// A name as typed (AmigaDOS: volume, assign, relative, parent `/`) resolved by DOS to an
/// absolute -nix path. For a name that does not exist yet its drawer must exist.
pub fn absolute(name: &str) -> Result<PathBuf, String> {
    #[cfg(target_os = "aros")]
    {
        let c = native::cstr(name)?;
        // SAFETY: input outlives call; DOS receives an owned writable buffer.
        native::read(|p, n| unsafe { native::ea_startup_absolute(c.as_ptr(), p, n) })?
            .ok_or_else(|| "path was not resolved".to_string())
            .and_then(|p| dos_to_nix(&p))
            .map(PathBuf::from)
    }
    #[cfg(not(target_os = "aros"))]
    std::path::absolute(name).map_err(|e| e.to_string())
}

/// Reports a startup error: standard error, and on AROS a requester titled `title` (a Workbench
/// start has no console to show it).
pub fn show_error(title: &str, message: &str) {
    eprintln!("{title}: {message}");
    #[cfg(target_os = "aros")]
    {
        let bytes = |s: &str| {
            let mut v = aros_gui_sys::charset::system().encode_display(s);
            v.push(0);
            v
        };
        let (t, m) = (bytes(title), bytes(message));
        // SAFETY: NUL-terminated strings; the requester completes before they are freed.
        unsafe { native::ea_startup_error(t.as_ptr().cast(), m.as_ptr().cast()) };
    }
}

/// A resolved DOS name (`Volume:rest`) in the -nix path mode the program runs in. No lexical
/// parent collapsing: Lock/NameFromLock have already resolved it.
pub fn dos_to_nix(path: &str) -> Result<String, String> {
    let (volume, rest) = path.split_once(':').ok_or("DOS did not return a volume-qualified path")?;
    if volume.is_empty() || volume.contains('/') {
        return Err("invalid DOS volume name".into());
    }
    Ok(if rest.is_empty() { format!("/{volume}") } else { format!("/{volume}/{rest}") })
}

/// A normalized -nix absolute path as a DOS name, for requesters and messages.
pub fn nix_to_dos(path: &str) -> Result<String, String> {
    let rest = path.strip_prefix('/').ok_or("expected a normalized -nix absolute path")?;
    let (volume, rest) = rest.split_once('/').unwrap_or((rest, ""));
    if volume.is_empty() {
        return Err("missing volume in -nix path".into());
    }
    Ok(format!("{volume}:{rest}"))
}

#[cfg(test)]
mod tests {
    use super::{dos_to_nix, nix_to_dos};
    #[test]
    fn run_with_stack_returns_and_resumes_panics() {
        assert_eq!(super::run_with_stack(1 << 20, || 41 + 1), 42);
        let r = std::panic::catch_unwind(|| super::run_with_stack(1 << 20, || panic!("inside")));
        assert!(r.is_err());
    }

    #[test]
    fn normalized_paths_round_trip() {
        for p in ["RAM:", "Work:My Pictures/zażółć.svg", "Ram Disk:a.svg"] {
            assert_eq!(nix_to_dos(&dos_to_nix(p).unwrap()).unwrap(), p);
        }
        assert!(dos_to_nix(":bad").is_err());
        assert!(dos_to_nix("no-volume").is_err());
    }
}
