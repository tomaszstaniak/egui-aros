//! Native checks of egui-aros' clipboard on AROS (clipboard.device unit 0). Each command prints
//! `PASS name` / `FAIL name: why` lines and ends with status 0 when every check passed, 10 when
//! one failed, 20 on bad usage. Every command can run again; it changes only the clipboard.
//!
//!   clip-probe charset             the system character set in use
//!   clip-probe text                text round trips (Polish, other scripts, combining marks),
//!                                  the CSET rule, a picture-free read of text
//!   clip-probe roundtrip           pictures written and read back pixel-exact (odd widths,
//!                                  not multiples of 16, a large one), reads leave the clipboard as is
//!   clip-probe errors              damaged, cut, oversized and zero-size pictures, text, and
//!                                  that each is reported, not read as pixels
//!   clip-probe put W H             write the probe pattern WxH (for MultiView / another program)
//!   clip-probe dump FILE           read the clipboard's picture and write it as raw RGBA with a
//!                                  `W H\n` header line to FILE (host-side pixel comparison)
//!   clip-probe expect W H          the clipboard holds the probe pattern WxH, pixel-exact
//!   clip-probe puttext TEXTFILE    put the UTF-8 text in TEXTFILE on the clipboard
//!   clip-probe gettext FILE        write the clipboard's text to FILE as UTF-8 (with its CSET)
//!   clip-probe putbad cut|huge|zero  leave a damaged picture on the clipboard (for paste tests)

use egui_aros::ClipboardImage;
use std::process::ExitCode;

struct Report {
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, result: Result<(), String>) {
        match result {
            Ok(()) => println!("PASS {name}"),
            Err(why) => {
                self.failed += 1;
                println!("FAIL {name}: {why}");
            }
        }
    }
}

/// The probe pattern: every channel varies, alpha 255 (a 24-bit ILBM keeps none).
fn pattern(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[(x * 7 + y) as u8, (y * 13 + x / 3) as u8, ((x * 31) ^ (y * 17)) as u8, 255]);
        }
    }
    v
}

fn read_pixels() -> Result<(u32, u32, Vec<u8>), String> {
    match egui_aros::clipboard_image() {
        ClipboardImage::Pixels { width, height, rgba } => Ok((width, height, rgba)),
        ClipboardImage::None => Err("no picture on the clipboard".into()),
        ClipboardImage::Unreadable(why) => Err(format!("unreadable: {why}")),
    }
}

fn compare(w: u32, h: u32, want: &[u8]) -> Result<(), String> {
    let (gw, gh, got) = read_pixels()?;
    if (gw, gh) != (w, h) {
        return Err(format!("size {gw}x{gh}, wrote {w}x{h}"));
    }
    if got.len() != want.len() {
        return Err(format!("{} bytes, expected {}", got.len(), want.len()));
    }
    // Compare RGB; alpha must be opaque (the ILBM has none).
    for (i, (g, e)) in got.chunks_exact(4).zip(want.chunks_exact(4)).enumerate() {
        if g[..3] != e[..3] || g[3] != 255 {
            return Err(format!("pixel {i} ({}, {}) is {g:?}, expected {e:?}", i as u32 % w, i as u32 / w));
        }
    }
    Ok(())
}

fn put(w: u32, h: u32) -> Result<(), String> {
    let px = pattern(w, h);
    egui_aros::set_clipboard_image(w, h, w as usize * 4, &px)
}

fn texts() -> Vec<(&'static str, &'static str)> {
    vec![
        ("polish", "Zażółć gęślą jaźń — ĄĆĘŁŃÓŚŹŻ ąćęłńóśźż"),
        ("polish-plain", "Zażółć gęślą jaźń ĄĆĘŁŃÓŚŹŻ ąćęłńóśźż"),
        ("greek-cyrillic", "Ωμέγα · Ёжик · Ελληνικά"),
        ("combining", "e\u{301} a\u{328} Z\u{307} n\u{303}"),
        ("cjk-emoji", "日本語 한국어 🙂"),
        ("tabs-lines", "a\tb\tc\n1\t2\t3\n"),
    ]
}

fn cmd_text(r: &mut Report) {
    let cs = egui_aros::system_charset();
    println!("INFO {}", cs.describe());
    for (name, text) in texts() {
        r.check(&format!("text-roundtrip-{name}"), (|| {
            if !egui_aros::set_clipboard_text(text) {
                return Err("write failed".into());
            }
            match egui_aros::clipboard_text() {
                Some(t) if t == text => Ok(()),
                Some(t) => Err(format!("read {t:?}")),
                None => Err("no text read back".into()),
            }
        })());
        // The CSET rule: none when the system set holds the text, 106 (UTF-8) otherwise.
        r.check(&format!("text-cset-{name}"), (|| {
            let (bytes, cset) = aros_gui_sys::clipboard::encode_ftxt(text, cs);
            let want = if cs.can_encode(text) && !cs.is_utf8() { 0 } else { 106 };
            if cset != want {
                return Err(format!("CSET {cset}, expected {want}"));
            }
            if cset == 0 && cs.decode(&bytes) != text {
                return Err("system-set bytes do not decode back".into());
            }
            Ok(())
        })());
    }
    r.check("text-is-not-a-picture", (|| {
        egui_aros::set_clipboard_text("plain text");
        match egui_aros::clipboard_image() {
            ClipboardImage::None => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })());
}

fn cmd_roundtrip(r: &mut Report) {
    for (w, h) in [(1, 1), (17, 9), (333, 77), (641, 3), (1023, 767)] {
        r.check(&format!("picture-{w}x{h}"), (|| {
            put(w, h)?;
            compare(w, h, &pattern(w, h))
        })());
    }
    r.check("picture-read-twice", (|| {
        put(101, 33)?;
        compare(101, 33, &pattern(101, 33))?;
        compare(101, 33, &pattern(101, 33))
    })());
    r.check("picture-text-read-is-none", (|| {
        put(15, 15)?;
        match egui_aros::clipboard_text() {
            None => Ok(()),
            Some(t) => Err(format!("text {t:?} from a picture")),
        }
    })());
    r.check("picture-large-2400x1600", (|| {
        put(2400, 1600)?;
        compare(2400, 1600, &pattern(2400, 1600))
    })());
}

fn bmhd(w: u16, h: u16, planes: u8, compression: u8) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[0..2].copy_from_slice(&w.to_be_bytes());
    b[2..4].copy_from_slice(&h.to_be_bytes());
    b[8] = planes;
    b[10] = compression;
    b[14] = 1;
    b[15] = 1;
    b[16..18].copy_from_slice(&w.to_be_bytes());
    b[18..20].copy_from_slice(&h.to_be_bytes());
    b
}

/// Not pixels: none or unreadable, and the program still runs.
fn not_pixels(name: &str) -> Result<(), String> {
    match egui_aros::clipboard_image() {
        ClipboardImage::Pixels { width, height, .. } => Err(format!("{name}: read as {width}x{height} pixels")),
        ClipboardImage::None => {
            println!("INFO {name}: none");
            Ok(())
        }
        ClipboardImage::Unreadable(why) => {
            println!("INFO {name}: unreadable: {why}");
            Ok(())
        }
    }
}

fn cmd_errors(r: &mut Report) {
    r.check("error-cut-body", (|| {
        let (head, body) = egui_aros::clipboard_image::encode_ilbm(64, 64, 256, &pattern(64, 64))?;
        egui_aros::clipboard_image::write_ilbm_payloads(&head, &body[..body.len() / 3])?;
        // A datatype may fill the missing rows; it must not crash and must give 64x64 if anything.
        match egui_aros::clipboard_image() {
            ClipboardImage::Pixels { width: 64, height: 64, .. } => {
                println!("INFO error-cut-body: read as 64x64 (missing rows filled by the datatype)");
                Ok(())
            }
            ClipboardImage::Pixels { width, height, .. } => Err(format!("read as {width}x{height}")),
            _ => not_pixels("error-cut-body"),
        }
    })());
    r.check("error-empty-body", (|| {
        egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(64, 64, 24, 1), &[])?;
        match egui_aros::clipboard_image() {
            ClipboardImage::Pixels { width: 64, height: 64, .. } | ClipboardImage::None | ClipboardImage::Unreadable(_) => Ok(()),
            ClipboardImage::Pixels { width, height, .. } => Err(format!("read as {width}x{height}")),
        }
    })());
    r.check("error-oversized-60000x60000", (|| {
        egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(60000, 60000, 24, 1), &[0x80; 64])?;
        not_pixels("error-oversized-60000x60000")
    })());
    r.check("error-over-limit-16385x2", (|| {
        egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(16385, 2, 24, 0), &vec![0; 2 * 24 * 2050])?;
        not_pixels("error-over-limit-16385x2")
    })());
    r.check("error-zero-size", (|| {
        egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(0, 0, 24, 1), &[])?;
        not_pixels("error-zero-size")
    })());
    r.check("error-bad-byterun", (|| {
        // Runs that claim far more bytes than a row holds.
        egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(16, 4, 24, 1), &[0x81, 0xAA].repeat(400))?;
        match egui_aros::clipboard_image() {
            ClipboardImage::Pixels { width: 16, height: 4, .. } | ClipboardImage::None | ClipboardImage::Unreadable(_) => Ok(()),
            ClipboardImage::Pixels { width, height, .. } => Err(format!("read as {width}x{height}")),
        }
    })());
    r.check("error-writer-refuses-bad-input", (|| {
        let refused = [
            egui_aros::set_clipboard_image(0, 1, 0, &[]).is_err(),
            egui_aros::set_clipboard_image(16385, 1, 65540, &vec![0; 65540]).is_err(),
            egui_aros::set_clipboard_image(4, 4, 16, &[0; 10]).is_err(),
        ];
        if refused.iter().all(|&x| x) { Ok(()) } else { Err(format!("{refused:?}")) }
    })());
    r.check("error-text-clipboard", (|| {
        egui_aros::set_clipboard_text("Zażółć");
        not_pixels("error-text-clipboard")
    })());
    // Last: the clipboard still works after all of the above.
    r.check("error-recovery", (|| {
        put(33, 11)?;
        compare(33, 11, &pattern(33, 11))
    })());
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut r = Report { failed: 0 };
    let num = |i: usize| args.get(i).and_then(|v| v.parse::<u32>().ok());
    match args.first().map(String::as_str) {
        Some("charset") => println!("INFO {}", egui_aros::system_charset().describe()),
        Some("text") => cmd_text(&mut r),
        Some("roundtrip") => cmd_roundtrip(&mut r),
        Some("errors") => cmd_errors(&mut r),
        Some("put") => match (num(1), num(2)) {
            (Some(w), Some(h)) => r.check(&format!("put-{w}x{h}"), put(w, h)),
            _ => return ExitCode::from(20),
        },
        Some("expect") => match (num(1), num(2)) {
            (Some(w), Some(h)) => r.check(&format!("expect-{w}x{h}"), compare(w, h, &pattern(w, h))),
            _ => return ExitCode::from(20),
        },
        Some("dump") if args.len() == 2 => r.check("dump", (|| {
            let (w, h, px) = read_pixels()?;
            let mut out = format!("{w} {h}\n").into_bytes();
            out.extend_from_slice(&px);
            std::fs::write(&args[1], out).map_err(|e| e.to_string())?;
            println!("INFO dump {w}x{h} to {}", args[1]);
            Ok(())
        })()),
        Some("puttext") if args.len() == 2 => r.check("puttext", (|| {
            let text = std::fs::read_to_string(&args[1]).map_err(|e| e.to_string())?;
            if egui_aros::set_clipboard_text(&text) { Ok(()) } else { Err("write failed".into()) }
        })()),
        Some("putbad") if args.len() == 2 => r.check("putbad", (|| match args[1].as_str() {
            "cut" => {
                let (head, body) = egui_aros::clipboard_image::encode_ilbm(64, 64, 256, &pattern(64, 64))?;
                egui_aros::clipboard_image::write_ilbm_payloads(&head, &body[..body.len() / 3])
            }
            "huge" => egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(60000, 60000, 24, 1), &[0x80; 64]),
            "zero" => egui_aros::clipboard_image::write_ilbm_payloads(&bmhd(0, 0, 24, 1), &[]),
            other => Err(format!("unknown kind {other}")),
        })()),
        Some("gettext") if args.len() == 2 => r.check("gettext", (|| {
            let text = egui_aros::clipboard_text().ok_or("no text on the clipboard")?;
            std::fs::write(&args[1], text.as_bytes()).map_err(|e| e.to_string())
        })()),
        _ => {
            eprintln!("usage: clip-probe charset|text|roundtrip|errors|put W H|expect W H|dump FILE|puttext FILE|gettext FILE");
            return ExitCode::from(20);
        }
    }
    println!("DONE failed={}", r.failed);
    if r.failed == 0 { ExitCode::SUCCESS } else { ExitCode::from(10) }
}
