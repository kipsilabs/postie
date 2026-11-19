// Builds the vendored rapidyenc C++ library (encoder only) with cmake and links it statically.
fn main() {
    println!("cargo::rerun-if-changed=../vendor/rapidyenc");
    let dst = cmake::Config::new("../vendor/rapidyenc")
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
