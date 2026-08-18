import Foundation
import OSLog
import ServiceManagement

/// Public coordinator entry points and status-facing lifecycle decisions.
extension OpenCodeServerAgentRegistrationController {
    func bootstrap() {
        switch openCodeServerAgent.status {
        case .notRegistered:
            switch registrationTransactionCoordinator.lookup() {
            case let .valid(transaction):
                logger.notice(
                    "OpenCodeServerAgent is not registered; resuming \(transaction.purpose.rawValue, privacy: .public) registration transaction at phase \(transaction.phase.rawValue, privacy: .public), attempt \(transaction.attempt + 1, privacy: .public)"
                )
                resume(transaction)
            case let .staleVersion(transaction):
                logger.notice(
                    "OpenCodeServerAgent transaction targeted bundle version \(transaction.version, privacy: .public); starting a new transaction for \(self.bundleVersion, privacy: .public)"
                )
                beginRegistration(
                    purpose: .bundleUpgrade,
                    attempt: 0
                )
            case .missing:
                logger.notice("OpenCodeServerAgent is not registered; starting initial registration")
                beginRegistration(
                    purpose: .initialRegistration,
                    attempt: 0
                )
            case .invalid:
                failClosed()
            }
        case .enabled:
            switch registrationTransactionCoordinator.lookup() {
            case let .valid(transaction):
                logger.notice(
                    "Resuming the pending OpenCodeServerAgent registration transaction at phase \(transaction.phase.rawValue, privacy: .public), attempt \(transaction.attempt + 1, privacy: .public)"
                )
                resume(transaction)
            case let .staleVersion(transaction):
                logger.notice(
                    "OpenCodeServerAgent transaction targeted bundle version \(transaction.version, privacy: .public); starting a new transaction for \(self.bundleVersion, privacy: .public)"
                )
                beginReplacement(
                    purpose: .bundleUpgrade,
                    attempt: 0
                )
            case .missing:
                let registeredVersion = defaults.string(
                    forKey: Self.registeredBundleVersionKey
                )
                if registeredVersion == bundleVersion {
                    logger.info(
                        "OpenCodeServerAgent registration already matches bundle version \(self.bundleVersion, privacy: .public)"
                    )
                } else {
                    logger.notice(
                        "Updating OpenCodeServerAgent registration for bundle version \(self.bundleVersion, privacy: .public)"
                    )
                    beginReplacement(
                        purpose: .bundleUpgrade,
                        attempt: 0
                    )
                }
            case .invalid:
                failClosed()
            }
        case .requiresApproval:
            logger.notice("OpenCodeServerAgent registration requires user approval")
        case .notFound:
            switch registrationTransactionCoordinator.lookup() {
            case let .valid(transaction):
                logger.notice(
                    "OpenCodeServerAgent was not found by Service Management; resuming the persisted registration transaction at phase \(transaction.phase.rawValue, privacy: .public), attempt \(transaction.attempt + 1, privacy: .public)"
                )
                resume(transaction)
            case let .staleVersion(transaction):
                logger.notice(
                    "OpenCodeServerAgent transaction targeted bundle version \(transaction.version, privacy: .public); starting a new transaction for \(self.bundleVersion, privacy: .public)"
                )
                beginRegistration(
                    purpose: .bundleUpgrade,
                    attempt: 0
                )
            case .missing:
                logger.error(
                    "OpenCodeServerAgent was not found by Service Management; attempting registration for diagnostics"
                )
                beginRegistration(
                    purpose: .initialRegistration,
                    attempt: 0
                )
            case .invalid:
                failClosed()
            }
        @unknown default:
            logger.error("OpenCodeServerAgent registration has an unknown status")
        }
    }

    func observeReachability(_ status: AgentStatus?) {
        guard openCodeServerAgent.status == .enabled,
              case let .awaitingIPC(version, _, _, _) = phase
        else {
            return
        }
        // IPC reachability alone proves nothing about which build answered.
        // Service Management can keep the previous build alive while already
        // accepting the new registration; only the baked bundle version may
        // commit the transaction.
        guard let status else {
            if !unverifiedReachabilityLogged {
                unverifiedReachabilityLogged = true
                logger.notice(
                    "OpenCodeServerAgent IPC is not yet reachable; the registration transaction remains uncommitted"
                )
            }
            return
        }
        guard status.bundleVersion == version else {
            if !unverifiedReachabilityLogged {
                unverifiedReachabilityLogged = true
                logger.notice(
                    "OpenCodeServerAgent IPC reported bundle version \(status.bundleVersion, privacy: .public) but expected \(version, privacy: .public); the registration transaction remains uncommitted"
                )
            }
            return
        }
        complete(version: version)
    }

    func register() {
        switch openCodeServerAgent.status {
        case .enabled:
            return
        case .notRegistered, .notFound:
            switch registrationTransactionCoordinator.lookup() {
            case let .valid(transaction):
                resume(transaction)
            case .staleVersion:
                beginRegistration(
                    purpose: .bundleUpgrade,
                    attempt: 0
                )
            case .missing:
                beginRegistration(
                    purpose: .initialRegistration,
                    attempt: 0
                )
            case .invalid:
                failClosed()
            }
        case .requiresApproval:
            logger.notice("OpenCodeServerAgent registration requires user approval")
        @unknown default:
            logger.error("OpenCodeServerAgent registration has an unknown status")
        }
    }

    func repair(completion: @escaping (Error?) -> Void) {
        switch phase {
        case .idle:
            break
        case .awaitingIPC, .retryScheduled, .failed:
            cancel()
        case .unregistering, .awaitingUnregistered, .awaitingRegistration:
            completion(OpenCodeServerAgentServiceError.operationInProgress)
            return
        }
        switch openCodeServerAgent.status {
        case .enabled:
            explicitRepairCompletion = completion
            beginReplacement(
                purpose: .explicitRepair,
                attempt: 0
            )
        case .notRegistered, .notFound:
            explicitRepairCompletion = completion
            beginRegistration(
                purpose: .explicitRepair,
                attempt: 0
            )
        case .requiresApproval:
            completion(OpenCodeServerAgentServiceError.requiresApproval)
        @unknown default:
            completion(OpenCodeServerAgentServiceError.notFound)
        }
    }

    func cancel() {
        generation += 1
        phase = .idle
        explicitRepairCompletion = nil
        registrationTransactionCoordinator.clear()
        onRegistrationVerificationPendingChange?(false)
    }

    func statusLabel(openCodeServerAgentReachable: Bool) -> String {
        switch openCodeServerAgent.status {
        case .enabled:
            if openCodeServerAgentReachable {
                return "Enabled and Reachable"
            }
            if case .awaitingIPC = phase {
                return "Starting"
            }
            return "Temporarily Unavailable"
        case .requiresApproval:
            return "Requires Approval"
        case .notRegistered:
            return "Not Registered"
        case .notFound:
            return "Not Found in OpenCodeServer"
        @unknown default:
            return "Unknown"
        }
    }
}
