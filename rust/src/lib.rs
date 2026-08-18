#![deny(unsafe_op_in_unsafe_fn)]

pub mod config;
pub mod config_fingerprint;
pub mod credential_grant;
#[cfg(all(feature = "diagnostic-local-network", target_os = "macos"))]
pub mod diagnostic_local_network;
pub mod health;
pub mod ipc;
pub mod keychain;
pub mod paths;
pub mod platform;
mod private_file;
pub mod process;
mod process_cleanup;
mod process_group;
pub mod protocol;
pub mod runtime_state;
pub mod supervisor;
#[cfg(any(test, feature = "test-fixture"))]
pub mod test_events;
mod version_cleanup;
mod version_query;

pub const PRODUCT_NAME: &str = "OpenCodeServer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The parent app bundle's `CFBundleVersion`, baked into the
/// OpenCodeServerAgent binary by the Xcode Run Script phase (see
/// `build.rs`). Unlike the user-controlled configuration, this identity
/// cannot be forged without rebuilding the signed product, so OpenCodeServer
/// uses it to prove that a registration transaction actually runs the new
/// build before committing `RegisteredBundleVersion`. Standalone development
/// builds use a distinct, explicit identity.
pub const BUNDLE_VERSION: &str = match option_env!("OPENCODESERVER_BUNDLE_VERSION") {
    Some(version) => version,
    None => "development",
};
