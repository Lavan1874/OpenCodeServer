import Foundation
import OSLog
import ServiceManagement

/// Authenticated IPC verification, bounded retry scheduling, and terminal
/// transaction outcomes.
extension OpenCodeServerAgentRegistrationController {
    func waitForIPC(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int,
        checksRemaining: Int
    ) {
        phase = .awaitingIPC(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt,
            checksRemaining: checksRemaining
        )
        guard checksRemaining > 0 else {
            if purpose == .initialRegistration {
                fail(
                    error: OpenCodeServerAgentServiceError.acceptedRegistrationDidNotBecomeReachable,
                    message: "Initial OpenCodeServerAgent registration remains unverified"
                )
            } else {
                scheduleRetry(purpose: purpose, completedAttempt: attempt)
            }
            return
        }
        let currentGeneration = generation
        scheduler.schedule(after: 2) { [weak self] in
            guard let self,
                  self.generation == currentGeneration,
                  self.phase == .awaitingIPC(
                      version: self.bundleVersion,
                      purpose: purpose,
                      attempt: attempt,
                      checksRemaining: checksRemaining
                  )
            else {
                return
            }
            self.waitForIPC(
                purpose: purpose,
                attempt: attempt,
                checksRemaining: checksRemaining - 1
            )
        }
    }

    func scheduleRetry(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        completedAttempt: Int
    ) {
        let nextAttempt = completedAttempt + 1
        guard nextAttempt < Self.maximumRegistrationAttempts else {
            fail(
                error: OpenCodeServerAgentServiceError.acceptedRegistrationDidNotBecomeReachable,
                message: "OpenCodeServerAgent registration transaction exhausted its bounded attempts"
            )
            return
        }
        phase = .retryScheduled(
            version: bundleVersion,
            purpose: purpose,
            attempt: nextAttempt
        )
        guard persist(
            phase: .retryScheduled,
            purpose: purpose,
            attempt: nextAttempt
        ) else {
            fail(
                error: OpenCodeServerAgentServiceError.registrationTransactionCouldNotBeSaved,
                message: "OpenCodeServerAgent registration retry could not persist its transaction"
            )
            return
        }
        let currentGeneration = generation
        logger.error(
            "Authenticated IPC did not verify OpenCodeServerAgent registration attempt \(completedAttempt + 1, privacy: .public); scheduling bounded attempt \(nextAttempt + 1, privacy: .public)"
        )
        scheduler.schedule(after: retryDelay(attempt: nextAttempt)) { [weak self] in
            guard let self,
                  self.generation == currentGeneration,
                  self.phase == .retryScheduled(
                      version: self.bundleVersion,
                      purpose: purpose,
                      attempt: nextAttempt
                  )
            else {
                return
            }
            self.startReplacement(purpose: purpose, attempt: nextAttempt)
        }
    }

    func complete(version: String) {
        defaults.set(version, forKey: Self.registeredBundleVersionKey)
        registrationTransactionCoordinator.clear()
        logger.notice(
            "OpenCodeServerAgent IPC verified for bundle version \(version, privacy: .public)"
        )
        finishExplicitRepair(error: nil)
        cancel()
    }

    func fail(error: Error, message: String) {
        registrationTransactionCoordinator.clear()
        phase = .failed(version: bundleVersion)
        onRegistrationVerificationPendingChange?(false)
        logger.error(
            "\(message, privacy: .public): \(error.localizedDescription, privacy: .public) OpenCodeServer will retry only on a later launch or explicit repair"
        )
        finishExplicitRepair(error: error)
    }

    func failClosed() {
        phase = .failed(version: bundleVersion)
        onRegistrationVerificationPendingChange?(false)
        logger.error(
            "OpenCodeServerAgent registration is blocked because its persisted transaction is invalid; choose Repair OpenCodeServerAgent to start a new transaction"
        )
    }

    func finishExplicitRepair(error: Error?) {
        let completion = explicitRepairCompletion
        explicitRepairCompletion = nil
        completion?(error)
    }

    func ipcVerificationChecksForCurrentContext() -> Int {
        systemUptime() < Self.coldSystemUptimeThreshold
            ? Self.ipcVerificationChecks
            : Self.interactiveIPCVerificationChecks
    }

    func retryDelay(attempt: Int) -> TimeInterval {
        switch attempt {
        case 1: 1
        default: 2
        }
    }
}
