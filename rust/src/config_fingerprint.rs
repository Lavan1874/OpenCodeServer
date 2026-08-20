use crate::config::ValidatedConfig;
use crate::paths::AppPaths;
use crate::platform::{effective_uid, file_descriptor_identity};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const FINGERPRINT_VERSION: u32 = 1;
const KEY_BYTES: usize = 32;
const TAG_BYTES: usize = 32;
const DOMAIN: &[u8] = b"OpenCodeServer.ConfigFingerprint.v1\0";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigFingerprint {
    pub version: u32,
    pub hmac_sha256: String,
}

impl fmt::Debug for ConfigFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigFingerprint")
            .field("version", &self.version)
            .field("hmac_sha256", &"<redacted>")
            .finish()
    }
}

pub struct ConfigFingerprintKey([u8; KEY_BYTES]);

impl fmt::Debug for ConfigFingerprintKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfigFingerprintKey(<redacted>)")
    }
}

impl ConfigFingerprintKey {
    pub fn load_or_create(paths: &AppPaths) -> io::Result<Self> {
        match Self::load(&paths.config_fingerprint_key) {
            Ok(key) => Ok(key),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Self::create(paths, &paths.config_fingerprint_key)
            }
            Err(error) => Err(error),
        }
    }

    pub fn fingerprint(&self, config: &ValidatedConfig) -> ConfigFingerprint {
        let mut mac = new_mac(&self.0);
        update_canonical_config(&mut mac, config);
        ConfigFingerprint {
            version: FINGERPRINT_VERSION,
            hmac_sha256: encode_hex(&mac.finalize().into_bytes()),
        }
    }

    pub fn verifies(&self, fingerprint: &ConfigFingerprint, config: &ValidatedConfig) -> bool {
        if fingerprint.version != FINGERPRINT_VERSION {
            return false;
        }
        let Some(expected_tag) = decode_hex::<TAG_BYTES>(&fingerprint.hmac_sha256) else {
            return false;
        };
        let mut mac = new_mac(&self.0);
        update_canonical_config(&mut mac, config);
        mac.verify_slice(&expected_tag).is_ok()
    }

    fn load(path: &Path) -> io::Result<Self> {
        let path_metadata = fs::symlink_metadata(path)?;
        validate_key_metadata(&path_metadata)?;

        let mut file = File::open(path)?;
        let (device, inode) = file_descriptor_identity(&file)?;
        if path_metadata.dev() != device || path_metadata.ino() != inode {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "configuration fingerprint key changed while it was being opened",
            ));
        }

        let metadata = file.metadata()?;
        validate_key_metadata(&metadata)?;
        let mut key = [0_u8; KEY_BYTES];
        file.read_exact(&mut key)?;
        let mut trailing = [0_u8; 1];
        if file.read(&mut trailing)? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "configuration fingerprint key has an invalid length",
            ));
        }
        Ok(Self(key))
    }

    fn create(paths: &AppPaths, destination: &Path) -> io::Result<Self> {
        paths.ensure_directories()?;
        let mut key = [0_u8; KEY_BYTES];
        getrandom::fill(&mut key).map_err(|error| {
            io::Error::other(format!("secure random generation failed: {error}"))
        })?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = paths.support_dir.join(format!(
            ".config-fingerprint-key.{}.{}.tmp",
            std::process::id(),
            nonce
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&key)?;
            file.sync_all()?;
            match fs::hard_link(&temporary, destination) {
                Ok(()) => Ok(Self(key)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    Self::load(destination)
                }
                Err(error) => Err(error),
            }
        })();
        let _ = fs::remove_file(&temporary);
        result
    }
}

fn validate_key_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration fingerprint key must be a regular file",
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "configuration fingerprint key must be owned by the current user",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "configuration fingerprint key must have mode 0600",
        ));
    }
    if metadata.len() != KEY_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration fingerprint key has an invalid length",
        ));
    }
    Ok(())
}

fn new_mac(key: &[u8; KEY_BYTES]) -> HmacSha256 {
    HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts a 32-byte key")
}

fn update_canonical_config(mac: &mut HmacSha256, config: &ValidatedConfig) {
    mac.update(DOMAIN);
    update_u64(mac, config.source.schema_version);
    update_bytes(mac, config.source.hostname.as_bytes());
    update_u16(mac, config.source.port);
    update_bytes(mac, config.effective_username.as_bytes());
    update_bytes(mac, config.source.password.as_bytes());
    mac.update(&[u8::from(config.source.mdns)]);
    update_bytes(
        mac,
        config.configured_executable.to_string_lossy().as_bytes(),
    );
}

fn update_u16(mac: &mut HmacSha256, value: u16) {
    mac.update(&value.to_be_bytes());
}

fn update_u64(mac: &mut HmacSha256, value: u64) {
    mac.update(&value.to_be_bytes());
}

fn update_bytes(mac: &mut HmacSha256, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("configuration field length fits in u64");
    update_u64(mac, length);
    mac.update(value);
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2 {
        return None;
    }
    let mut output = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = decode_nibble(pair[0])?
            .checked_mul(16)?
            .checked_add(decode_nibble(pair[1])?)?;
    }
    Some(output)
}

fn decode_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigFile, ValidatedConfig};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn key() -> ConfigFingerprintKey {
        ConfigFingerprintKey([0x5a; KEY_BYTES])
    }

    fn validated(config: ConfigFile) -> ValidatedConfig {
        let configured_executable = if config.executable_path.is_empty() {
            PathBuf::from("/opt/homebrew/bin/opencode")
        } else {
            PathBuf::from(&config.executable_path)
        };
        let effective_username = if config.username.is_empty() {
            "opencode".to_owned()
        } else {
            config.username.clone()
        };
        ValidatedConfig {
            source: config,
            configured_executable: configured_executable.clone(),
            canonical_executable: configured_executable,
            effective_username,
        }
    }

    #[test]
    fn normalizes_effective_username_and_executable() {
        let implicit = validated(ConfigFile::default());
        let explicit = validated(ConfigFile {
            username: "opencode".to_owned(),
            executable_path: "/opt/homebrew/bin/opencode".to_owned(),
            ..ConfigFile::default()
        });
        assert_eq!(key().fingerprint(&implicit), key().fingerprint(&explicit));
    }

    #[test]
    fn detects_semantic_changes_even_when_length_is_unchanged() {
        let first = validated(ConfigFile {
            hostname: "127.0.0.1".to_owned(),
            ..ConfigFile::default()
        });
        let second = validated(ConfigFile {
            hostname: "127.0.0.2".to_owned(),
            ..ConfigFile::default()
        });
        assert_ne!(key().fingerprint(&first), key().fingerprint(&second));
    }

    #[test]
    fn fingerprint_verification_is_versioned_and_password_is_not_serialized() {
        let secret = "unit-test-password-that-must-not-appear";
        let config = validated(ConfigFile {
            password: secret.to_owned(),
            ..ConfigFile::default()
        });
        let fingerprint = key().fingerprint(&config);
        assert!(key().verifies(&fingerprint, &config));
        assert!(
            !serde_json::to_string(&fingerprint)
                .expect("serialize fingerprint")
                .contains(secret)
        );

        let mut unsupported = fingerprint;
        unsupported.version += 1;
        assert!(!key().verifies(&unsupported, &config));
    }

    #[test]
    fn generated_key_is_private_and_persistent() {
        let root = std::env::temp_dir().join(format!(
            "opencodeserver-fingerprint-key-{}",
            std::process::id()
        ));
        let paths = AppPaths::from_support_dir(root.clone());
        let first = ConfigFingerprintKey::load_or_create(&paths).expect("create key");
        let second = ConfigFingerprintKey::load_or_create(&paths).expect("reload key");
        let config = validated(ConfigFile::default());
        assert_eq!(first.fingerprint(&config), second.fingerprint(&config));
        let metadata = fs::metadata(&paths.config_fingerprint_key).expect("key metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(root);
    }
}
