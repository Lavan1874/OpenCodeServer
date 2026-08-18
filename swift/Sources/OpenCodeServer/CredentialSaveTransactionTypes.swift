import Foundation

enum CredentialMutation: Sendable {
    case none
    case create(account: String, password: String, oldAccount: String?)
    case update(account: String, password: String, original: String)
    case delete(account: String)

    var account: String {
        switch self {
        case .none:
            ""
        case .create(let account, _, _), .update(let account, _, _), .delete(let account):
            account
        }
    }
}

enum CredentialSaveTransactionResult: Sendable {
    case succeeded(outcome: KeychainStore.SaveOutcome, warning: String?)
    case failed(message: String, configurationSaved: Bool)
}

typealias CredentialSaveCompletion = (CredentialSaveTransactionResult) -> Void
typealias KeychainCreateOperation = @Sendable (String, String) throws -> KeychainStore.SaveOutcome
typealias KeychainUpdateOperation = @Sendable (String, String, String) throws -> KeychainStore.SaveOutcome
typealias KeychainDeleteOperation = @Sendable (String) throws -> Void
typealias KeychainContainsOperation = @Sendable (String) throws -> Bool
