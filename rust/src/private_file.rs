//! Bounded reads for user-owned private state files.
//!
//! The caller never inspects path metadata before opening. O_NOFOLLOW and
//! descriptor metadata checks keep a final-component symlink swap from turning
//! a private-state read into an arbitrary-file read.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use crate::platform::effective_uid;

pub(crate) fn read_private_file(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options.open(path)?;
    validate_descriptor(&file, max_bytes)?;

    let mut bytes =
        Vec::with_capacity(file.metadata()?.len().min(max_bytes.saturating_add(1)) as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private state exceeds its size limit",
        ));
    }
    Ok(bytes)
}

fn validate_descriptor(file: &File, max_bytes: u64) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private state must be a regular file",
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state must be owned by the current user",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state must not be accessible by group or other users",
        ));
    }
    if metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private state exceeds its size limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ocs-private-file-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("fixture directory");
        let path = root.join("state");
        (root, path)
    }

    fn write_private(path: &std::path::Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("fixture file");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("fixture permissions");
    }

    #[test]
    fn missing_file_is_an_ordinary_io_error() {
        let (root, path) = fixture("missing");
        let error = read_private_file(&path, 64).expect_err("missing file");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn regular_current_user0600_file_reads() {
        let (root, path) = fixture("normal");
        write_private(&path, b"state");
        assert_eq!(read_private_file(&path, 64).expect("read"), b"state");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn symlink_is_rejected_before_following() {
        let (root, path) = fixture("symlink");
        let target = root.join("target");
        write_private(&target, b"secret");
        symlink(&target, &path).expect("symlink");
        assert!(read_private_file(&path, 64).is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn directory_is_not_a_private_file() {
        let (root, path) = fixture("directory");
        fs::create_dir(&path).expect("directory");
        let error = read_private_file(&path, 64).expect_err("directory");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn oversized_file_is_rejected() {
        let (root, path) = fixture("oversized");
        write_private(&path, b"12345");
        let error = read_private_file(&path, 4).expect_err("oversized");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn group_readable_file_is_rejected() {
        let (root, path) = fixture("mode");
        write_private(&path, b"state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("permissions");
        let error = read_private_file(&path, 64).expect_err("mode");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
