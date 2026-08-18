//! Persisted record that a Keychain decrypt once succeeded for an account
//! with a build of OpenCodeServerAgent (ADR 0016).
//!
//! On macOS 26 the legacy-keychain consent dialog cannot be suppressed by any
//! query key: any decrypt-class read by an untrusted binary blocks on the
//! system dialog. OpenCodeServerAgent may therefore only attempt a background
//! decrypt (agent restart, configuration reload) when there is positive
//! evidence that a decrypt already succeeded for the same account — otherwise
//! a routine config reload could raise a dialog from a background process.
//! This marker file is that evidence:
//!
//! - written after every successful decrypt (typically the Settings
//!   "Allow Keychain Access…" flow after the user picked "Always Allow");
//! - cleared when the Keychain item disappears, the user declines a prompt,
//!   or the item is rewritten without matching team evidence (the
//!   SecItemUpdate-wipes-the-partition rule was measured self-signed; see
//!   below), so a revoked grant cannot re-prompt on every configuration
//!   reload;
//! - scoped to the account name, the agent's bundle version, AND the agent's
//!   signing team identifier. The account scopes the evidence to one Keychain
//!   item. The bundle version scopes it to one binary: measured self-signed /
//!   no-Team-ID (2026-08-05 v42→v43 walkthrough, isolated 2026-08-09
//!   KeychainPartitionProbe), the XARA partition grant is pinned to the
//!   approving binary's cdHash, so a marker written by a different bundle
//!   version proves nothing about THIS binary — `covers` therefore stays an
//!   exact account+version match. The team identifier records whose cdHash
//!   pinning that was: measured 2026-08-17 on macOS 26.6.1 under the ADR 0021
//!   Apple Development identity (ADR 0016 amendment), the team-anchored
//!   partition grant survived a same-team cdHash change, a real SecItemUpdate
//!   password change, and fresh agent process restarts — all silent. A marker
//!   whose version no longer matches but whose recorded team equals this
//!   running build's own team (`team_evidence`) therefore authorizes ONE
//!   bounded automatic silent re-read; without matching team evidence the
//!   self-signed measurements remain the reason background paths refuse.
//!
//! The schema is exactly three lines: account, bundle version, team
//! identifier. The team line may be empty — an ad hoc/unsigned build has no
//! team — in which case the marker loads but yields no team evidence.
//! Anything else, including a legacy two-line marker, is absent evidence:
//! fail-safe, no migration; the user simply clicks "Allow Keychain Access…"
//! once on the introducing upgrade.
//!
//! The marker contains no secret — only the account name, bundle version, and
//! team identifier — and losing it is fail-safe: the user simply clicks
//! "Allow Keychain Access…" once more.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::paths::AppPaths;
use crate::private_file::read_private_file;

const MARKER_FILE_NAME: &str = "credential-grant";
const MAX_MARKER_BYTES: u64 = 16 * 1024;

pub struct CredentialGrant {
    path: PathBuf,
}

impl CredentialGrant {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            path: paths.support_dir.join(MARKER_FILE_NAME),
        }
    }

    /// The recorded `(account, bundle version, team identifier)` triple, if
    /// the marker contains exactly the current three-line schema. The team
    /// line may be empty (a grant recorded by an ad hoc/unsigned build); the
    /// account and version lines may not. Anything else — including a legacy
    /// two-line marker — is absent evidence.
    pub fn load(&self) -> Option<(String, String, String)> {
        let bytes = read_private_file(&self.path, MAX_MARKER_BYTES).ok()?;
        let contents = std::str::from_utf8(&bytes).ok()?;
        let mut lines = contents.lines();
        let account = lines.next()?.trim();
        let version = lines.next()?.trim();
        let team = lines.next()?.trim();
        if account.is_empty() || version.is_empty() || lines.next().is_some() {
            return None;
        }
        Some((account.to_owned(), version.to_owned(), team.to_owned()))
    }

    /// Returns true when the marker proves a decrypt succeeded for `account`
    /// with a binary whose bundle version matches `bundle_version`. The
    /// version comparison is exact: measured self-signed, a grant written by
    /// build N says nothing about build N+1, whose cdHash the item's
    /// partition list does not contain. The team line is not consulted here;
    /// a version-mismatched marker is evaluated through [`Self::team_evidence`]
    /// instead.
    pub fn covers(&self, account: &str, bundle_version: &str) -> bool {
        match self.load() {
            Some((recorded_account, recorded_version, _)) => {
                recorded_account == account && recorded_version == bundle_version
            }
            None => false,
        }
    }

    /// The signing team identifier recorded for `account`: `Some` when the
    /// marker names that account and carries a non-empty team line, `None`
    /// otherwise. An empty team line (a grant recorded by an ad hoc/unsigned
    /// build) is no team evidence, so dev builds can never take the
    /// same-team automatic re-read path.
    pub fn team_evidence(&self, account: &str) -> Option<String> {
        match self.load() {
            Some((recorded_account, _, recorded_team))
                if recorded_account == account && !recorded_team.is_empty() =>
            {
                Some(recorded_team)
            }
            _ => None,
        }
    }

    /// Atomically records that a decrypt succeeded for `account` with this
    /// bundle version and signing team. `team` may be empty (ad hoc/unsigned
    /// build): the marker is still written so the schema stays strict and
    /// dev-build behavior stays observable, but it yields no team evidence on
    /// load.
    pub fn record(&self, account: &str, bundle_version: &str, team: &str) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let staging = self.path.with_extension("tmp");
        // A staging file left behind by an interrupted previous record is
        // disposable; remove it so `create_new` cannot wedge on the stale
        // copy (or on one created with wider permissions).
        if let Err(error) = fs::remove_file(&staging)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        // The staging file is created with mode 0600 from the start — the
        // marker must never exist with wider permissions, not even between
        // creation and rename.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        let result = (|| {
            file.write_all(format!("{account}\n{bundle_version}\n{team}\n").as_bytes())?;
            file.sync_all()?;
            fs::rename(&staging, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staging);
        }
        result
    }

    /// Removes the marker for `account`, regardless of the recorded version
    /// or team; a marker recorded for a different account is left untouched.
    /// A missing file is not an error.
    pub fn clear_for(&self, account: &str) -> io::Result<()> {
        match self.load() {
            Some((recorded_account, _, _)) if recorded_account == account => {}
            _ => return Ok(()),
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn marker(tag: &str) -> CredentialGrant {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ocs-grant-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        CredentialGrant {
            path: dir.join(MARKER_FILE_NAME),
        }
    }

    fn write_private(path: &std::path::Path, contents: &[u8]) {
        fs::write(path, contents).expect("write marker fixture");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("set marker permissions");
    }

    #[test]
    fn absent_marker_covers_nothing() {
        let grant = marker("absent");
        assert!(!grant.covers("opencode", "44"));
        assert_eq!(grant.load(), None);
        assert_eq!(grant.team_evidence("opencode"), None);
    }

    #[test]
    fn a_symlink_marker_is_not_grant_evidence() {
        let grant = marker("symlink");
        let target = grant.path.with_file_name("target");
        write_private(&target, b"opencode\n44\nTEAM\n");
        symlink(&target, &grant.path).expect("marker symlink");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
        assert_eq!(grant.team_evidence("opencode"), None);
    }

    #[test]
    fn an_oversized_marker_is_not_grant_evidence() {
        let grant = marker("oversized");
        write_private(&grant.path, &vec![b'a'; 16 * 1024 + 1]);
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
    }

    #[test]
    fn a_group_readable_marker_is_not_grant_evidence() {
        let grant = marker("mode");
        write_private(&grant.path, b"opencode\n44\nTEAM\n");
        fs::set_permissions(&grant.path, fs::Permissions::from_mode(0o640))
            .expect("make marker group-readable");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
    }

    #[test]
    fn record_then_covers_same_account_and_version_only() {
        let grant = marker("record");
        grant
            .record("opencode", "44", "TESTTEAMID")
            .expect("record");
        assert!(grant.covers("opencode", "44"));
        assert!(!grant.covers("other", "44"));
        // A different build of the same product is a different cdHash and
        // therefore unproven.
        assert!(!grant.covers("opencode", "45"));
        assert_eq!(
            grant.load(),
            Some((
                "opencode".to_owned(),
                "44".to_owned(),
                "TESTTEAMID".to_owned()
            ))
        );
    }

    #[test]
    fn team_evidence_is_scoped_to_the_account() {
        let grant = marker("team");
        grant
            .record("opencode", "44", "TESTTEAMID")
            .expect("record");
        assert_eq!(
            grant.team_evidence("opencode"),
            Some("TESTTEAMID".to_owned())
        );
        assert_eq!(grant.team_evidence("other"), None);
    }

    #[test]
    fn legacy_two_line_marker_is_absent_evidence() {
        // The introducing upgrade meets the pre-team two-line schema on disk:
        // strict parsing treats it as no evidence at all (fail-safe, no
        // migration — one documented "Allow Keychain Access…" click).
        let grant = marker("legacy");
        write_private(&grant.path, b"opencode\n44\n");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
        assert_eq!(grant.team_evidence("opencode"), None);
    }

    #[test]
    fn an_empty_team_line_loads_but_yields_no_team_evidence() {
        // Ad hoc/unsigned builds have no team; the marker is still recorded
        // (schema stays strict, dev-build behavior stays observable) but the
        // empty line must not read as team evidence.
        let grant = marker("empty-team");
        grant.record("opencode", "44", "").expect("record");
        assert_eq!(
            grant.load(),
            Some(("opencode".to_owned(), "44".to_owned(), String::new()))
        );
        assert!(grant.covers("opencode", "44"));
        assert_eq!(grant.team_evidence("opencode"), None);
    }

    #[test]
    fn non_current_marker_shape_is_not_a_grant() {
        let grant = marker("invalid-shape");
        write_private(&grant.path, b"opencode\n");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
        let grant = marker("invalid-shape-four");
        write_private(&grant.path, b"opencode\n44\nTESTTEAMID\nextra\n");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
        assert_eq!(grant.team_evidence("opencode"), None);
    }

    #[test]
    fn clear_for_removes_matching_account_regardless_of_version() {
        let grant = marker("clear");
        grant
            .record("opencode", "44", "TESTTEAMID")
            .expect("record");
        grant.clear_for("other").expect("clear other");
        assert!(grant.covers("opencode", "44"));
        grant.clear_for("opencode").expect("clear");
        assert!(!grant.covers("opencode", "44"));
        // Clearing twice is not an error.
        grant.clear_for("opencode").expect("clear again");
    }

    #[test]
    fn an_unwritable_marker_reports_the_error_and_proves_no_grant() {
        // Losing the marker is fail-safe by design: the caller logs and
        // carries on, and the missing evidence simply costs one more explicit
        // "Allow Keychain Access…" click. What must NEVER happen is a failed
        // write leaving behind something that later reads as a valid grant.
        let grant = marker("unwritable");
        let parent = grant.path.parent().expect("marker parent").to_owned();
        let mut permissions = fs::metadata(&parent).expect("metadata").permissions();
        let original = permissions.clone();
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o500);
        }
        fs::set_permissions(&parent, permissions).expect("make the directory read-only");

        let result = grant.record("opencode", "44", "TESTTEAMID");

        fs::set_permissions(&parent, original).expect("restore permissions");
        assert!(
            result.is_err(),
            "an unwritable marker must report the error"
        );
        assert!(
            !grant.covers("opencode", "44"),
            "a failed write must not leave anything that reads as a grant"
        );
        assert_eq!(grant.load(), None);
    }

    #[test]
    fn an_unreadable_marker_is_treated_as_absent() {
        // `load` swallows I/O errors into `None` on purpose: an unreadable
        // marker is not evidence, and the fail-closed answer is "no proven
        // grant" rather than a background decrypt on a guess. A directory in
        // the marker's place is the cheap portable stand-in for an
        // unreadable file.
        let grant = marker("unreadable");
        fs::create_dir_all(&grant.path).expect("create a directory where the marker belongs");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("opencode", "44"));
    }

    #[test]
    fn a_blank_account_line_is_not_a_grant() {
        // A truncated or zero-filled marker (interrupted write, restored
        // backup) must not authorize anything.
        let grant = marker("blank");
        write_private(&grant.path, b"\n44\nTESTTEAMID\n");
        assert_eq!(grant.load(), None);
        assert!(!grant.covers("", "44"));
        assert!(!grant.covers("opencode", "44"));
        assert_eq!(grant.team_evidence(""), None);
    }
}
