import Foundation

/// Two-phase transaction for moving a credential to a new Keychain account.
/// The old item is untouched until the new configuration is durable.
@MainActor
final class CredentialMigrationSaveTransaction {
    private let configStore: ConfigStore
    private let credentialMutations: CredentialMutationCoordinator
    private let keychainCreate: KeychainCreateOperation
    private let keychainDelete: KeychainDeleteOperation
    private let keychainContains: KeychainContainsOperation

    init(
        configStore: ConfigStore,
        credentialMutations: CredentialMutationCoordinator,
        keychainCreate: @escaping KeychainCreateOperation,
        keychainDelete: @escaping KeychainDeleteOperation,
        keychainContains: @escaping KeychainContainsOperation
    ) {
        self.configStore = configStore
        self.credentialMutations = credentialMutations
        self.keychainCreate = keychainCreate
        self.keychainDelete = keychainDelete
        self.keychainContains = keychainContains
    }

    func start(
        mutation: CredentialMutation,
        config: AppConfig,
        completion: @escaping CredentialSaveCompletion
    ) {
        guard case let .create(account, password, oldAccount) = mutation,
              let oldAccount
        else {
            completion(.failed(message: "Invalid credential migration.", configurationSaved: false))
            return
        }
        let generation: UInt64
        do {
            generation = try credentialMutations.stageMigration(
                oldAccount: oldAccount,
                newAccount: account
            )
        } catch {
            completion(.failed(message: error.localizedDescription, configurationSaved: false))
            return
        }
        let create = keychainCreate
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = Result { try create(account, password) }
            DispatchQueue.main.async { [weak self] in
                self?.finishCreate(
                    result: result,
                    config: config,
                    oldAccount: oldAccount,
                    newAccount: account,
                    generation: generation,
                    completion: completion
                )
            }
        }
    }

    private func finishCreate(
        result: Result<KeychainStore.SaveOutcome, Error>,
        config: AppConfig,
        oldAccount: String?,
        newAccount: String,
        generation: UInt64,
        completion: @escaping CredentialSaveCompletion
    ) {
        guard case let .success(outcome) = result, outcome == .created else {
            let message: String
            switch result {
            case .failure(let error): message = error.localizedDescription
            case .success: message = "The new Keychain account was not created."
            }
            rollback(generation: generation, message: message, completion: completion)
            return
        }
        do {
            try credentialMutations.mark(generation: generation, phase: .newCredentialReady)
        } catch {
            completion(.failed(message: error.localizedDescription, configurationSaved: false))
            return
        }
        do {
            try configStore.save(config)
        } catch {
            inspectFailedConfiguration(
                intendedConfig: config,
                newAccount: newAccount,
                generation: generation,
                errorMessage: error.localizedDescription,
                completion: completion
            )
            return
        }
        commitConfiguration(
            oldAccount: oldAccount,
            newAccount: newAccount,
            generation: generation,
            outcome: outcome,
            completion: completion
        )
    }

    private func inspectFailedConfiguration(
        intendedConfig: AppConfig,
        newAccount: String,
        generation: UInt64,
        errorMessage: String,
        completion: @escaping CredentialSaveCompletion
    ) {
        let configurationPaths = configStore.applicationPaths
        let contains = keychainContains
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let inspection: Result<(AppConfig, Bool), Error> = Result {
                let persisted = try ConfigStore(paths: configurationPaths).load()
                return (persisted, try contains(newAccount))
            }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                guard case let .success((persistedConfig, newExists)) = inspection else {
                    completion(.failed(
                        message: errorMessage + " The account transition remains pending until it can be checked safely.",
                        configurationSaved: false
                    ))
                    return
                }
                guard persistedConfig == intendedConfig else {
                    guard newExists else {
                        self.rollback(
                            generation: generation,
                            message: errorMessage,
                            completion: completion
                        )
                        return
                    }
                    self.removeInactiveNew(
                        account: newAccount,
                        generation: generation,
                        message: errorMessage,
                        completion: completion
                    )
                    return
                }
                guard newExists else {
                    completion(.failed(
                        message: errorMessage + " The saved configuration points to the new account, but its Keychain item is unavailable; the migration remains pending.",
                        configurationSaved: true
                    ))
                    return
                }
                let currentAccount = KeychainStore.account(forUsername: persistedConfig.username)
                if currentAccount == newAccount {
                    self.commitConfiguration(
                        oldAccount: self.credentialMutations.migration?.oldAccount,
                        newAccount: newAccount,
                        generation: generation,
                        outcome: .created,
                        completion: completion
                    )
                } else {
                    self.removeInactiveNew(
                        account: newAccount,
                        generation: generation,
                        message: errorMessage,
                        completion: completion
                    )
                }
            }
        }
    }

    private func commitConfiguration(
        oldAccount: String?,
        newAccount: String,
        generation: UInt64,
        outcome: KeychainStore.SaveOutcome,
        completion: @escaping CredentialSaveCompletion
    ) {
        do {
            try credentialMutations.mark(generation: generation, phase: .configurationSaved)
            try credentialMutations.commitMigration(generation: generation)
        } catch {
            completion(.failed(message: error.localizedDescription, configurationSaved: true))
            return
        }
        guard let oldAccount else {
            complete(outcome: outcome, generation: generation, warning: nil, completion: completion)
            return
        }
        let delete = keychainDelete
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let result = Result { try delete(oldAccount) }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                switch result {
                case .success:
                    self.complete(
                        outcome: outcome,
                        generation: generation,
                        warning: nil,
                        completion: completion
                    )
                case .failure:
                    completion(.succeeded(
                        outcome: outcome,
                        warning: "Saved the new username and password. The old Keychain item could not be removed and will be retried."
                    ))
                }
            }
        }
    }

    private func complete(
        outcome: KeychainStore.SaveOutcome,
        generation: UInt64,
        warning: String?,
        completion: @escaping CredentialSaveCompletion
    ) {
        do {
            try credentialMutations.completeMigration(generation: generation)
            completion(.succeeded(outcome: outcome, warning: warning))
        } catch {
            completion(.succeeded(
                outcome: outcome,
                warning: warning ?? "Saved the new username and password. Old-account cleanup remains pending and will be retried."
            ))
        }
    }

    private func rollback(
        generation: UInt64,
        message: String,
        completion: @escaping CredentialSaveCompletion
    ) {
        do {
            try credentialMutations.rollbackMigration(generation: generation)
            completion(.failed(message: message, configurationSaved: false))
        } catch {
            completion(.failed(
                message: message + " The migration cleanup intent could not be cleared and will be retried.",
                configurationSaved: false
            ))
        }
    }

    private func removeInactiveNew(
        account: String,
        generation: UInt64,
        message: String,
        completion: @escaping CredentialSaveCompletion
    ) {
        do {
            try credentialMutations.mark(generation: generation, phase: .cleanupNew)
        } catch {
            completion(.failed(
                message: message + " The new Keychain item remains pending cleanup.",
                configurationSaved: false
            ))
            return
        }
        let delete = keychainDelete
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let result = Result { try delete(account) }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                switch result {
                case .success:
                    self.rollback(generation: generation, message: message, completion: completion)
                case .failure:
                    completion(.failed(
                        message: message + " The new Keychain item could not be removed; cleanup will be retried.",
                        configurationSaved: false
                    ))
                }
            }
        }
    }
}
