import Foundation

/// Owns exactly one Settings credential transaction. The concrete stores are
/// kept in the two small flows below so this type remains a lifetime owner
/// and dispatcher rather than a second transaction implementation.
@MainActor
final class CredentialSaveTransaction {
    typealias Completion = CredentialSaveCompletion

    private let migration: CredentialMigrationSaveTransaction
    private let sameAccount: CredentialMutationSaveTransaction

    init(
        configStore: ConfigStore,
        credentialMutations: CredentialMutationCoordinator,
        keychainCreate: @escaping KeychainCreateOperation,
        keychainUpdate: @escaping KeychainUpdateOperation,
        keychainDelete: @escaping KeychainDeleteOperation,
        keychainContains: @escaping KeychainContainsOperation
    ) {
        migration = CredentialMigrationSaveTransaction(
            configStore: configStore,
            credentialMutations: credentialMutations,
            keychainCreate: keychainCreate,
            keychainDelete: keychainDelete,
            keychainContains: keychainContains
        )
        sameAccount = CredentialMutationSaveTransaction(
            configStore: configStore,
            credentialMutations: credentialMutations,
            keychainCreate: keychainCreate,
            keychainUpdate: keychainUpdate,
            keychainDelete: keychainDelete
        )
    }

    func start(
        mutation: CredentialMutation,
        config: AppConfig,
        completion: @escaping Completion
    ) {
        switch mutation {
        case .create(_, _, let oldAccount):
            if oldAccount == nil {
                sameAccount.start(mutation: mutation, config: config, completion: completion)
            } else {
                migration.start(mutation: mutation, config: config, completion: completion)
            }
        case .none, .update, .delete:
            sameAccount.start(mutation: mutation, config: config, completion: completion)
        }
    }
}
