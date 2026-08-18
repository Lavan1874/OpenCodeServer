use crate::paths::AppPaths;
use crate::platform::{effective_uid, file_descriptor_identity};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CONFIG_SCHEMA_VERSION: u64 = 1;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(rename = "SchemaVersion")]
    pub schema_version: u64,
    #[serde(rename = "Hostname")]
    pub hostname: String,
    #[serde(rename = "Port")]
    pub port: u16,
    #[serde(rename = "Username")]
    pub username: String,
    /// The OpenCode password is never stored in this file. It lives in the
    /// login keychain (see `rust/src/keychain.rs`) and OpenCodeServerAgent
    /// merges it into this field after validation, so the fingerprint,
    /// spawn-environment, and health-check consumers keep one source.
    /// Serde skips the in-memory merge field in both directions; the current
    /// plist schema has no password key.
    #[serde(skip)]
    pub password: String,
    #[serde(rename = "MDNS")]
    pub mdns: bool,
    #[serde(rename = "ExecutablePath")]
    pub executable_path: String,
}

// Hand-written so a `{:?}` formatting can never render the in-memory merged
// password; the field exists only after the Keychain merge (see above).
impl fmt::Debug for ConfigFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigFile")
            .field("schema_version", &self.schema_version)
            .field("hostname", &self.hostname)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("mdns", &self.mdns)
            .field("executable_path", &self.executable_path)
            .finish()
    }
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            hostname: default_hostname(),
            port: default_port(),
            username: default_username(),
            password: String::new(),
            mdns: false,
            executable_path: String::new(),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedConfig {
    pub source: ConfigFile,
    pub configured_executable: PathBuf,
    pub canonical_executable: PathBuf,
    pub effective_username: String,
}

// Hand-written so a `{:?}` formatting routes the password through
// `ConfigFile`'s redacting Debug impl.
impl fmt::Debug for ValidatedConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedConfig")
            .field("source", &self.source)
            .field("configured_executable", &self.configured_executable)
            .field("canonical_executable", &self.canonical_executable)
            .field("effective_username", &self.effective_username)
            .finish()
    }
}

impl ValidatedConfig {
    pub fn endpoint(&self) -> String {
        format_endpoint(&self.source.hostname, self.source.port)
    }

    pub fn authentication_enabled(&self) -> bool {
        !self.source.password.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub issues: Vec<String>,
    pub selected_executable: Option<String>,
    pub candidates: Vec<String>,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    Invalid(Vec<String>),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Invalid(issues) => write!(formatter, "{}", issues.join("; ")),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn load_or_create(paths: &AppPaths) -> Result<ValidatedConfig, ConfigError> {
    if !paths.config_file.exists() {
        write_config_atomically(&paths.config_file, &ConfigFile::default())?;
    }
    load_and_validate(&paths.config_file)
}

pub fn load_and_validate(path: &Path) -> Result<ValidatedConfig, ConfigError> {
    validate(read_config(path)?)
}

pub fn validation_report(path: &Path) -> ValidationReport {
    let candidates = discover_executables()
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    match load_and_validate(path) {
        Ok(config) => ValidationReport {
            valid: true,
            issues: Vec::new(),
            selected_executable: Some(config.configured_executable.to_string_lossy().into_owned()),
            candidates,
        },
        Err(error) => ValidationReport {
            valid: false,
            issues: match error {
                ConfigError::Invalid(issues) => issues,
                ConfigError::Io(error) => vec![error.to_string()],
            },
            selected_executable: None,
            candidates,
        },
    }
}

fn read_config(path: &Path) -> Result<ConfigFile, ConfigError> {
    let symlink_metadata = fs::symlink_metadata(path)?;
    if symlink_metadata.file_type().is_symlink() || !symlink_metadata.is_file() {
        return Err(ConfigError::Invalid(vec![
            "config.plist must be a regular file, not a symbolic link".to_owned(),
        ]));
    }
    if symlink_metadata.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::Invalid(vec![
            "config.plist exceeds the 64 KiB limit".to_owned(),
        ]));
    }
    if symlink_metadata.mode() & 0o077 != 0 {
        return Err(ConfigError::Invalid(vec![
            "config.plist must not be accessible by group or other users (use mode 0600)"
                .to_owned(),
        ]));
    }
    if symlink_metadata.uid() != effective_uid() {
        return Err(ConfigError::Invalid(vec![
            "config.plist must be owned by the current user".to_owned(),
        ]));
    }

    let mut file = File::open(path)?;
    let (device, inode) = file_descriptor_identity(&file)?;
    if symlink_metadata.dev() != device || symlink_metadata.ino() != inode {
        return Err(ConfigError::Invalid(vec![
            "config.plist changed while it was being opened".to_owned(),
        ]));
    }
    let metadata = file.metadata()?;

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(ConfigError::Invalid(vec![
            "config.plist exceeds the 64 KiB limit".to_owned(),
        ]));
    }
    plist::from_bytes::<ConfigFile>(&bytes)
        .map_err(|error| ConfigError::Invalid(vec![format!("config.plist is not valid: {error}")]))
}

fn validate(mut config: ConfigFile) -> Result<ValidatedConfig, ConfigError> {
    // Normalize a bracketed literal IPv6 address (`[::1]`) to its canonical
    // unbracketed form so the spawn arguments, the health check, and the
    // endpoint preflight all see one hostname shape; `format_endpoint`
    // re-adds brackets for display.
    if let Some(unbracketed) = config
        .hostname
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        config.hostname = unbracketed.to_owned();
    }
    let mut issues = Vec::new();
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        issues.push(format!(
            "unsupported SchemaVersion {}; expected {}",
            config.schema_version, CONFIG_SCHEMA_VERSION
        ));
    }
    if let Err(issue) = validate_hostname(&config.hostname) {
        issues.push(issue);
    }
    if config.port == 0 {
        issues.push("Port must be between 1 and 65535".to_owned());
    }
    if config.username.len() > 128
        || config.username.contains(':')
        || config.username.chars().any(char::is_control)
    {
        issues.push(
            "Username must be at most 128 characters and contain no colon or control character"
                .to_owned(),
        );
    }

    let configured_executable = if config.executable_path.trim().is_empty() {
        discover_executables().into_iter().next()
    } else {
        Some(PathBuf::from(&config.executable_path))
    };
    let Some(configured_executable) = configured_executable else {
        issues.push(
            "No OpenCode executable was found; select an absolute native Mach-O executable"
                .to_owned(),
        );
        return Err(ConfigError::Invalid(issues));
    };

    if !configured_executable.is_absolute() {
        issues.push("ExecutablePath must be absolute".to_owned());
    }

    let canonical_executable = match fs::canonicalize(&configured_executable) {
        Ok(path) => path,
        Err(error) => {
            issues.push(format!("OpenCode executable cannot be resolved: {error}"));
            configured_executable.clone()
        }
    };
    match fs::metadata(&canonical_executable) {
        Ok(metadata) => {
            if !metadata.is_file() {
                issues.push("OpenCode executable target must be a regular file".to_owned());
            }
            if metadata.mode() & 0o111 == 0 {
                issues.push("OpenCode executable target is not executable".to_owned());
            }
        }
        Err(error) => issues.push(format!("OpenCode executable cannot be inspected: {error}")),
    }
    if issues.is_empty() {
        match is_arm64_macho(&canonical_executable) {
            Ok(true) => {}
            Ok(false) => issues.push(
                "OpenCode executable must be a native Mach-O containing arm64 code".to_owned(),
            ),
            Err(error) => issues.push(format!("OpenCode executable cannot be inspected: {error}")),
        }
    }

    if !issues.is_empty() {
        return Err(ConfigError::Invalid(issues));
    }
    let effective_username = if config.username.is_empty() {
        default_username()
    } else {
        config.username.clone()
    };
    Ok(ValidatedConfig {
        source: config,
        configured_executable,
        canonical_executable,
        effective_username,
    })
}

pub fn write_config_atomically(path: &Path, config: &ConfigFile) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;

    let mut data = Vec::new();
    plist::to_writer_xml(&mut data, config)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".config.plist.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(&temporary)?;
    let write_result = (|| {
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

pub fn discover_executables() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(PathBuf::from("/opt/homebrew/bin/opencode"));
    if let Some(home) = env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".opencode/bin/opencode"));
    }
    if let Some(search_path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&search_path).map(|directory| directory.join("opencode")));
    }

    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .filter(|path| {
            fs::metadata(path)
                .map(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
                .unwrap_or(false)
        })
        .filter(|path| {
            fs::canonicalize(path)
                .ok()
                .and_then(|canonical| is_arm64_macho(&canonical).ok())
                .unwrap_or(false)
        })
        .collect()
}

pub fn format_endpoint(hostname: &str, port: u16) -> String {
    if hostname.contains(':') && !hostname.starts_with('[') {
        format!("[{hostname}]:{port}")
    } else {
        format!("{hostname}:{port}")
    }
}

pub fn health_hostname(hostname: &str) -> &str {
    match hostname {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "::1",
        other => other,
    }
}

fn validate_hostname(hostname: &str) -> Result<(), String> {
    if hostname.is_empty() || hostname.len() > 253 {
        return Err("Hostname must contain between 1 and 253 characters".to_owned());
    }
    if hostname.chars().any(char::is_control)
        || hostname.chars().any(char::is_whitespace)
        || hostname.contains('/')
    {
        return Err("Hostname contains an invalid character".to_owned());
    }
    let unbracketed = hostname
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(hostname);
    if unbracketed.parse::<IpAddr>().is_ok() || hostname == "localhost" {
        return Ok(());
    }
    if hostname.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        Ok(())
    } else {
        Err("Hostname is not a valid IP address or DNS name".to_owned())
    }
}

fn is_arm64_macho(path: &Path) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut header = [0_u8; 4096];
    let length = file.read(&mut header)?;
    is_arm64_macho_bytes(&header[..length])
}

fn is_arm64_macho_bytes(bytes: &[u8]) -> io::Result<bool> {
    if bytes.len() < 8 {
        return Ok(false);
    }
    let magic_le = u32::from_le_bytes(bytes[0..4].try_into().expect("four bytes"));
    if matches!(magic_le, 0xfeed_face | 0xfeed_facf) {
        let cpu = u32::from_le_bytes(bytes[4..8].try_into().expect("four bytes"));
        return Ok(cpu == CPU_TYPE_ARM64);
    }

    let magic_be = u32::from_be_bytes(bytes[0..4].try_into().expect("four bytes"));
    let (is_64, byte_order_big) = match magic_be {
        0xcafe_babe => (false, true),
        0xcafe_babf => (true, true),
        0xbeba_feca => (false, false),
        0xbfba_feca => (true, false),
        _ => return Ok(false),
    };
    let read_u32 = |slice: &[u8]| {
        if byte_order_big {
            u32::from_be_bytes(slice.try_into().expect("four bytes"))
        } else {
            u32::from_le_bytes(slice.try_into().expect("four bytes"))
        }
    };
    let count = read_u32(&bytes[4..8]) as usize;
    let entry_size = if is_64 { 32 } else { 20 };
    let required = 8_usize.saturating_add(count.saturating_mul(entry_size));
    if count > 64 || bytes.len() < required {
        return Ok(false);
    }
    for index in 0..count {
        let offset = 8 + index * entry_size;
        if read_u32(&bytes[offset..offset + 4]) == CPU_TYPE_ARM64 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn default_hostname() -> String {
    "127.0.0.1".to_owned()
}

fn default_port() -> u16 {
    4096
}

fn default_username() -> String {
    "opencode".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_and_stable() {
        let config = ConfigFile::default();
        assert_eq!(config.hostname, "127.0.0.1");
        assert_eq!(config.port, 4096);
        assert_eq!(config.username, "opencode");
        assert!(config.password.is_empty());
        assert!(!config.mdns);
    }

    #[test]
    fn formats_ipv6_endpoint() {
        assert_eq!(format_endpoint("::1", 4096), "[::1]:4096");
        assert_eq!(format_endpoint("127.0.0.1", 4096), "127.0.0.1:4096");
    }

    #[test]
    fn detects_thin_arm64_macho() {
        let mut bytes = vec![0_u8; 32];
        bytes[0..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&CPU_TYPE_ARM64.to_le_bytes());
        assert!(is_arm64_macho_bytes(&bytes).expect("parse"));
    }

    #[test]
    fn rejects_script_header() {
        assert!(!is_arm64_macho_bytes(b"#!/bin/sh\n").expect("parse"));
    }

    #[test]
    fn validates_hostnames() {
        assert!(validate_hostname("127.0.0.1").is_ok());
        assert!(validate_hostname("::1").is_ok());
        assert!(validate_hostname("mac-studio.local").is_ok());
        assert!(validate_hostname("bad host").is_err());
    }

    #[test]
    fn bracketed_ipv6_hostname_is_stored_in_canonical_unbracketed_form() {
        let executable = std::env::current_exe()
            .expect("test executable")
            .to_string_lossy()
            .into_owned();
        let config = validate(ConfigFile {
            hostname: "[::1]".to_owned(),
            executable_path: executable.clone(),
            ..ConfigFile::default()
        })
        .expect("bracketed IPv6 must validate");
        assert_eq!(config.source.hostname, "::1");
        // Display re-adds the brackets; spawn, the health check, and the
        // endpoint preflight all see the single unbracketed form.
        assert_eq!(config.endpoint(), "[::1]:4096");

        let wildcard = validate(ConfigFile {
            hostname: "[::]".to_owned(),
            executable_path: executable,
            ..ConfigFile::default()
        })
        .expect("bracketed IPv6 wildcard must validate");
        assert_eq!(wildcard.source.hostname, "::");
        assert_eq!(health_hostname(&wildcard.source.hostname), "::1");
    }

    #[test]
    fn debug_formatting_never_renders_the_password() {
        let secret = "unit-test-password-that-must-not-appear";
        let config = ConfigFile {
            password: secret.to_owned(),
            ..ConfigFile::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("<redacted>"));

        let validated = ValidatedConfig {
            source: config,
            configured_executable: PathBuf::from("/opt/homebrew/bin/opencode"),
            canonical_executable: PathBuf::from("/opt/homebrew/bin/opencode"),
            effective_username: "opencode".to_owned(),
        };
        let rendered = format!("{validated:?}");
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn legacy_password_key_is_rejected() {
        // Pre-Keychain builds stored the OpenCode password in config.plist
        // under `Password`. The current schema has no password key at all, so
        // deny_unknown_fields must reject the legacy key instead of loading a
        // file whose credential the product no longer reads.
        let root =
            std::env::temp_dir().join(format!("ocs-config-legacy-password-{}", std::process::id()));
        fs::create_dir_all(&root).expect("create temp dir");
        let path = root.join("config.plist");
        let legacy = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>SchemaVersion</key>
    <integer>1</integer>
    <key>Hostname</key>
    <string>127.0.0.1</string>
    <key>Port</key>
    <integer>4096</integer>
    <key>Username</key>
    <string>opencode</string>
    <key>Password</key>
    <string>legacy-secret</string>
    <key>MDNS</key>
    <false/>
    <key>ExecutablePath</key>
    <string></string>
</dict>
</plist>
"#;
        fs::write(&path, legacy).expect("write legacy config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode 0600");
        let result = read_config(&path);
        let _ = fs::remove_dir_all(&root);
        assert!(
            matches!(result, Err(ConfigError::Invalid(_))),
            "a config.plist with a legacy Password key must be rejected"
        );
    }
}
