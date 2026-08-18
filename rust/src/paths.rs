use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub support_dir: PathBuf,
    pub config_file: PathBuf,
    pub run_dir: PathBuf,
    pub control_socket: PathBuf,
    pub runtime_state: PathBuf,
    pub config_fingerprint_key: PathBuf,
}

impl AppPaths {
    pub fn discover() -> io::Result<Self> {
        Ok(Self::from_support_dir(resolve_support_directory(
            env::var_os("OPENCODESERVER_SUPPORT_DIR"),
            env::var_os("HOME"),
        )?))
    }

    pub fn from_support_dir(support_dir: PathBuf) -> Self {
        let run_dir = support_dir.join("run");
        Self {
            config_file: support_dir.join("config.plist"),
            control_socket: run_dir.join("control.sock"),
            runtime_state: run_dir.join("state.json"),
            config_fingerprint_key: support_dir.join(".config-fingerprint.key"),
            support_dir,
            run_dir,
        }
    }

    pub fn ensure_directories(&self) -> io::Result<()> {
        ensure_private_directory(&self.support_dir)?;
        ensure_private_directory(&self.run_dir)
    }
}

fn resolve_support_directory(
    override_path: Option<OsString>,
    home: Option<OsString>,
) -> io::Result<PathBuf> {
    if let Some(value) = override_path.filter(|value| !value.to_string_lossy().trim().is_empty()) {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OPENCODESERVER_SUPPORT_DIR must be an absolute path",
            ));
        }
        return Ok(path);
    }

    let home = home
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("OpenCodeServer"))
}

pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a private directory", path.display()),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_home() -> OsString {
        OsString::from("/fixture-home")
    }

    fn default_support_directory() -> PathBuf {
        PathBuf::from("/fixture-home/Library/Application Support/OpenCodeServer")
    }

    #[test]
    fn derives_expected_paths() {
        let paths = AppPaths::from_support_dir(PathBuf::from("/tmp/ocs-test"));
        assert_eq!(
            paths.config_file,
            PathBuf::from("/tmp/ocs-test/config.plist")
        );
        assert_eq!(
            paths.control_socket,
            PathBuf::from("/tmp/ocs-test/run/control.sock")
        );
        assert_eq!(
            paths.config_fingerprint_key,
            PathBuf::from("/tmp/ocs-test/.config-fingerprint.key")
        );
    }

    #[test]
    fn absent_support_directory_override_uses_application_support() {
        assert_eq!(
            resolve_support_directory(None, Some(fixture_home())).expect("resolve absent override"),
            default_support_directory()
        );
    }

    #[test]
    fn empty_support_directory_override_uses_application_support() {
        assert_eq!(
            resolve_support_directory(Some(OsString::new()), Some(fixture_home()))
                .expect("resolve empty override"),
            default_support_directory()
        );
    }

    #[test]
    fn whitespace_support_directory_override_uses_application_support() {
        assert_eq!(
            resolve_support_directory(Some(OsString::from(" \t\n")), Some(fixture_home()))
                .expect("resolve whitespace override"),
            default_support_directory()
        );
    }

    #[test]
    fn relative_support_directory_override_is_rejected() {
        let error = resolve_support_directory(
            Some(OsString::from("relative/support")),
            Some(fixture_home()),
        )
        .expect_err("reject relative override");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn absolute_support_directory_override_is_used_verbatim() {
        assert_eq!(
            resolve_support_directory(
                Some(OsString::from("/fixture-support")),
                Some(fixture_home()),
            )
            .expect("resolve absolute override"),
            PathBuf::from("/fixture-support")
        );
    }
}
