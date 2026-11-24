// Builds the vendored rapidyenc C++ library (encoder only) with cmake and links it statically.
fn main() {
    println!("cargo::rerun-if-changed=../vendor/rapidyenc");
    // rapidyenc picks its SIMD sources from TARGET_ARCH, falling back to CMAKE_SYSTEM_PROCESSOR,
    // which reflects the *host process*: a cmake launched under Rosetta reports x86_64 and adds
    // `-msse2` to an arm64 build. Derive it from the Rust target instead (also right when
    // cross-compiling).
    let target_arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "i686".to_string(),
        Ok(arch) => arch.to_string(),
        Err(_) => String::new(),
    };
    let mut cfg = cmake::Config::new("../vendor/rapidyenc");
    if !target_arch.is_empty() {
        cfg.define("TARGET_ARCH", &target_arch);
    }
    let dst = cfg
        .profile("Release")
        .define("DISABLE_SHARED", "ON")
        .define("DISABLE_TOOL", "ON")
        .define("DISABLE_DECODE", "ON")
        .define("DISABLE_CRC", "ON")
        .build_target("rapidyenc_static")
        .build();
    println!(
        "cargo::rustc-link-search=native={}",
        dst.join("build/rapidyenc_static").display()
    );
    // multi-config generators (MSVC) put the library in a per-profile subdirectory
    println!(
        "cargo::rustc-link-search=native={}",
        dst.join("build/rapidyenc_static/Release").display()
    );
    println!("cargo::rustc-link-lib=static=rapidyenc");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        return;
    }
    let cxx = if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        "c++"
    } else {
        "stdc++"
    };
    println!("cargo::rustc-link-lib=dylib={cxx}");
}
