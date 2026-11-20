// Builds the C rapidyenc static library when the `rapidyenc` feature is on.
// RAPIDYENC_DIR must point at a checkout of github.com/animetosho/rapidyenc.
fn main() {
    println!("cargo::rerun-if-env-changed=RAPIDYENC_DIR");
    #[cfg(feature = "rapidyenc")]
    {
        let dir = std::env::var("RAPIDYENC_DIR").expect("set RAPIDYENC_DIR");
        let dst = cmake::Config::new(dir)
            .profile("Release")
            .define("DISABLE_SHARED", "ON")
            .define("DISABLE_TOOL", "ON")
            .build_target("rapidyenc_static")
            .build();
        println!("cargo::rustc-link-search=native={}", dst.join("build/rapidyenc_static").display());
        println!("cargo::rustc-link-lib=static=rapidyenc");
        println!("cargo::rustc-link-lib=dylib=c++");
    }
}
