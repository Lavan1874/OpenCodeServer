import Foundation

/// Configuration-first transaction for a same-account create, update, or
/// removal. A failed Keychain operation leaves the saved configuration and
/// item state explicit; the UI retains its editor state for a retry.
@MainActor
final class CredentialMutationSaveTransaction {
    private let configStore: ConfigStore
    private let credentialMutations: CredentialMutationCoordinator
    private let keychainCreate: KeychainCreateOperation
    private let keychainUpdate: KeychainUpdateOperation
    private let keychainDelete: KeychainDeleteOperation

    init(
        configStore: ConfigStore,
        credentialMutations: CredentialMutationCoordinator,
        keychainCreate: @escaping KeychainCreateOperation,
        keychainUpdate: @escaping KeychainUpdateOperation,
        keychainDelete: @escaping KeychainDeleteOperation
    ) {
        self.configStore = configStore
        self.credentialMutations = credentialMutations
        self.keychainCreate = keychainCreate
        self.keychainUpdate = keychainUpdate
        self.keychainDelete = keychainDelete
    }

    func start(
        mutation: CredentialMutation,
        config: AppConfig,
        completion: @escaping CredentialSaveCompletion
    ) {
        switch mutation {
        case .create, .update, .delete:
            break
        case .none:
            completion(.failed(
                message: "Invalid same-account credential operation.",
                configurationSaved: false
            ))
            return
        }
        do {
            try configStore.save(config)
        } catch {
            completion(.failed(message: error.localizedDescription, configurationSaved: false))
            return
        }
        let generation: UInt64
        do {
            generation = try credentialMutations.stage(account: mutation.account)
        } catch {
            completion(.failed(message: error.localizedDescription, configurationSaved: true))
            return
        }

        let update = keychainUpdate
        let create = keychainCreate
        let delete = keychainDelete
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = Result { () -> KeychainStore.SaveOutcome in
                switch mutation {
                case .create(let account, let password, let oldAccount):
                    guard oldAccount == nil else {
                        throw CredentialMutationJournalError.invalidTransition
                    }
                    return try create(account, password)
                case .update(let account, let password, let original):
                    return try update(account, password, original)
                case .delete(let account):
                    try delete(account)
                    return .deleted
                case .none:
                    throw CredentialMutationJournalError.invalidTransition
                }
            }
            DispatchQueue.main.async { [weak self] in
                self?.finish(
                    result: result,
                    generation: generation,
                    completion: completion
                )
            }
        }
    }

    private func finish(
        result: Result<KeychainStore.SaveOutcome, Error>,
        generation: UInt64,
        completion: @escaping CredentialSaveCompletion
    ) {
        let outcome: KeychainStore.SaveOutcome
        do {
            outcome = try result.get()
        } catch {
            // Configuration is durable, and the failed item operation did not
            // make it absent. Keep the UI's prior credential state.
            try? credentialMutations.rollback(generation: generation)
            completion(.failed(message: error.localizedDescription, configurationSaved: true))
            return
        }
        do {
            try credentialMutations.commit(generation: generation, outcome: outcome)
            completion(.succeeded(outcome: outcome, warning: nil))
        } catch {
            // The mutation landed. Leave the regular staged notice on disk so
            // a restart promotes it to the matching account.
            completion(.failed(message: error.localizedDescription, configurationSaved: true))
        }
    }
}
