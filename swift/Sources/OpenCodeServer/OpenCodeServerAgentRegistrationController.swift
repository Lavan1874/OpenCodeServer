import Foundation
import OSLog
import ServiceManagement

/// Owns the bounded OpenCodeServerAgent Service Management transaction. The
/// facade in ServiceController remains responsible for the separate login-item
/// registration and for exposing the public status surface.
@MainActor
final class OpenCodeServerAgentRegistrationController {
    enum Phase: Equatable {
        case idle
        case unregistering(
            version: String,
            purpose: OpenCodeServerAgentRegistrationPurpose,
            attempt: Int
        )
        case awaitingUnregistered(
            version: String,
            purpose: OpenCodeServerAgentRegistrationPurpose,
            attempt: Int,
            checksRemaining: Int
        )
        case awaitingRegistration(
            version: String,
            purpose: OpenCodeServerAgentRegistrationPurpose,
            attempt: Int
        )
        case awaitingIPC(
            version: String,
            purpose: OpenCodeServerAgentRegistrationPurpose,
            attempt: Int,
            checksRemaining: Int
        )
        case retryScheduled(
            version: String,
            purpose: OpenCodeServerAgentRegistrationPurpose,
            attempt: Int
        )
        case failed(version: String)
    }

    static let registeredBundleVersionKey = "RegisteredBundleVersion"
    static let maximumRegistrationAttempts = 3
    static let unregistrationStatusChecks = 50
    /// Cold-system IPC verification window (15 x 2s); see the note on
    /// `interactiveIPCVerificationChecks` for when this budget applies.
    static let ipcVerificationChecks = 15
    /// The macOS 26 launch-constraint evidence (ADR 0006 addendum 2026-08-03):
    /// after a bundle-content change the first registration's spawn is killed
    /// by a stale cached launch constraint (`OS_REASON_CODESIGNING`). Roughly
    /// ten seconds after the latest register — the launchd respawn throttle —
    /// an xpcproxy retry triggers BTM invalidation of the stale item, which is
    /// recreated with fresh constraints; only a register issued *after* that
    /// invalidation binds the job to the fresh item and spawns instantly. A
    /// retry that lands before the invalidation restarts the ten-second
    /// clock, so the interactive window (6 x 2s) places attempt 2's register
    /// at ~14s — comfortably past the observed invalidation point — while a
    /// cold system (login-storm trampoline latency, ADR 0012) keeps the long
    /// window.
    static let interactiveIPCVerificationChecks = 6
    static let coldSystemUptimeThreshold: TimeInterval = 600

    let logger: Logger
    let defaults: UserDefaults
    let registrationTransactionCoordinator: OpenCodeServerAgentRegistrationTransactionCoordinator
    let openCodeServerAgent: any AppServiceControlling
    let scheduler: any ServiceUpdateScheduling
    let bundleVersion: String
    let systemUptime: () -> TimeInterval

    /// Called on the main actor when the registration transaction enters or
    /// leaves the "accepted but unproven IPC" window, so the UI can tighten
    /// status polling while verification is pending instead of waiting out
    /// the subscription's degraded reconnect backoff.
    var onRegistrationVerificationPendingChange: ((Bool) -> Void)?

    var phase = Phase.idle
    var generation = 0
    var explicitRepairCompletion: ((Error?) -> Void)?
    /// Rate-limits the "reachable but unproven build" notice to one entry per
    /// registration transaction (reset when a new IPC verification starts).
    var unverifiedReachabilityLogged = false

    init(
        defaults: UserDefaults,
        openCodeServerAgent: any AppServiceControlling,
        scheduler: any ServiceUpdateScheduling,
        bundleVersion: String,
        systemUptime: @escaping () -> TimeInterval,
        logger: Logger
    ) {
        self.defaults = defaults
        self.openCodeServerAgent = openCodeServerAgent
        self.scheduler = scheduler
        self.bundleVersion = bundleVersion
        self.systemUptime = systemUptime
        self.logger = logger
        registrationTransactionCoordinator =
            OpenCodeServerAgentRegistrationTransactionCoordinator(
                defaults: defaults,
                bundleVersion: bundleVersion,
                maximumAttempts: Self.maximumRegistrationAttempts
            )
    }
}
