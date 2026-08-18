use crate::config::ValidationReport;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 6;
pub const MAX_MESSAGE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Status,
    Start,
    Stop,
    ContinueStop,
    ForceStop,
    Restart,
    RefreshFda,
    RefreshCredentials,
    /// Non-interactive notice from the GUI that the Keychain item was just
    /// changed: the agent must not trust its in-memory password as current,
    /// but must NOT read the item from this background path either (the
    /// consent dialog cannot be suppressed). The re-read only happens behind
    /// an explicit RefreshCredentials (the Settings "Allow Keychain Access…" button).
    CredentialChanged,
    /// Non-interactive notice that Settings explicitly deleted the Keychain
    /// item. Unlike a rewrite, absence is already proven by the successful
    /// SecItemDelete, so the agent must clear the carried credential and
    /// converge directly to NotConfigured without attempting any Keychain
    /// read or asking for authorization.
    CredentialRemoved,
    ValidateConfig,
    Subscribe,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub command: Command,
}

impl Request {
    pub fn new(command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            command,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Stopped,
    Starting,
    Healthy,
    Unhealthy,
    Stopping,
    StopTimedOut,
    WaitingToRestart,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Unknown,
    Healthy,
    Unhealthy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FdaState {
    Verified,
    NotVerified,
    UnableToDetermine,
}

/// Whether the OpenCode password in the login keychain is usable by
/// OpenCodeServerAgent. `AccessPending` means an item may exist but the
/// agent has not been authorized to read it yet (the user grants access
/// from the Settings window); it is never treated as "not configured".
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PasswordState {
    NotConfigured,
    AccessPending,
    Configured,
}

/// The lifecycle actions that OpenCodeServerAgent can safely accept at the
/// instant represented by a status snapshot.  OpenCodeServerAgent computes
/// this value from its authoritative process, credential, and runtime-state
/// facts; OpenCodeServer must only render the result and still handle a
/// command-time race through the normal IPC response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionCapabilities {
    pub start: bool,
    pub stop: bool,
    pub restart: bool,
    pub continue_stop: bool,
    pub force_stop: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Failure,
    Recovered,
    FinalFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationEvent {
    pub event_id: String,
    pub kind: NotificationKind,
    pub title: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Status {
    pub protocol_version: u32,
    pub agent_version: String,
    pub agent_uptime_seconds: u64,
    pub desired_state: DesiredState,
    pub server_state: ServerState,
    pub health: HealthState,
    pub fda: FdaState,
    pub uptime_seconds: Option<u64>,
    pub endpoint: String,
    pub username: String,
    pub password_state: PasswordState,
    pub authentication_enabled: bool,
    pub action_capabilities: ActionCapabilities,
    pub installed_version: Option<String>,
    pub running_version: Option<String>,
    pub version_pending: bool,
    pub config_pending: bool,
    pub config_error: Option<String>,
    pub last_error: Option<String>,
    pub pid: Option<u32>,
    pub stop_grace_remaining_seconds: Option<u64>,
    pub notification: Option<NotificationEvent>,
    pub process_started_at_unix_seconds: Option<u64>,
    /// The parent bundle's `CFBundleVersion` baked into this
    /// OpenCodeServerAgent binary at build time. OpenCodeServer requires it
    /// to match the pending registration transaction before committing
    /// `RegisteredBundleVersion`.
    pub bundle_version: String,
}

/// The status fields that subscribers care about. Volatile counters
/// (uptimes, remaining grace seconds) are excluded so a quiet system does
/// not produce a push every second; OpenCodeServer recomputes those labels
/// locally from `process_started_at_unix_seconds` while the menu is open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusFingerprint {
    pub desired_state: DesiredState,
    pub server_state: ServerState,
    pub health: HealthState,
    pub fda: FdaState,
    pub endpoint: String,
    pub username: String,
    pub password_state: PasswordState,
    pub action_capabilities: ActionCapabilities,
    pub installed_version: Option<String>,
    pub running_version: Option<String>,
    pub version_pending: bool,
    pub config_pending: bool,
    pub config_error: Option<String>,
    pub last_error: Option<String>,
    pub pid: Option<u32>,
    pub notification: Option<NotificationEvent>,
    pub process_started_at_unix_seconds: Option<u64>,
}

impl From<&Status> for StatusFingerprint {
    fn from(status: &Status) -> Self {
        Self {
            desired_state: status.desired_state,
            server_state: status.server_state,
            health: status.health,
            fda: status.fda,
            endpoint: status.endpoint.clone(),
            username: status.username.clone(),
            password_state: status.password_state,
            action_capabilities: status.action_capabilities,
            installed_version: status.installed_version.clone(),
            running_version: status.running_version.clone(),
            version_pending: status.version_pending,
            config_pending: status.config_pending,
            config_error: status.config_error.clone(),
            last_error: status.last_error.clone(),
            pid: status.pid,
            notification: status.notification.clone(),
            process_started_at_unix_seconds: status.process_started_at_unix_seconds,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Response {
    pub version: u32,
    pub ok: bool,
    pub error: Option<String>,
    pub status: Option<Status>,
    pub validation: Option<ValidationReport>,
}

impl Response {
    pub fn success(status: Status) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: true,
            error: None,
            status: Some(status),
            validation: None,
        }
    }

    pub fn validation(report: ValidationReport, status: Status) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: report.valid,
            error: None,
            status: Some(status),
            validation: Some(report),
        }
    }

    pub fn error(message: impl Into<String>, status: Option<Status>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: false,
            error: Some(message.into()),
            status,
            validation: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_encoding_is_stable() {
        let json = serde_json::to_string(&Request::new(Command::ForceStop)).expect("json");
        assert_eq!(json, r#"{"version":6,"command":"force_stop"}"#);
    }

    #[test]
    fn subscribe_encoding_is_stable() {
        let json = serde_json::to_string(&Request::new(Command::Subscribe)).expect("json");
        assert_eq!(json, r#"{"version":6,"command":"subscribe"}"#);
    }

    #[test]
    fn refresh_credentials_encoding_is_stable() {
        let json = serde_json::to_string(&Request::new(Command::RefreshCredentials)).expect("json");
        assert_eq!(json, r#"{"version":6,"command":"refresh_credentials"}"#);
    }

    #[test]
    fn credential_changed_encoding_is_stable() {
        let json = serde_json::to_string(&Request::new(Command::CredentialChanged)).expect("json");
        assert_eq!(json, r#"{"version":6,"command":"credential_changed"}"#);
    }

    #[test]
    fn credential_removed_encoding_is_stable() {
        let json = serde_json::to_string(&Request::new(Command::CredentialRemoved)).expect("json");
        assert_eq!(json, r#"{"version":6,"command":"credential_removed"}"#);
    }

    #[test]
    fn fingerprint_ignores_volatile_counters() {
        let base: Status = serde_json::from_value(serde_json::json!({
            "protocol_version": 6,
            "agent_version": "test",
            "agent_uptime_seconds": 10,
            "desired_state": "running",
            "server_state": "healthy",
            "health": "healthy",
            "fda": "verified",
            "uptime_seconds": 5,
            "endpoint": "127.0.0.1:4096",
            "username": "opencode",
            "password_state": "configured",
            "authentication_enabled": false,
            "action_capabilities": {
                "start": false,
                "stop": true,
                "restart": true,
                "continue_stop": false,
                "force_stop": false
            },
            "installed_version": "1.0.0",
            "running_version": "1.0.0",
            "version_pending": false,
            "config_pending": false,
            "config_error": null,
            "last_error": null,
            "pid": 42,
            "stop_grace_remaining_seconds": 3,
            "notification": null,
            "process_started_at_unix_seconds": 100,
            "bundle_version": "57"
        }))
        .expect("status");
        assert_eq!(
            base.action_capabilities,
            ActionCapabilities {
                start: false,
                stop: true,
                restart: true,
                continue_stop: false,
                force_stop: false,
            }
        );
        let mut later = base.clone();
        later.agent_uptime_seconds = 11;
        later.uptime_seconds = Some(6);
        later.stop_grace_remaining_seconds = Some(2);
        assert_eq!(
            StatusFingerprint::from(&base),
            StatusFingerprint::from(&later)
        );

        let mut changed = base.clone();
        changed.server_state = ServerState::Stopping;
        assert_ne!(
            StatusFingerprint::from(&base),
            StatusFingerprint::from(&changed)
        );

        let mut capability_changed = base.clone();
        capability_changed.action_capabilities.start = true;
        assert_ne!(
            StatusFingerprint::from(&base),
            StatusFingerprint::from(&capability_changed)
        );
    }
}
