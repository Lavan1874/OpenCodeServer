import Foundation

/// Cleanup and configuration-boundary steps for CredentialMigrationReconciler.
/// The reconciler keeps the decision and dispatch flow in its main file; this
/// extension owns only the durable cleanup work that follows that decision.
@MainActor
extension CredentialMigrationReconciler {
    func discard(record: CredentialMigrationRecord) {
        // A committed notice for the new account can never be discarded while
        // the configuration names the old account. Keep the record so a
        // later recovery cannot deliver to the wrong account.
        guard journal.pending?.account != record.newAccount else { return }
        do {
            try journal.completeMigration(generation: record.generation)
            onStateChange?()
            onReadyToDrain?()
        } catch {
            // Retain the intent for the next restart.
        }
    }

    func removeInactiveNew(
        record: CredentialMigrationRecord,
        currentConfiguration: @escaping ConfigurationLoad,
        delete: @escaping Delete
    ) {
        guard journal.pending?.account != record.newAccount else {
            inFlightGeneration = nil
            return
        }
        do {
            try journal.setMigrationPhase(
                generation: record.generation,
                phase: .cleanupNew
            )
            onStateChange?()
        } catch {
            inFlightGeneration = nil
            return
        }
        worker.async { [weak self] in
            let result = Result {
                try Self.requireCurrentAccount(
                    record.oldAccount,
                    from: currentConfiguration
                )
                try delete(record.newAccount)
            }
            DispatchQueue.main.async {
                self?.finishInactiveNewCleanup(result, generation: record.generation)
            }
        }
    }

    private func finishInactiveNewCleanup(
        _ result: Result<Void, Error>,
        generation: UInt64
    ) {
        guard case .success = result else {
            if case let .failure(error) = result {
                onRecoveryFailure?(error.localizedDescription)
            }
            inFlightGeneration = nil
            return
        }
        do {
            try journal.rollbackMigration(generation: generation)
            onStateChange?()
            onReadyToDrain?()
        } catch {
            onRecoveryFailure?("The inactive new-account cleanup could not be recorded.")
            // The cleanup landed but its durable completion did not. The
            // retained intent is safe: the next attribute-only probe sees no
            // active new item and can finish the rollback.
        }
        inFlightGeneration = nil
    }

    func useNewConfiguration(
        record: CredentialMigrationRecord,
        oldExists: Bool?,
        currentConfiguration: @escaping ConfigurationLoad,
        delete: @escaping Delete,
        configurationValidated: Bool
    ) {
        let needsNoticeCommit = journal.pending == nil && record.phase != .cleanupOld
        if needsNoticeCommit && !configurationValidated {
            worker.async { [weak self] in
                let result = Result {
                    try Self.requireCurrentAccount(
                        record.newAccount,
                        from: currentConfiguration
                    )
                }
                DispatchQueue.main.async {
                    self?.finishUseNewValidation(
                        result,
                        record: record,
                        oldExists: oldExists,
                        currentConfiguration: currentConfiguration,
                        delete: delete
                    )
                }
            }
            return
        }
        if needsNoticeCommit {
            do {
                try journal.setMigrationPhase(
                    generation: record.generation,
                    phase: .configurationSaved
                )
                try journal.commitMigration(generation: record.generation)
                onStateChange?()
                onNeedsDelivery?()
            } catch {
                inFlightGeneration = nil
                return
            }
        } else if journal.pending?.account != record.newAccount {
            // cleanupOld with no pending notice is the post-ACK state. It is
            // deliberately not rebuilt into another credential_changed
            // notice; only the inactive old item remains to be retried.
            guard record.phase == .cleanupOld, journal.pending == nil else {
                inFlightGeneration = nil
                return
            }
        }

        guard let oldAccount = record.oldAccount, oldExists == true else {
            finishActiveMigration(record: record)
            return
        }
        do {
            try journal.setMigrationPhase(
                generation: record.generation,
                phase: .cleanupOld
            )
            onStateChange?()
        } catch {
            onRecoveryFailure?("The new-account cleanup intent could not be saved.")
            inFlightGeneration = nil
            return
        }
        worker.async { [weak self] in
            let result = Result {
                try Self.requireCurrentAccount(
                    record.newAccount,
                    from: currentConfiguration
                )
                try delete(oldAccount)
            }
            DispatchQueue.main.async {
                self?.finishInactiveOldCleanup(result, generation: record.generation)
            }
        }
    }

    private func finishUseNewValidation(
        _ result: Result<Void, Error>,
        record: CredentialMigrationRecord,
        oldExists: Bool?,
        currentConfiguration: @escaping ConfigurationLoad,
        delete: @escaping Delete
    ) {
        guard inFlightGeneration == record.generation,
              journal.migration?.generation == record.generation
        else {
            inFlightGeneration = nil
            return
        }
        switch result {
        case .success:
            useNewConfiguration(
                record: record,
                oldExists: oldExists,
                currentConfiguration: currentConfiguration,
                delete: delete,
                configurationValidated: true
            )
        case .failure(let error):
            retryRecoveryDecision(
                record: record,
                currentConfiguration: currentConfiguration,
                probe: .observed(
                    newExists: true,
                    oldExists: oldExists
                ),
                delete: delete,
                failure: error
            )
        }
    }

    private func retryRecoveryDecision(
        record: CredentialMigrationRecord,
        currentConfiguration: @escaping ConfigurationLoad,
        probe: CredentialMigrationProbeResult,
        delete: @escaping Delete,
        failure: Error
    ) {
        worker.async { [weak self] in
            let result = Result {
                let configuration = try currentConfiguration()
                return KeychainStore.account(forUsername: configuration.username)
            }
            DispatchQueue.main.async {
                guard let self else { return }
                switch result {
                case .success(let currentAccount):
                    self.finishRecovery(
                        record: record,
                        currentAccount: currentAccount,
                        probe: probe,
                        currentConfiguration: currentConfiguration,
                        delete: delete,
                        configurationValidated: true
                    )
                case .failure:
                    self.onRecoveryFailure?(failure.localizedDescription)
                    self.inFlightGeneration = nil
                }
            }
        }
    }

    private func finishActiveMigration(record: CredentialMigrationRecord) {
        do {
            try journal.completeMigration(generation: record.generation)
            onStateChange?()
            onReadyToDrain?()
        } catch {
            // Leave cleanup intent in place for a later recovery attempt.
        }
        inFlightGeneration = nil
    }

    private func finishInactiveOldCleanup(
        _ result: Result<Void, Error>,
        generation: UInt64
    ) {
        guard case .success = result else {
            if case let .failure(error) = result {
                onRecoveryFailure?(error.localizedDescription)
            }
            inFlightGeneration = nil
            return
        }
        do {
            try journal.completeMigration(generation: generation)
            onStateChange?()
            onReadyToDrain?()
        } catch {
            onRecoveryFailure?("The inactive old-account cleanup could not be recorded.")
            // The old item is already inactive. Retaining cleanupOld is
            // harmless and makes the durable intent retryable.
        }
        inFlightGeneration = nil
    }

    nonisolated private static func requireCurrentAccount(
        _ expectedAccount: String?,
        from currentConfiguration: @escaping ConfigurationLoad
    ) throws {
        guard let expectedAccount else {
            throw CredentialMigrationRecoveryError.configurationUnavailable
        }
        let configuration: AppConfig
        do {
            configuration = try currentConfiguration()
        } catch {
            throw CredentialMigrationRecoveryError.configurationUnavailable
        }
        guard KeychainStore.account(forUsername: configuration.username) == expectedAccount else {
            throw CredentialMigrationRecoveryError.configurationChanged
        }
    }
}
