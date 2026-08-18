//! Keychain credential storage for the OpenCode password (ADR 0016).
//!
//! The password that authenticates OpenCodeServerAgent health checks lives in
//! the login keychain as a Generic Password item, not in `config.plist`:
//! `service` is fixed ([`KEYCHAIN_SERVICE`]), `account` is the configured
//! OpenCode username. OpenCodeServer owns the item (creates/updates/deletes
//! it); OpenCodeServerAgent only reads it.
//!
//! Sharing works through the item's ACL: the first decrypt by
//! OpenCodeServerAgent triggers the system consent dialog and the user picks
//! "Always Allow" for a permanent grant. That prompt is only ever triggered
//! from the explicit "Allow Keychain Access…" action in the Settings window.
//!
//! ## Why routine reads must be attribute-only
//!
//! On macOS 26 the legacy-keychain consent dialog CANNOT be suppressed. Both
//! `kSecUseAuthenticationUISkip`/`UIFail` and the officially recommended
//! `kSecUseAuthenticationContext` + `LAContext.interactionNotAllowed` were
//! measured on macOS 26 to raise the dialog anyway for any decrypt-class
//! read by an untrusted binary (the `SecItem.h` note on
//! `kSecUseNoAuthenticationUI` spells out the platform direction: "Legacy
//! keychain items will still activate UI if needed"). Therefore this module
//! offers two strictly separate operations:
//!
//! - [`probe_item`] — an attribute-only query (`kSecReturnAttributes`, never
//!   `kSecReturnData`). Attribute access is not ACL-gated, so the call always
//!   returns immediately and can never raise UI. This is the ONLY operation
//!   routine code paths (config load, kqueue reload, periodic recheck) may
//!   use.
//! - [`read_password`] — a decrypt-class read. If this binary is not in the
//!   item ACL, macOS 26 WILL show the consent dialog and block until the user
//!   answers. Callers must restrict it to (a) the Settings "Allow Keychain Access…"
//!   flow, where the dialog is expected, or (b) background paths gated by the
//!   persisted grant marker (`credential_grant`), which proves a decrypt
//!   already succeeded for the same account.
//!
//! Per TN3137, `kSecAttrAccessible*` values are effectively a no-op on the
//! file-based login keychain (items behave as `AfterFirstUnlock`). We still
//! write `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` at creation time
//! (on the OpenCodeServer side) as documented intent for a future
//! data-protection keychain migration. No `kSecAttrSynchronizable` and no
//! `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` are ever set: both would
//! route the item to the data-protection keychain, which a launchd-started
//! background process cannot read (errSecMissingEntitlement -34018).

use std::fmt;

#[cfg(target_os = "macos")]
use security_framework::base::Error;
#[cfg(target_os = "macos")]
use security_framework::passwords::{PasswordOptions, generic_password};
#[cfg(target_os = "macos")]
use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound};

/// `kSecAttrService` value for the OpenCode credential. Shared with
/// OpenCodeServer's `KeychainStore`; both sides must stay in sync.
pub const KEYCHAIN_SERVICE: &str = "ai.opencode.server";

/// Not exported by security-framework-sys 3.x. Returned by
/// `SecItemCopyMatching` when a keychain read needs UI and the query forbade
/// interaction, or when a non-UI session cannot interact. See
/// `Security/SecBase.h`.
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

/// Not exported by security-framework-sys 3.x. The user cancelled an
/// authorization prompt, or keychain access was denied by policy.
/// See `Security/SecBase.h`.
#[cfg(target_os = "macos")]
const ERR_SEC_USER_CANCELED: i32 = -128;

/// Not exported by security-framework-sys 3.x. Missing keychain-access-group
/// entitlement; reachable on a file-based keychain after the calling binary
/// changed (e.g. an update), per TN3137. Treated as a soft
/// "access pending" state, never as proof the item is gone.
#[cfg(target_os = "macos")]
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;

/// The outcome of probing for the OpenCode password item without decrypting
/// it. Always non-interactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainProbe {
    /// An item exists for this service/account. Says nothing about whether
    /// this process may decrypt it.
    Exists,
    /// `errSecItemNotFound` (-25300): no item for this service/account.
    /// This is the ONLY state that means "no password configured".
    NotConfigured,
    /// Any other OSStatus: an unexpected keychain failure, reported verbatim.
    Failed(i32),
}

impl fmt::Display for KeychainProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeychainProbe::Exists => write!(f, "item exists"),
            KeychainProbe::NotConfigured => write!(f, "not configured"),
            KeychainProbe::Failed(code) => write!(f, "keychain probe failed (OSStatus {code})"),
        }
    }
}

/// The outcome of reading the OpenCode password from the login keychain.
#[derive(Clone, PartialEq, Eq)]
pub enum KeychainRead {
    /// The credential exists and was readable by this process.
    Found(String),
    /// `errSecItemNotFound` (-25300): no item for this service/account.
    /// This is the ONLY state that means "no password configured"; the
    /// agent must not infer that from access-denied errors (those can be
    /// returned even though the item exists and a grant is pending or was
    /// revoked).
    NotConfigured,
    /// The item exists (or the system refused to say) but this process may
    /// not read it right now: authorization not yet granted (-25308),
    /// denied or prompt cancelled (-128 / errSecAuthFailed -25293), or a
    /// keychain-access failure after a binary change (-34018). Soft state:
    /// the user can grant access from Settings; the agent must never delete
    /// or report "not configured" because of it.
    AccessPending,
    /// Any other OSStatus: an unexpected keychain failure, reported verbatim.
    Failed(i32),
}

// Hand-written so a `{:?}` formatting (including an assert_eq! failure
// message) can never render the decrypted password carried by `Found`.
impl fmt::Debug for KeychainRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeychainRead::Found(_) => f.write_str("KeychainRead::Found(<redacted>)"),
            KeychainRead::NotConfigured => f.write_str("KeychainRead::NotConfigured"),
            KeychainRead::AccessPending => f.write_str("KeychainRead::AccessPending"),
            KeychainRead::Failed(code) => write!(f, "KeychainRead::Failed({code})"),
        }
    }
}

impl fmt::Display for KeychainRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeychainRead::Found(_) => write!(f, "configured"),
            KeychainRead::NotConfigured => write!(f, "not configured"),
            KeychainRead::AccessPending => write!(f, "access pending authorization"),
            KeychainRead::Failed(code) => write!(f, "keychain read failed (OSStatus {code})"),
        }
    }
}

/// Probes whether a Generic Password item exists for `account` under
/// [`KEYCHAIN_SERVICE`] WITHOUT decrypting it.
///
/// The query asks for attributes only (`kSecReturnAttributes`), which is not
/// ACL-gated: it never raises the consent dialog and never blocks, making it
/// safe for routine background use. `Exists` does not imply the item is
/// decryptable by this process.
pub fn probe_item(account: &str) -> KeychainProbe {
    // Test hook: see `read_password`. Fixture builds derive the probe result
    // from the same environment override.
    #[cfg(any(test, feature = "test-fixture"))]
    if let Ok(password) = std::env::var("OPENCODESERVER_TEST_PASSWORD") {
        return if password.is_empty() {
            KeychainProbe::NotConfigured
        } else {
            KeychainProbe::Exists
        };
    }
    probe_item_platform(account)
}

/// Reads (decrypts) the Generic Password for `account` under
/// [`KEYCHAIN_SERVICE`].
///
/// On macOS 26 this call raises the system consent dialog whenever the
/// calling binary is not already trusted by the item ACL, and it blocks
/// until the user answers. It must therefore only run from the Settings
/// "Allow Keychain Access…" flow or from background paths gated by the persisted
/// grant marker. There is deliberately no `allow_ui` parameter: no query key
/// suppresses the dialog on macOS 26, so pretending otherwise only invites
/// the prompt-storm regression this module documents.
///
/// Never logs the returned password; the secret stays in process memory and
/// is only passed to the OpenCode child via its documented environment
/// variables.
pub fn read_password(account: &str) -> KeychainRead {
    // Test hook: integration tests run ad-hoc signed helper binaries whose
    // Keychain access would trigger authorization prompts, so fixture builds
    // take the credential from the environment instead. This branch never
    // compiles into production builds (no `test-fixture` feature there).
    #[cfg(any(test, feature = "test-fixture"))]
    if let Ok(password) = std::env::var("OPENCODESERVER_TEST_PASSWORD") {
        return if password.is_empty() {
            KeychainRead::NotConfigured
        } else {
            KeychainRead::Found(password)
        };
    }
    read_password_platform(account)
}

/// The signing team identifier of THIS running binary, read from its own code
/// signature at runtime — `SecCodeCopySelf` + `SecCodeCopySigningInformation`
/// (`kSecCodeInfoTeamIdentifier`). This is the ground truth: nothing is baked
/// in at build time, so an atomically replaced binary cannot inherit a stale
/// team claim. Returns `None` for unsigned or ad hoc-signed builds (no team),
/// which disables the team-anchored automatic credential reads: development
/// and fixture builds keep the explicit "Allow Keychain Access…" click.
///
/// The only consumer is the credential grant marker logic (ADR 0016,
/// 2026-08-17 amendment): a marker whose recorded team matches this value
/// authorizes one bounded automatic silent re-read. The safe
/// `security-framework` crate (3.7) exposes no SecCode API and
/// `security-framework-sys` 2.17's `code_signing` module stops at
/// `SecCodeCopySelf`, so the remaining two symbols are declared locally from
/// Apple's public <Security/SecCode.h>, in the same style as the `ERR_SEC_*`
/// constants above.
pub fn signing_team_identifier() -> Option<String> {
    signing_team_identifier_platform()
}

#[cfg(target_os = "macos")]
fn signing_team_identifier_platform() -> Option<String> {
    use core_foundation::base::{CFRelease, CFType, CFTypeRef, OSStatus, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use security_framework_sys::base::errSecSuccess;
    use security_framework_sys::code_signing::{SecCSFlags, SecCodeCopySelf, SecCodeRef};

    // Not exported by security-framework-sys 2.17; declared from Apple's
    // public <Security/SecCode.h>. Both are available since macOS 10.7,
    // long before the deployment target.
    unsafe extern "C" {
        fn SecCodeCopySigningInformation(
            code: SecCodeRef,
            flags: SecCSFlags,
            information: *mut CFDictionaryRef,
        ) -> OSStatus;
        static kSecCodeInfoTeamIdentifier: CFStringRef;
    }

    let mut code: SecCodeRef = std::ptr::null_mut();
    // SAFETY: `code` is a valid out-pointer; on success it holds a +1
    // (Copy-rule) SecCode describing the running process. Flags 0
    // (kSecCSDefaultFlags) requests the current dynamic code object without
    // additional validation. An unsigned process reports an error status,
    // which the caller maps to None.
    let status = unsafe { SecCodeCopySelf(0, &mut code) };
    if status != errSecSuccess || code.is_null() {
        return None;
    }
    let mut information: CFDictionaryRef = std::ptr::null();
    // SAFETY: `code` is the live +1 SecCode obtained above and `information`
    // is a valid out-pointer written only on success. The
    // kSecCSSigningInformation flag (2) is REQUIRED: measured on macOS
    // 26.6.1 (teamid_probe, Apple Development-signed), flags 0 and 1 return
    // a signing-information dictionary WITHOUT kSecCodeInfoTeamIdentifier;
    // the key only appears once kSecCSSigningInformation is set. On success
    // `information` is +1: the <Security/SecCode.h> declaration carries
    // CF_RETURNS_RETAINED.
    const K_SEC_CS_SIGNING_INFORMATION: SecCSFlags = 2;
    let status = unsafe {
        SecCodeCopySigningInformation(code, K_SEC_CS_SIGNING_INFORMATION, &mut information)
    };
    // SAFETY: `code` is a +1 CoreFoundation object owned by this scope and
    // released exactly once here, regardless of the outcome above.
    unsafe { CFRelease(code as CFTypeRef) };
    if status != errSecSuccess || information.is_null() {
        return None;
    }
    // SAFETY: `information` is the +1 CFDictionary returned above;
    // wrap_under_create_rule takes over the reference and releases it on
    // drop, and the dictionary outlives every use below.
    let information: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(information) };
    // SAFETY: kSecCodeInfoTeamIdentifier is an `extern const CFStringRef`
    // global declared in <Security/SecCode.h>. Like the kSecAttr* constants
    // in probe_item_platform, no function returns it, so nothing grants a
    // reference to release; wrap_under_get_rule retains a +1 for the
    // wrapper's own lifetime.
    let key = unsafe { CFString::wrap_under_get_rule(kSecCodeInfoTeamIdentifier) };
    // The key is absent for ad hoc-signed code, and a non-string value would
    // be a platform surprise — both read as "no team", never as a guess.
    information
        .find(&key)
        .and_then(|value| value.downcast::<CFString>())
        .map(|team| team.to_string())
}

#[cfg(not(target_os = "macos"))]
fn signing_team_identifier_platform() -> Option<String> {
    // OpenCodeServer only ships for macOS; this branch keeps the crate
    // compiling elsewhere for cross-checks.
    None
}

#[cfg(target_os = "macos")]
fn probe_item_platform(account: &str) -> KeychainProbe {
    use core_foundation::base::{CFRelease, CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use security_framework_sys::base::errSecSuccess;
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecReturnAttributes,
    };
    use security_framework_sys::keychain_item::SecItemCopyMatching;

    // SAFETY: every key is a global CoreFoundation string constant: an
    // `extern const CFStringRef` global in <Security/SecItem.h>, declared
    // by security-framework-sys as `pub static ...: CFStringRef`. No
    // function returns these constants, and Apple's ownership naming rules
    // "only apply to C functions that return Core Foundation objects", so
    // nothing here grants a reference we must release; the constants stay
    // at +0. core-foundation's generated `wrap_under_get_rule` calls
    // `CFRetain` before wrapping (0.10.1, `impl_TCFType!` macro); the
    // generated Drop calls `CFRelease` (0.10.1, `declare_TCFType!`
    // macro), so each wrapper owns a +1 reference for its own lifetime.
    // The query dictionary retains keys
    // and values for its own lifetime, which outlasts the
    // SecItemCopyMatching call.
    let pairs: Vec<(CFString, CFType)> = vec![
        (
            unsafe { CFString::wrap_under_get_rule(kSecClass) },
            unsafe { CFString::wrap_under_get_rule(kSecClassGenericPassword) }.into_CFType(),
        ),
        (
            unsafe { CFString::wrap_under_get_rule(kSecAttrService) },
            CFString::from(KEYCHAIN_SERVICE).into_CFType(),
        ),
        (
            unsafe { CFString::wrap_under_get_rule(kSecAttrAccount) },
            CFString::from(account).into_CFType(),
        ),
        (
            unsafe { CFString::wrap_under_get_rule(kSecReturnAttributes) },
            CFBoolean::from(true).into_CFType(),
        ),
    ];
    let query = CFDictionary::from_CFType_pairs(&pairs);
    let mut result = std::ptr::null();
    // SAFETY: `query` is a live CFDictionary and `result` a valid out-pointer
    // written only on success. The returned attributes dictionary is released
    // immediately; only the status code carries information here.
    let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) };
    if !result.is_null() {
        // SAFETY: `result` is a +1 (create-rule) CoreFoundation object
        // returned by SecItemCopyMatching. The +1 ownership is declared
        // directly on the function's signature in <Security/SecItem.h> via
        // `CF_RETURNS_RETAINED`, which is stronger evidence than the
        // Create/Copy naming convention alone.
        unsafe { CFRelease(result) };
    }
    match status {
        c if c == errSecSuccess => KeychainProbe::Exists,
        c if c == errSecItemNotFound => KeychainProbe::NotConfigured,
        code => KeychainProbe::Failed(code),
    }
}

#[cfg(not(target_os = "macos"))]
fn probe_item_platform(_account: &str) -> KeychainProbe {
    // OpenCodeServer only ships for macOS; this branch keeps the crate
    // compiling elsewhere for cross-checks.
    KeychainProbe::Failed(0)
}

#[cfg(target_os = "macos")]
fn read_password_platform(account: &str) -> KeychainRead {
    let options = PasswordOptions::new_generic_password(KEYCHAIN_SERVICE, account);
    match generic_password(options) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(password) => KeychainRead::Found(password),
            // A credential written by this product is always UTF-8. Anything
            // else means the item was edited outside the product; treat it
            // as unusable without claiming it is missing.
            Err(_) => KeychainRead::Failed(errSecAuthFailed),
        },
        Err(err) => classify(err),
    }
}

#[cfg(not(target_os = "macos"))]
fn read_password_platform(_account: &str) -> KeychainRead {
    KeychainRead::Failed(0)
}

#[cfg(target_os = "macos")]
fn classify(err: Error) -> KeychainRead {
    match err.code() {
        c if c == errSecItemNotFound => KeychainRead::NotConfigured,
        c if c == ERR_SEC_INTERACTION_NOT_ALLOWED
            || c == ERR_SEC_USER_CANCELED
            || c == errSecAuthFailed
            || c == ERR_SEC_MISSING_ENTITLEMENT =>
        {
            KeychainRead::AccessPending
        }
        c => KeychainRead::Failed(c),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn unique_account(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        format!("ocs-keychain-test-{tag}-{}-{nanos}", std::process::id())
    }

    fn delete_item(account: &str) {
        use security_framework::passwords::delete_generic_password;
        let _ = delete_generic_password(KEYCHAIN_SERVICE, account);
    }

    fn set_item(account: &str, password: &str) {
        use security_framework::passwords::set_generic_password;
        delete_item(account);
        set_generic_password(KEYCHAIN_SERVICE, account, password.as_bytes())
            .expect("set_generic_password");
    }

    #[test]
    fn probe_reports_missing_item_as_not_configured() {
        let account = unique_account("probe-missing");
        delete_item(&account);
        assert_eq!(probe_item(&account), KeychainProbe::NotConfigured);
    }

    #[test]
    fn probe_reports_existing_item_as_exists() {
        let account = unique_account("probe-exists");
        set_item(&account, "pw");
        let result = probe_item(&account);
        delete_item(&account);
        assert_eq!(result, KeychainProbe::Exists);
    }

    #[test]
    fn missing_item_reports_not_configured() {
        let account = unique_account("missing");
        delete_item(&account);
        assert_eq!(read_password(&account), KeychainRead::NotConfigured);
    }

    #[test]
    fn round_trip_reads_written_password() {
        let account = unique_account("roundtrip");
        set_item(&account, "s3cret-测试");
        let result = read_password(&account);
        delete_item(&account);
        assert_eq!(result, KeychainRead::Found("s3cret-测试".to_string()));
    }

    #[test]
    fn accounts_are_isolated() {
        let a = unique_account("a");
        let b = unique_account("b");
        set_item(&a, "pw-a");
        assert_eq!(probe_item(&b), KeychainProbe::NotConfigured);
        assert_eq!(read_password(&b), KeychainRead::NotConfigured);
        delete_item(&a);
    }

    #[test]
    fn ad_hoc_test_binary_has_no_team_identifier() {
        // The cargo-built test binary is ad hoc signed, so the runtime
        // self-lookup must return None — the same answer an unsigned or ad
        // hoc production build gets, which is what keeps dev builds on the
        // explicit "Allow Keychain Access…" path.
        assert_eq!(signing_team_identifier(), None);
    }

    #[test]
    fn debug_formatting_never_renders_the_password() {
        let secret = "unit-test-password-that-must-not-appear";
        let rendered = format!("{:?}", KeychainRead::Found(secret.to_owned()));
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("<redacted>"));
        assert_eq!(
            format!("{:?}", KeychainRead::NotConfigured),
            "KeychainRead::NotConfigured"
        );
        assert_eq!(
            format!("{:?}", KeychainRead::Failed(-25308)),
            "KeychainRead::Failed(-25308)"
        );
    }
}
