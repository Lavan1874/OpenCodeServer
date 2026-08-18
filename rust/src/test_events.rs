//! Per-fixture event traces used by the process-supervision integration suite.
//!
//! This module is compiled only for tests and the `test-fixture` feature. It
//! deliberately has no process-global state: every trace is selected by the
//! private executable path owned by one test instance. Production builds do
//! not contain this module or its file protocol.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ENABLED_MARKER: &str = "query-events.enabled";
pub const EVENT_LOG: &str = "query.events";

fn marker_path(executable: &Path) -> PathBuf {
    executable.with_file_name(ENABLED_MARKER)
}

pub fn event_log_path(executable: &Path) -> PathBuf {
    executable.with_file_name(EVENT_LOG)
}

pub fn enabled(executable: &Path) -> bool {
    marker_path(executable).is_file()
}

/// Appends one bounded, privacy-neutral event record. Each writer is scoped to
/// a test-owned fixture directory; no global event sink or environment flag is
/// involved. Event details are constrained to one line so a failure report can
/// be parsed without exposing configuration or user state.
pub fn emit(executable: &Path, event: &str, detail: &str) {
    if !enabled(executable) {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let clean_event = clean_field(event);
    let clean_detail = clean_field(detail);
    let line = format!(
        "{timestamp}\t{}\t{clean_event}\t{clean_detail}\n",
        process::id()
    );
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(event_log_path(executable))
    else {
        return;
    };
    let _ = file.write_all(line.as_bytes());
    let _ = file.flush();
}

pub fn read(executable: &Path) -> Vec<String> {
    fs::read_to_string(event_log_path(executable))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn clean_field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' | '\0' => ' ',
            character => character,
        })
        .collect()
}
