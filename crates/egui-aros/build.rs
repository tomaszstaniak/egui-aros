//! Compiles `c/egui_aros.c` with the AROS C compiler on AROS targets. `AROS_CC`/`AROS_AR` name
//! the compiler and archiver; `AROS_SDK`, when set, is passed as `--sysroot`, so the C code sees
//! the same SDK headers as the final link (ABIv1 and ABIv11 inline library calls differently).
fn main() {
    println!("cargo:rerun-if-changed=c/egui_aros.c");
    for var in ["AROS_CC", "AROS_AR", "AROS_SDK"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("aros") {
        return;
    }
    let mut build = cc::Build::new();
    if let Ok(cc) = std::env::var("AROS_CC") {
        build.compiler(cc);
    }
    if let Ok(ar) = std::env::var("AROS_AR") {
        build.archiver(ar);
    }
    if let Ok(sdk) = std::env::var("AROS_SDK") {
        build.flag(format!("--sysroot={sdk}"));
    }
    // AROS executables are relocatable objects: no PIC, which cc adds by default.
    build.file("c/egui_aros.c").pic(false).compile("egui_aros_c");
}
