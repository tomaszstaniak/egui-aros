//! The interface renderer a program is started with: CPU, AUTO or GPU, from (highest first) the
//! Shell argument `--gui-renderer`, the Workbench ToolType `GUI_RENDERER`, the environment
//! variable `GUI_RENDERER` and the program's default. No platform calls here, so the rules are
//! tested on the host. Taken from the VectorCraft AROS port, shared by the Craft programs.

use crate::RendererRequest;

/// Where the choice came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Argument,
    ToolType,
    Environment,
    Default,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Self::Argument => "argument",
            Self::ToolType => "ToolType",
            Self::Environment => "environment variable",
            Self::Default => "default",
        }
    }
}

/// The renderer requested and where that came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererChoice {
    pub request: RendererRequest,
    pub source: Source,
}

/// The ToolType key (and environment variable) for the renderer.
pub const KEY: &str = "GUI_RENDERER";

/// Lines for a program's `--help`.
pub const HELP: &str = "\
  --gui-renderer CPU   draw the interface with the processor; OpenGL is not opened
  --gui-renderer AUTO  use hardware OpenGL when there is a context on it, else the processor
                       (the reason is logged; a software rasterizer counts as none)
  --gui-renderer GPU   use OpenGL or stop with an error
Workbench: ToolType GUI_RENDERER=CPU|AUTO|GPU. A Shell argument wins over the ToolType, which
wins over the GUI_RENDERER environment variable.";

pub fn parse_request(value: &str, key: &str) -> Result<RendererRequest, String> {
    match value.trim().to_ascii_uppercase().as_str() {
        "CPU" => Ok(RendererRequest::Cpu),
        "AUTO" => Ok(RendererRequest::Auto),
        "GPU" => Ok(RendererRequest::Gpu),
        _ => Err(format!("{key}: unknown value \"{}\"; expected CPU, AUTO or GPU", value.trim())),
    }
}

pub fn request_name(r: RendererRequest) -> &'static str {
    match r {
        RendererRequest::Cpu => "CPU",
        RendererRequest::Auto => "AUTO",
        RendererRequest::Gpu => "GPU",
    }
}

/// Takes `--gui-renderer X` / `--gui-renderer=X` out of `args` (the program parses the rest) and
/// decides. `tooltypes` are `KEY=value` entries (a Workbench start), `env` the variable's value.
/// Only the value in force is parsed, so a stale ToolType cannot stop a start whose argument
/// overrides it. Arguments after `--` are left alone.
pub fn choose(args: &mut Vec<String>, tooltypes: &[String], env: Option<&str>, default: RendererRequest) -> Result<RendererChoice, String> {
    let mut from_arg = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            break;
        }
        if args[i] == "--gui-renderer" {
            let value = args.get(i + 1).cloned().ok_or("--gui-renderer: missing CPU, AUTO or GPU")?;
            from_arg = Some(parse_request(&value, "--gui-renderer")?);
            args.drain(i..i + 2);
            continue;
        }
        if let Some(value) = args[i].strip_prefix("--gui-renderer=") {
            from_arg = Some(parse_request(value, "--gui-renderer")?);
            args.remove(i);
            continue;
        }
        i += 1;
    }
    if let Some(request) = from_arg {
        return Ok(RendererChoice { request, source: Source::Argument });
    }
    let mut from_tooltype = None;
    for entry in tooltypes {
        if let Some((key, value)) = entry.split_once('=') {
            if key.trim().eq_ignore_ascii_case(KEY) {
                from_tooltype = Some(value.to_owned());
            }
        }
    }
    if let Some(value) = from_tooltype {
        return Ok(RendererChoice { request: parse_request(&value, KEY)?, source: Source::ToolType });
    }
    if let Some(value) = env.filter(|v| !v.trim().is_empty()) {
        return Ok(RendererChoice { request: parse_request(value, KEY)?, source: Source::Environment });
    }
    Ok(RendererChoice { request: default, source: Source::Default })
}

/// Why AUTO does not try OpenGL on this system at all, if it does not. On the Raspberry Pi
/// OpenGL is unverified on hardware, and under emulation (Mesa's software rasterizer, no
/// VideoCore) it stopped the whole system (VectorCraft, 2026-10-28); GPU, asked for by name,
/// still tries it.
pub const AUTO_SKIPS_GL: Option<&str> = if cfg!(target_arch = "aarch64") {
    Some("OpenGL on the Raspberry Pi is not verified on Pi hardware; choose GPU to try it")
} else {
    None
};

/// The request to pass to [`crate::run`] for `choice`, and the reason when AUTO is narrowed to
/// the CPU before any OpenGL is touched.
pub fn effective(choice: RendererChoice) -> (RendererRequest, Option<&'static str>) {
    match (choice.request, AUTO_SKIPS_GL) {
        (RendererRequest::Auto, Some(reason)) => (RendererRequest::Cpu, Some(reason)),
        (r, _) => (r, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RendererRequest::{Auto, Cpu, Gpu};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn precedence_argument_tooltype_environment_default() {
        let mut a = s(&["--gui-renderer", "auto", "Work:a b.docx"]);
        let c = choose(&mut a, &s(&["GUI_RENDERER=bogus"]), Some("gpu"), Cpu).unwrap();
        assert_eq!((c.request, c.source), (Auto, Source::Argument));
        assert_eq!(a, s(&["Work:a b.docx"]));
        let c = choose(&mut s(&[]), &s(&["(GUI_RENDERER=GPU)", "gui_renderer=Cpu"]), Some("gpu"), Auto).unwrap();
        assert_eq!((c.request, c.source), (Cpu, Source::ToolType));
        let c = choose(&mut s(&[]), &[], Some(" Gpu "), Cpu).unwrap();
        assert_eq!((c.request, c.source), (Gpu, Source::Environment));
        let c = choose(&mut s(&[]), &[], Some(""), Auto).unwrap();
        assert_eq!((c.request, c.source), (Auto, Source::Default));
    }

    #[test]
    fn inline_form_and_end_of_options() {
        let mut a = s(&["x.docx", "--gui-renderer=gPu", "--", "--gui-renderer=CPU"]);
        let c = choose(&mut a, &[], None, Cpu).unwrap();
        assert_eq!(c.request, Gpu);
        assert_eq!(a, s(&["x.docx", "--", "--gui-renderer=CPU"]));
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(choose(&mut s(&["--gui-renderer", "fast"]), &[], None, Cpu).is_err());
        assert!(choose(&mut s(&["--gui-renderer"]), &[], None, Cpu).is_err());
        assert!(choose(&mut s(&[]), &s(&["GUI_RENDERER=fast"]), None, Cpu).is_err());
        assert!(choose(&mut s(&[]), &[], Some("fast"), Cpu).is_err());
    }

    #[test]
    fn auto_narrowing_follows_the_architecture() {
        let auto = RendererChoice { request: Auto, source: Source::Default };
        let (r, why) = effective(auto);
        if cfg!(target_arch = "aarch64") {
            assert_eq!(r, Cpu);
            assert!(why.is_some());
        } else {
            assert_eq!((r, why), (Auto, None));
        }
        assert_eq!(effective(RendererChoice { request: Gpu, source: Source::Argument }).0, Gpu);
    }
}
