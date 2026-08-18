import Foundation

/// Attribute-only observations used to reconcile a migration after a
/// restart. An error is represented as `uncertain`, never as item absence.
enum CredentialMigrationProbeResult: Sendable, Equatable {
    case observed(newExists: Bool, oldExists: Bool?)
    case uncertain
}

enum CredentialMigrationRecoveryDecision: Sendable, Equatable {
    /// The persisted configuration still names the old account. The new item
    /// is inactive and can be removed without notifying OpenCodeServerAgent.
    case removeNew
    /// The persisted configuration names the new account. The new item is
    /// authoritative; the old item is an inactive cleanup candidate.
    case useNew(oldExists: Bool?)
    /// The create did not leave a new item and configuration still names the
    /// old account, so the intent can be discarded.
    case discard
    /// The account or attribute-only observation is not sufficiently certain.
    case hold
}

enum CredentialMigrationRecoveryError: LocalizedError {
    case configurationUnavailable
    case configurationChanged

    var errorDescription: String? {
        switch self {
        case .configurationUnavailable:
            "The current configuration could not be checked safely; credential cleanup remains pending."
        case .configurationChanged:
            "The current configuration changed before credential cleanup; the inactive item remains pending."
        }
    }
}

enum CredentialMigrationRecovery {
    static func decide(
        record: CredentialMigrationRecord,
        currentAccount: String?,
        probe: CredentialMigrationProbeResult
    ) -> CredentialMigrationRecoveryDecision {
        guard case let .observed(newExists, oldExists) = probe,
              let currentAccount,
              let oldAccount = record.oldAccount,
              oldAccount != record.newAccount
        else {
            return .hold
        }

        if currentAccount != record.newAccount {
            // A record with a known old account may only be reconciled when
            // the persisted configuration still names that exact account.
            // An unrelated account is an ambiguous concurrent/configuration
            // change; retain the intent rather than deleting a possibly
            // active credential.
            guard record.oldAccount == currentAccount else {
                return .hold
            }
            return newExists ? .removeNew : .discard
        }

        guard newExists else {
            return .hold
        }
        return .useNew(oldExists: oldExists)
    }
}

/// Reconciles one durable migration intent. It deliberately owns only the
/// migration state and its bounded worker; notice delivery remains the
/// responsibility of CredentialMutationCoordinator.
@MainActor
final class CredentialMigrationReconciler {
    typealias ConfigurationLoad = @Sendable () throws -> AppConfig
    typealias AttributeProbe = @Sendable (String) throws -> Bool
    typealias Delete = @Sendable (String) throws -> Void

    let journal: CredentialMutationJournal
    let worker = DispatchQueue(
        label: "ai.opencode.server.credential-migration",
        qos: .utility
    )
    var inFlightGeneration: UInt64?

    var onStateChange: (() -> Void)?
    var onNeedsDelivery: (() -> Void)?
    var onReadyToDrain: (() -> Void)?
    var onRecoveryFailure: ((String) -> Void)?

    init(journal: CredentialMutationJournal) {
        self.journal = journal
    }

    var record: CredentialMigrationRecord? {
        journal.migration
    }

    var recoveryInFlight: Bool {
        inFlightGeneration != nil
    }

    @discardableResult
    func stage(oldAccount: String?, newAccount: String) throws -> UInt64 {
        try journal.stageMigration(oldAccount: oldAccount, newAccount: newAccount)
    }

    func mark(generation: UInt64, phase: CredentialMigrationPhase) throws {
        try journal.setMigrationPhase(generation: generation, phase: phase)
    }

    func commit(generation: UInt64) throws {
        try journal.commitMigration(generation: generation)
    }

    func complete(generation: UInt64) throws {
        try journal.completeMigration(generation: generation)
    }

    func rollback(generation: UInt64) throws {
        try journal.rollbackMigration(generation: generation)
    }

    func recover(
        currentConfiguration: @escaping ConfigurationLoad,
        contains: @escaping AttributeProbe,
        delete: @escaping Delete
    ) {
        guard inFlightGeneration == nil, let record else { return }
        inFlightGeneration = record.generation
        worker.async { [weak self] in
            let probe: CredentialMigrationProbeResult
            let probeFailure: String?
            do {
                let newExists = try contains(record.newAccount)
                let oldExists = try record.oldAccount.map(contains)
                probe = .observed(newExists: newExists, oldExists: oldExists)
                probeFailure = nil
            } catch {
                probe = .uncertain
                probeFailure = "The attribute-only Keychain probe was inconclusive."
            }
            let currentAccount: String?
            let configurationFailure: String?
            do {
                let configuration = try currentConfiguration()
                currentAccount = KeychainStore.account(forUsername: configuration.username)
                configurationFailure = nil
            } catch {
                currentAccount = nil
                configurationFailure = CredentialMigrationRecoveryError
                    .configurationUnavailable.localizedDescription
            }
            DispatchQueue.main.async {
                guard let self else { return }
                if let probeFailure {
                    self.onRecoveryFailure?(probeFailure)
                }
                if let configurationFailure {
                    self.onRecoveryFailure?(configurationFailure)
                }
                self.finishRecovery(
                    record: record,
                    currentAccount: currentAccount,
                    probe: probe,
                    currentConfiguration: currentConfiguration,
                    delete: delete
                )
            }
        }
    }

    func finishRecovery(
        record: CredentialMigrationRecord,
        currentAccount: String?,
        probe: CredentialMigrationProbeResult,
        currentConfiguration: @escaping ConfigurationLoad,
        delete: @escaping Delete,
        configurationValidated: Bool = false
    ) {
        guard inFlightGeneration == record.generation else { return }
        guard journal.migration?.generation == record.generation else {
            inFlightGeneration = nil
            return
        }

        switch CredentialMigrationRecovery.decide(
            record: record,
            currentAccount: currentAccount,
            probe: probe
        ) {
        case .hold:
            inFlightGeneration = nil
            return
        case .discard:
            discard(record: record)
            inFlightGeneration = nil
        case .removeNew:
            removeInactiveNew(
                record: record,
                currentConfiguration: currentConfiguration,
                delete: delete
            )
        case .useNew(let oldExists):
            useNewConfiguration(
                record: record,
                oldExists: oldExists,
                currentConfiguration: currentConfiguration,
                delete: delete,
                configurationValidated: configurationValidated
            )
        }
    }

}
