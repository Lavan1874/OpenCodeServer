//! Exhaustive pure-decision tables for the reattachment policy.
//!
//! The core deliverable of the extraction: every
//! `(identity_unconfirmed, identity, config, config_matches,
//! credential_state) → InitialAction` and `(identity2, health) →
//! FinalAction` combination is locked here without a `Supervisor`, a live
//! process, a health endpoint, or any I/O — which is exactly what was
//! impossible while the decision lived inside `try_reattach`.

use super::CredentialState;
use super::reattach_policy::{
    FinalAction, HealthVerdict, InitialAction, decide_after_health, decide_initial,
};
use crate::config::{ConfigFile, ValidatedConfig};
use crate::process::RecordIdentity;
use std::path::PathBuf;

fn config() -> ValidatedConfig {
    ValidatedConfig {
        source: ConfigFile::default(),
        configured_executable: PathBuf::from("/opt/homebrew/bin/opencode"),
        canonical_executable: PathBuf::from("/opt/homebrew/bin/opencode"),
        effective_username: "test-user".to_owned(),
    }
}

#[test]
fn unconfirmed_records_discard_only_a_provably_gone_pid() {
    for identity in [
        RecordIdentity::Current,
        RecordIdentity::ExecutableVanished,
        RecordIdentity::ExecutableMismatch,
        RecordIdentity::GroupEscaped,
        RecordIdentity::Mismatched,
    ] {
        assert_eq!(
            decide_initial(
                true,
                Ok(identity),
                Some(config()),
                true,
                CredentialState::Available,
            ),
            InitialAction::MarkUnverified {
                reason: "the recorded process identity was never confirmed after spawn",
            },
            "unconfirmed + {identity:?} must stay unverified"
        );
    }
    assert_eq!(
        decide_initial(
            true,
            Err(()),
            Some(config()),
            true,
            CredentialState::Available,
        ),
        InitialAction::MarkUnverified {
            reason: "the recorded process identity was never confirmed after spawn",
        },
        "unconfirmed + inspection error must stay unverified"
    );
    assert_eq!(
        decide_initial(
            true,
            Ok(RecordIdentity::Missing),
            Some(config()),
            true,
            CredentialState::Available,
        ),
        InitialAction::DiscardStaleRecord {
            reason: "the unconfirmed recorded PID is no longer running",
        },
        "a provably gone PID makes even an unconfirmed record stale"
    );
}

#[test]
fn the_unconfirmed_gate_never_reads_the_configuration() {
    // Gate 0 precedes gates 2/3: no configuration and no fingerprint match
    // change the unconfirmed outcome.
    assert_eq!(
        decide_initial(
            true,
            Ok(RecordIdentity::Current),
            None,
            false,
            CredentialState::NotConfigured,
        ),
        InitialAction::MarkUnverified {
            reason: "the recorded process identity was never confirmed after spawn",
        },
    );
    assert_eq!(
        decide_initial(
            true,
            Ok(RecordIdentity::Missing),
            None,
            false,
            CredentialState::NotConfigured,
        ),
        InitialAction::DiscardStaleRecord {
            reason: "the unconfirmed recorded PID is no longer running",
        },
    );
}

#[test]
fn confirmed_missing_or_mismatched_records_are_discarded() {
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::Missing),
            None,
            false,
            CredentialState::NotConfigured,
        ),
        InitialAction::DiscardStaleRecord {
            reason: "recorded PID is no longer running",
        },
    );
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::Mismatched),
            None,
            false,
            CredentialState::NotConfigured,
        ),
        InitialAction::DiscardStaleRecord {
            reason: "recorded PID no longer has the recorded process identity",
        },
    );
}

#[test]
fn a_confirmed_executable_mismatch_attaches_unconfirmed() {
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::ExecutableMismatch),
            None,
            false,
            CredentialState::NotConfigured,
        ),
        InitialAction::AttachUnconfirmed {
            reason: "executable identity could not be confirmed",
        },
    );
}

#[test]
fn a_confirmed_group_escape_stays_unverified() {
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::GroupEscaped),
            None,
            false,
            CredentialState::NotConfigured,
        ),
        InitialAction::MarkUnverified {
            reason: "the process kept its identity but abandoned its dedicated process group",
        },
    );
}

#[test]
fn an_inspection_error_stays_unverified_fail_closed() {
    assert_eq!(
        decide_initial(false, Err(()), None, false, CredentialState::NotConfigured,),
        InitialAction::MarkUnverified {
            reason: "process identity could not be inspected",
        },
    );
}

#[test]
fn a_missing_configuration_stays_unverified() {
    for identity in [RecordIdentity::Current, RecordIdentity::ExecutableVanished] {
        assert_eq!(
            decide_initial(
                false,
                Ok(identity),
                None,
                false,
                CredentialState::NotConfigured,
            ),
            InitialAction::MarkUnverified {
                reason: "configuration is unavailable",
            },
            "{identity:?} without a configuration must stay unverified"
        );
    }
}

#[test]
fn the_configuration_mismatch_reason_tracks_the_credential_state() {
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::Current),
            Some(config()),
            false,
            CredentialState::AccessPending,
        ),
        InitialAction::AttachStaleConfig {
            reason: "grant Keychain access, then restart",
        },
    );
    for credential_state in [CredentialState::Available, CredentialState::NotConfigured] {
        assert_eq!(
            decide_initial(
                false,
                Ok(RecordIdentity::Current),
                Some(config()),
                false,
                credential_state,
            ),
            InitialAction::AttachStaleConfig {
                reason: "restart to apply the changes",
            },
            "{credential_state:?} must name the plain restart remedy"
        );
    }
    // ExecutableVanished continues to the configuration gates too.
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::ExecutableVanished),
            Some(config()),
            false,
            CredentialState::Available,
        ),
        InitialAction::AttachStaleConfig {
            reason: "restart to apply the changes",
        },
    );
}

#[test]
fn a_matching_configuration_requests_the_conditional_health_check() {
    for credential_state in [
        CredentialState::NotConfigured,
        CredentialState::AccessPending,
        CredentialState::Available,
    ] {
        assert_eq!(
            decide_initial(
                false,
                Ok(RecordIdentity::Current),
                Some(config()),
                true,
                credential_state,
            ),
            InitialAction::NeedsHealthCheck { config: config() },
            "{credential_state:?} must not block the health phase"
        );
    }
    assert_eq!(
        decide_initial(
            false,
            Ok(RecordIdentity::ExecutableVanished),
            Some(config()),
            true,
            CredentialState::Available,
        ),
        InitialAction::NeedsHealthCheck { config: config() },
    );
}

#[test]
fn a_healthy_answer_with_intact_identity_reattaches() {
    let expected = FinalAction::ReattachHealthy {
        version: "fixture-1".to_owned(),
        config: config(),
    };
    assert_eq!(
        decide_after_health(
            Ok(RecordIdentity::Current),
            HealthVerdict::Healthy {
                version: "fixture-1",
            },
            config(),
        ),
        expected,
    );
    // A vanished executable file does not weaken the kernel identity
    // evidence: pid + start + uid + group still pin the process.
    assert_eq!(
        decide_after_health(
            Ok(RecordIdentity::ExecutableVanished),
            HealthVerdict::Healthy {
                version: "fixture-1",
            },
            config(),
        ),
        expected,
    );
}

#[test]
fn the_post_health_identity_recheck_maps_each_arm() {
    let health = HealthVerdict::Healthy {
        version: "fixture-1",
    };
    assert_eq!(
        decide_after_health(Ok(RecordIdentity::ExecutableMismatch), health, config()),
        FinalAction::AttachUnconfirmed {
            reason: "executable identity changed during reattachment",
        },
    );
    assert_eq!(
        decide_after_health(Ok(RecordIdentity::GroupEscaped), health, config()),
        FinalAction::MarkUnverified {
            reason: "the process abandoned its dedicated process group during reattachment",
        },
    );
    assert_eq!(
        decide_after_health(Ok(RecordIdentity::Missing), health, config()),
        FinalAction::DiscardStaleRecord {
            reason: "recorded process exited during reattachment",
        },
    );
    assert_eq!(
        decide_after_health(Ok(RecordIdentity::Mismatched), health, config()),
        FinalAction::DiscardStaleRecord {
            reason: "recorded process identity changed during reattachment",
        },
    );
    assert_eq!(
        decide_after_health(Err(()), health, config()),
        FinalAction::MarkUnverified {
            reason: "process identity could not be rechecked after health verification",
        },
    );
}

#[test]
fn an_unhealthy_answer_never_reattaches() {
    for identity in [
        Ok(RecordIdentity::Current),
        Ok(RecordIdentity::ExecutableVanished),
        Ok(RecordIdentity::ExecutableMismatch),
        Ok(RecordIdentity::GroupEscaped),
        Ok(RecordIdentity::Missing),
        Ok(RecordIdentity::Mismatched),
        Err(()),
    ] {
        assert_eq!(
            decide_after_health(identity, HealthVerdict::Unhealthy, config()),
            FinalAction::MarkUnverified {
                reason: "health endpoint did not report healthy",
            },
            "{identity:?} with an unhealthy answer must stay unverified"
        );
    }
}

#[test]
fn a_failed_health_check_never_reattaches() {
    for identity in [
        Ok(RecordIdentity::Current),
        Ok(RecordIdentity::ExecutableVanished),
        Ok(RecordIdentity::ExecutableMismatch),
        Ok(RecordIdentity::GroupEscaped),
        Ok(RecordIdentity::Missing),
        Ok(RecordIdentity::Mismatched),
        Err(()),
    ] {
        assert_eq!(
            decide_after_health(identity, HealthVerdict::Failed, config()),
            FinalAction::MarkUnverified {
                reason: "health endpoint could not be verified",
            },
            "{identity:?} with a failed health check must stay unverified"
        );
    }
}
