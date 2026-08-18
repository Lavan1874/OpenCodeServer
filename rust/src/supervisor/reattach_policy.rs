//! Pure reattachment decision logic extracted from `Supervisor::try_reattach`
//! (see `docs/refactor/reattachment-policy-boundary.md`).
//!
//! This module is deliberately stateless and side-effect-free: every
//! function receives already-collected facts (kernel identity inspection
//! results, the health-check outcome, configuration presence/match, the
//! credential state) and returns an action for the orchestrator to execute.
//! It performs no I/O, logs nothing, holds no state, and never touches
//! `Supervisor` — the orchestrator owns the kernel identity inspections and
//! the health check, and the existing Supervisor helpers own every side
//! effect. The decision is split into two phases because the health check
//! connects to the recorded process's endpoint and must stay conditional:
//! gates 0–3 failing means the process may be foreign or dead, so probing
//! it would be a behavior change.

use super::CredentialState;
use crate::config::ValidatedConfig;
use crate::process::RecordIdentity;

/// The health-check fact the orchestrator collected, in the tri-state the
/// decision needs: healthy (with the reported version), reachable but not
/// healthy, or failed/unreachable. The orchestrator digests the real
/// `health::check` result into this verdict before calling the policy, so
/// the policy has no coupling to the health module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HealthVerdict<'a> {
    Healthy { version: &'a str },
    Unhealthy,
    Failed,
}

/// The terminal decisions of the first phase (gates 0–3) plus the request
/// to run the conditional health check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum InitialAction {
    DiscardStaleRecord {
        reason: &'static str,
    },
    MarkUnverified {
        reason: &'static str,
    },
    AttachUnconfirmed {
        reason: &'static str,
    },
    AttachStaleConfig {
        reason: &'static str,
    },
    /// Run the authenticated health check and re-inspect the identity, then
    /// call `decide_after_health`. Carries the configuration the health
    /// check and the eventual success block need, so the health phase has a
    /// configuration by construction.
    NeedsHealthCheck {
        config: ValidatedConfig,
    },
}

/// The decisions of the second phase (gate 4), made after the authenticated
/// health check and the second identity inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum FinalAction {
    /// Every check passed: reattach with the observed version under the
    /// verified configuration. The orchestrator re-stamps the record
    /// fingerprint (it owns the fingerprint key) and applies the success
    /// block.
    ReattachHealthy {
        version: String,
        config: ValidatedConfig,
    },
    DiscardStaleRecord {
        reason: &'static str,
    },
    MarkUnverified {
        reason: &'static str,
    },
    AttachUnconfirmed {
        reason: &'static str,
    },
}

/// Encodes gates 0–3 of the reattachment decision.
///
/// Gate 0: a record whose kernel identity was never confirmed at spawn can
/// authorize no signal and no takeover. Only a provably gone PID makes it
/// stale; any other observation keeps it unverified.
///
/// Gate 1: the first identity inspection. Missing and mismatched records
/// are discarded without signaling; an executable mismatch attaches
/// unconfirmed; a group escape or an inspection error keeps the record
/// unverified (fail-closed). `Current` and `ExecutableVanished` proceed —
/// the orchestrator logs the vanished-executable Notice itself.
///
/// Gate 2: without a configuration nothing further can be verified.
///
/// Gate 3: an identity-verified process whose configuration fingerprint no
/// longer matches is never abandoned — it is adopted as a managed
/// stale-configuration process. The reason distinguishes the pending
/// Keychain grant from an ordinary configuration change.
pub(super) fn decide_initial(
    identity_unconfirmed: bool,
    identity: Result<RecordIdentity, ()>,
    config: Option<ValidatedConfig>,
    config_matches: bool,
    credential_state: CredentialState,
) -> InitialAction {
    if identity_unconfirmed {
        return match identity {
            Ok(RecordIdentity::Missing) => InitialAction::DiscardStaleRecord {
                reason: "the unconfirmed recorded PID is no longer running",
            },
            _ => InitialAction::MarkUnverified {
                reason: "the recorded process identity was never confirmed after spawn",
            },
        };
    }
    match identity {
        Ok(RecordIdentity::Missing) => {
            return InitialAction::DiscardStaleRecord {
                reason: "recorded PID is no longer running",
            };
        }
        Ok(RecordIdentity::Mismatched) => {
            return InitialAction::DiscardStaleRecord {
                reason: "recorded PID no longer has the recorded process identity",
            };
        }
        Ok(RecordIdentity::Current | RecordIdentity::ExecutableVanished) => {}
        Ok(RecordIdentity::ExecutableMismatch) => {
            return InitialAction::AttachUnconfirmed {
                reason: "executable identity could not be confirmed",
            };
        }
        Ok(RecordIdentity::GroupEscaped) => {
            return InitialAction::MarkUnverified {
                reason: "the process kept its identity but abandoned its dedicated process group",
            };
        }
        Err(_) => {
            return InitialAction::MarkUnverified {
                reason: "process identity could not be inspected",
            };
        }
    }
    let Some(config) = config else {
        return InitialAction::MarkUnverified {
            reason: "configuration is unavailable",
        };
    };
    if !config_matches {
        // An unauthorized Keychain read merges an empty password and makes a
        // genuine configuration match look like a change. Say so explicitly;
        // the identity-verified process is adopted either way, because
        // identity evidence — not the configuration fingerprint — is what
        // authorizes later signals.
        let reason = if credential_state == CredentialState::AccessPending {
            "grant Keychain access, then restart"
        } else {
            "restart to apply the changes"
        };
        return InitialAction::AttachStaleConfig { reason };
    }
    InitialAction::NeedsHealthCheck { config }
}

/// Encodes gate 4 of the reattachment decision: the authenticated health
/// answer plus the post-health identity re-inspection.
///
/// Only a healthy answer AND a re-inspection that still sees the recorded
/// kernel identity (`Current` or `ExecutableVanished`) reattach. Every
/// other combination discards, attaches unconfirmed, or keeps the record
/// unverified with a gate-4-specific reason. An unhealthy answer or a
/// failed health check never reattaches and never signals.
pub(super) fn decide_after_health(
    identity: Result<RecordIdentity, ()>,
    health: HealthVerdict<'_>,
    config: ValidatedConfig,
) -> FinalAction {
    match health {
        HealthVerdict::Healthy { version } => match identity {
            Ok(RecordIdentity::Current | RecordIdentity::ExecutableVanished) => {
                FinalAction::ReattachHealthy {
                    version: version.to_owned(),
                    config,
                }
            }
            Ok(RecordIdentity::ExecutableMismatch) => FinalAction::AttachUnconfirmed {
                reason: "executable identity changed during reattachment",
            },
            Ok(RecordIdentity::GroupEscaped) => FinalAction::MarkUnverified {
                reason: "the process abandoned its dedicated process group during reattachment",
            },
            Ok(RecordIdentity::Missing) => FinalAction::DiscardStaleRecord {
                reason: "recorded process exited during reattachment",
            },
            Ok(RecordIdentity::Mismatched) => FinalAction::DiscardStaleRecord {
                reason: "recorded process identity changed during reattachment",
            },
            Err(_) => FinalAction::MarkUnverified {
                reason: "process identity could not be rechecked after health verification",
            },
        },
        HealthVerdict::Unhealthy => FinalAction::MarkUnverified {
            reason: "health endpoint did not report healthy",
        },
        HealthVerdict::Failed => FinalAction::MarkUnverified {
            reason: "health endpoint could not be verified",
        },
    }
}
