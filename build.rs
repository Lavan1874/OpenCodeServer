fn main() {
    // Declare the `ci` cfg flag so clippy/rustc accept #[cfg_attr(ci, ...)]
    // without an unexpected-cfgs warning. The flag is set via
    // RUSTFLAGS="--cfg ci" in the CI workflow; local builds never set it.
    println!("cargo::rustc-check-cfg=cfg(ci)");

    println!("cargo:rerun-if-changed=rust/platform/logging_bridge.c");
    cc::Build::new()
        .file("rust/platform/logging_bridge.c")
        .warnings(true)
        .compile("opencodeserver_logging");

    // The Xcode Run Script phase exports the parent bundle's
    // CURRENT_PROJECT_VERSION here so the OpenCodeServerAgent binary carries
    // its own build identity. OpenCodeServer compares this value against the
    // pending registration transaction before committing
    // `RegisteredBundleVersion`; "IPC is reachable" alone is not proof that
    // the new build is running. The value is baked in at compile time and is
    // not configurable at runtime. Empty values are ignored so plain
    // `cargo build` keeps working outside Xcode.
    println!("cargo:rerun-if-env-changed=OPENCODESERVER_BUNDLE_VERSION");
    if let Ok(version) = std::env::var("OPENCODESERVER_BUNDLE_VERSION") {
        let version = version.trim();
        if !version.is_empty() {
            println!("cargo:rustc-env=OPENCODESERVER_BUNDLE_VERSION={version}");
        }
    }
}
