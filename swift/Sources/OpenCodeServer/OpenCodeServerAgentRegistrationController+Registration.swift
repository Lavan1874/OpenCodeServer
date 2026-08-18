import Foundation
import OSLog
import ServiceManagement

/// Registration ordering and the Service Management state-observation waits.
extension OpenCodeServerAgentRegistrationController {
    func beginRegistration(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        guard phase == .idle else { return }
        generation += 1
        performRegistration(purpose: purpose, attempt: attempt)
    }

    func beginReplacement(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        guard phase == .idle else { return }
        generation += 1
        startReplacement(purpose: purpose, attempt: attempt)
    }

    func resume(
        _ transaction: OpenCodeServerAgentRegistrationTransaction
    ) {
        guard phase == .idle else { return }
        generation += 1
        let action = registrationTransactionCoordinator.recoveryAction(
            for: transaction,
            serviceIsEnabled: openCodeServerAgent.status == .enabled,
            serviceIsNotRegistered: openCodeServerAgent.status == .notRegistered
        )
        switch action {
        case .waitForUnregistered:
            waitForUnregistered(
                purpose: transaction.purpose,
                attempt: transaction.attempt,
                checksRemaining: Self.unregistrationStatusChecks
            )
        case .startReplacement:
            startReplacement(
                purpose: transaction.purpose,
                attempt: transaction.attempt
            )
        case .scheduleRegistration:
            scheduleRegistration(
                purpose: transaction.purpose,
                attempt: transaction.attempt
            )
        case .awaitIPC:
            beginAwaitingIPC(
                purpose: transaction.purpose,
                attempt: transaction.attempt
            )
        case .performRegistration:
            performRegistration(
                purpose: transaction.purpose,
                attempt: transaction.attempt
            )
        }
    }

    func beginAwaitingIPC(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        let checks = ipcVerificationChecksForCurrentContext()
        phase = .awaitingIPC(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt,
            checksRemaining: checks
        )
        unverifiedReachabilityLogged = false
        onRegistrationVerificationPendingChange?(true)
        waitForIPC(
            purpose: purpose,
            attempt: attempt,
            checksRemaining: checks
        )
    }

    func persist(
        phase: OpenCodeServerAgentRegistrationTransactionPhase,
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) -> Bool {
        do {
            try registrationTransactionCoordinator.save(
                phase: phase,
                purpose: purpose,
                attempt: attempt
            )
            return true
        } catch {
            logger.error(
                "Could not persist the OpenCodeServerAgent registration transaction: \(error.localizedDescription, privacy: .public)"
            )
            return false
        }
    }

    func startReplacement(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        guard openCodeServerAgent.status == .enabled else {
            performRegistration(purpose: purpose, attempt: attempt)
            return
        }
        phase = .unregistering(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt
        )
        guard persist(
            phase: .unregistering,
            purpose: purpose,
            attempt: attempt
        ) else {
            fail(
                error: OpenCodeServerAgentServiceError.registrationTransactionCouldNotBeSaved,
                message: "OpenCodeServerAgent replacement could not persist its transaction before unregistering"
            )
            return
        }
        logger.notice(
            "Unregistering OpenCodeServerAgent before \(purpose == .bundleUpgrade ? "bundle upgrade" : "explicit repair", privacy: .public) attempt \(attempt + 1, privacy: .public)"
        )
        openCodeServerAgent.unregister { [weak self] error in
            self?.handleUnregistration(
                error,
                purpose: purpose,
                attempt: attempt
            )
        }
    }

    func handleUnregistration(
        _ error: Error?,
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        guard phase == .unregistering(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt
        ) else {
            return
        }
        if let error, openCodeServerAgent.status != .notRegistered {
            fail(
                error: error,
                message: "OpenCodeServerAgent replacement could not unregister the previous registration"
            )
            return
        }
        logger.notice("OpenCodeServerAgent unregistration completed")
        waitForUnregistered(
            purpose: purpose,
            attempt: attempt,
            checksRemaining: Self.unregistrationStatusChecks
        )
    }

    func waitForUnregistered(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int,
        checksRemaining: Int
    ) {
        phase = .awaitingUnregistered(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt,
            checksRemaining: checksRemaining
        )
        if checksRemaining == Self.unregistrationStatusChecks,
           !persist(
               phase: .awaitingUnregistered,
               purpose: purpose,
               attempt: attempt
           )
        {
            fail(
                error: OpenCodeServerAgentServiceError.registrationTransactionCouldNotBeSaved,
                message: "OpenCodeServerAgent replacement could not persist its unregistration state"
            )
            return
        }
        if openCodeServerAgent.status == .notRegistered {
            scheduleRegistration(purpose: purpose, attempt: attempt)
            return
        }
        if openCodeServerAgent.status == .requiresApproval {
            fail(
                error: OpenCodeServerAgentServiceError.requiresApproval,
                message: "OpenCodeServerAgent replacement requires user approval"
            )
            return
        }
        guard checksRemaining > 0 else {
            fail(
                error: OpenCodeServerAgentServiceError.statusDidNotBecomeUnregistered,
                message: "OpenCodeServerAgent did not reach the unregistered state"
            )
            return
        }
        let currentGeneration = generation
        scheduler.schedule(after: 0.1) { [weak self] in
            guard let self, self.generation == currentGeneration else { return }
            self.waitForUnregistered(
                purpose: purpose,
                attempt: attempt,
                checksRemaining: checksRemaining - 1
            )
        }
    }

    func scheduleRegistration(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        phase = .awaitingRegistration(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt
        )
        guard persist(
            phase: .awaitingRegistration,
            purpose: purpose,
            attempt: attempt
        ) else {
            fail(
                error: OpenCodeServerAgentServiceError.registrationTransactionCouldNotBeSaved,
                message: "OpenCodeServerAgent replacement could not persist its registration state"
            )
            return
        }
        let currentGeneration = generation
        scheduler.schedule(after: registrationSettleDelay(attempt: attempt)) {
            [weak self] in
            guard let self,
                  self.generation == currentGeneration,
                  self.phase == .awaitingRegistration(
                      version: self.bundleVersion,
                      purpose: purpose,
                      attempt: attempt
                  )
            else {
                return
            }
            self.performRegistration(purpose: purpose, attempt: attempt)
        }
    }

    func performRegistration(
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) {
        phase = .awaitingRegistration(
            version: bundleVersion,
            purpose: purpose,
            attempt: attempt
        )
        guard persist(
            phase: .awaitingRegistration,
            purpose: purpose,
            attempt: attempt
        ) else {
            fail(
                error: OpenCodeServerAgentServiceError.registrationTransactionCouldNotBeSaved,
                message: "OpenCodeServerAgent registration could not persist its registration state"
            )
            return
        }
        var registrationError: Error?
        do {
            try openCodeServerAgent.register()
        } catch {
            registrationError = error
        }
        if let registrationError,
           openCodeServerAgent.status != .enabled
        {
            fail(error: registrationError, message: "OpenCodeServerAgent registration failed")
            return
        }
        guard openCodeServerAgent.status != .requiresApproval else {
            fail(
                error: OpenCodeServerAgentServiceError.requiresApproval,
                message: "OpenCodeServerAgent registration requires user approval"
            )
            return
        }
        guard persist(
            phase: .awaitingIPC,
            purpose: purpose,
            attempt: attempt
        ) else {
            fail(
                error: OpenCodeServerAgentServiceError.registrationTransactionCouldNotBeSaved,
                message: "OpenCodeServerAgent registration could not persist its IPC verification state"
            )
            return
        }
        if let registrationError {
            logger.error(
                "OpenCodeServerAgent registration attempt \(attempt + 1, privacy: .public) returned an error while Service Management remained enabled; awaiting authenticated IPC: \(registrationError.localizedDescription, privacy: .public)"
            )
        } else {
            logger.notice(
                "OpenCodeServerAgent registration attempt \(attempt + 1, privacy: .public) returned successfully for bundle version \(self.bundleVersion, privacy: .public); awaiting authenticated IPC"
            )
        }
        beginAwaitingIPC(purpose: purpose, attempt: attempt)
    }

    func registrationSettleDelay(attempt: Int) -> TimeInterval {
        switch attempt {
        case 0: 0.5
        case 1: 1
        default: 2
        }
    }
}
