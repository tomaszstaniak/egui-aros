fn main() {
    println!("cargo:rerun-if-changed=c/sigalloc.c");
    println!("cargo:rerun-if-env-changed=AROS_CC");
    println!("cargo:rerun-if-env-changed=AROS_SDK");
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
    build.file("c/sigalloc.c").pic(false).compile("ea_sigalloc");
}
