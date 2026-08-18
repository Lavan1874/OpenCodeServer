import Foundation
import Security

/// Login-keychain storage for the OpenCode password (ADR 0016).
///
/// The password lives as a Generic Password item with a fixed `service` and
/// the configured OpenCode username as `account`. OpenCodeServer owns the
/// item: it creates, updates, and deletes it. OpenCodeServerAgent only reads
/// it. New items are created with an ACL that pre-lists the creating app
/// and the agent (see `makeCreationAccess`): the agent's first decrypt
/// still raises the one unavoidable consent dialog, but a single
/// "Always Allow" then completes the grant (application entry and
/// partition list) instead of prompting twice.
///
/// Discipline enforced here:
/// - Password changes use `SecItemUpdate` in place, and only when the value
///   actually changed. Delete + re-add would reset the whole item ACL; an
///   update keeps the application list, but on macOS 26 still wipes the
///   XARA partition list, so a real change forces OpenCodeServerAgent
///   through the authorization prompt again — an unchanged save stays a
///   no-op to avoid that.
/// - `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` documents the intent
///   (this device only, never synced). Per TN3137 the file-based login
///   keychain treats `kSecAttrAccessible*` as a no-op; the login keychain's
///   inherent behavior already provides the semantics.
/// - `kSecAttrSynchronizable` and the data-protection keychain are never
///   used: they would route the item to a keychain a launchd-started
///   background process cannot read (errSecMissingEntitlement).
enum KeychainStore {
    static let service = "ai.opencode.server"

    /// Maximum accepted password length, mirroring the historical plist
    /// validation so over-sized input is rejected before hitting the
    /// keychain.
    static let maximumPasswordLength = 4_096

    enum StoreError: LocalizedError {
        case readFailed(OSStatus)
        case writeFailed(OSStatus)
        case emptyPassword
        case invalidLength

        var errorDescription: String? {
            switch self {
            case .readFailed:
                return "The saved password couldn’t be read from Keychain. Try again."
            case .writeFailed:
                return "The password couldn’t be saved to Keychain. Make sure your login keychain is unlocked, then try again."
            case .emptyPassword:
                return "Enter a password, or use Remove to delete the saved password."
            case .invalidLength:
                return "Password must be at most \(KeychainStore.maximumPasswordLength) characters and contain no NUL character."
            }
        }
    }

    /// A person can dismiss the legacy Keychain consent dialog with Cancel,
    /// Escape, or Command-Period. This is an abandoned action, not a product
    /// error, so Settings silently returns to the stable stored state.
    static func isUserCancellation(_ error: Error) -> Bool {
        guard case let StoreError.readFailed(status) = error else { return false }
        return status == errSecUserCanceled
    }

    /// Converts Security.framework failures into concise, actionable UI copy.
    /// The raw OSStatus remains available to Unified Logging for diagnosis but
    /// is never exposed as the explanation a person has to interpret.
    static func userFacingReadFailure(_ error: Error) -> String {
        guard case let StoreError.readFailed(status) = error else {
            return "The saved password couldn’t be read from Keychain. Try again."
        }
        switch status {
        case errSecAuthFailed:
            return "Keychain didn’t allow access to the saved password. Choose Edit or Copy to try again."
        case errSecInteractionNotAllowed:
            return "Keychain access isn’t available right now. Unlock your login keychain, then try again."
        default:
            return "The saved password couldn’t be read from Keychain. Try again."
        }
    }

    /// Attribute-only existence probe. This does not ask Keychain to decrypt
    /// the item and therefore cannot raise the legacy consent dialog. The
    /// caller still runs it off the main thread because securityd IPC has no
    /// useful UI-thread latency bound.
    static func contains(account: String) throws -> Bool {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecReturnAttributes: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            return true
        case errSecItemNotFound:
            return false
        default:
            throw StoreError.readFailed(status)
        }
    }

    /// Returns the stored password for `account`, or nil when no item
    /// exists. This is a decrypt-class read and may raise a system consent
    /// dialog; callers must put it behind an explicit action and run it off
    /// the main thread.
    static func load(account: String) throws -> String? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data,
                  let password = String(data: data, encoding: .utf8)
            else {
                // An item edited outside the product may hold non-UTF-8
                // data; treat it as unreadable rather than missing.
                throw StoreError.readFailed(errSecAuthFailed)
            }
            return password
        case errSecItemNotFound:
            return nil
        default:
            throw StoreError.readFailed(status)
        }
    }

    /// What a credential mutation actually did, so callers can react to a
    /// grant-revoking write.
    enum SaveOutcome: Equatable, Sendable {
        /// A new item was added (fresh ACL, no grant yet).
        case created
        /// An existing item's value was updated in place — on macOS 26 this
        /// wiped the XARA partition list, revoking OpenCodeServerAgent's
        /// grant; the caller should arrange a new interactive consent.
        case updatedExisting
        /// The stored value already matched; nothing was written (a write
        /// would have revoked the grant for no benefit).
        case unchanged
        /// The item was deleted by an explicit removal action.
        case deleted
    }

    /// Creates a credential without a preceding decrypt-class read. Settings
    /// calls this only after an attribute-only probe established that the
    /// account is empty. A race that creates the item meanwhile fails closed
    /// with `errSecDuplicateItem` instead of overwriting an unseen secret.
    static func create(account: String, password: String) throws -> SaveOutcome {
        let data = try validatedPasswordData(password)
        var attributes: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecValueData: data,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        if let access = Self.makeCreationAccess() {
            attributes[kSecAttrAccess] = access
        }
        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw StoreError.writeFailed(status)
        }
        return .created
    }

    /// Updates an item after an explicit Edit read supplied the original
    /// value. Comparing locally keeps an unchanged Save a true no-op without
    /// asking Keychain to decrypt again. The caller runs this write off the
    /// main thread because Security.framework may wait on securityd.
    static func update(
        account: String,
        password: String,
        knownCurrentPassword: String
    ) throws -> SaveOutcome {
        guard password != knownCurrentPassword else { return .unchanged }
        let data = try validatedPasswordData(password)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account
        ]
        let status = SecItemUpdate(
            query as CFDictionary,
            [kSecValueData: data] as CFDictionary
        )
        guard status == errSecSuccess else {
            throw StoreError.writeFailed(status)
        }
        return .updatedExisting
    }

    private static func validatedPasswordData(_ password: String) throws -> Data {
        guard !password.isEmpty else { throw StoreError.emptyPassword }
        guard password.count <= maximumPasswordLength, !password.contains("\0") else {
            throw StoreError.invalidLength
        }
        return Data(password.utf8)
    }

    /// Builds the access object for a newly created item: the creating app
    /// and the embedded OpenCodeServerAgent are both listed explicitly.
    /// Returns nil (fall back to the default ACL) when any legacy SecAccess
    /// call is unavailable — the grant flow still works, it merely costs a
    /// second consent.
    ///
    /// The creating app must be listed EXPLICITLY: measured 2026-08-05 on
    /// macOS 26, a custom `kSecAttrAccess` replaces the default ACL instead
    /// of extending it, so an item created with only the agent in the list
    /// prompts even when the GUI itself reads the value back.
    ///
    /// On macOS 26 the first decrypt by the agent always raises one consent
    /// dialog; pre-seeding cannot avoid that (only interactive consent adds
    /// the caller to the item's partition list). What it does avoid is the
    /// SECOND prompt: measured 2026-08-05, an "Always Allow" approval writes
    /// the partition grant immediately when the approving binary already has
    /// an ACL application entry, whereas the first approval that creates the
    /// entry leaves the partition list untouched and the next agent process
    /// prompts again (securityd logs `ACL partition mismatch`).
    private static func makeCreationAccess() -> SecAccess? {
        // nil path = the calling application itself.
        var selfTrusted: SecTrustedApplication?
        guard SecTrustedApplicationCreateFromPath(nil, &selfTrusted) == errSecSuccess,
              let selfTrusted
        else {
            return nil
        }
        let agentPath = Bundle.main.bundlePath
            + "/Contents/Resources/OpenCodeServerAgent.app/Contents/MacOS/OpenCodeServerAgent"
        guard FileManager.default.fileExists(atPath: agentPath) else {
            return nil
        }
        var agentTrusted: SecTrustedApplication?
        guard agentPath.withCString({ SecTrustedApplicationCreateFromPath($0, &agentTrusted) })
            == errSecSuccess,
            let agentTrusted
        else {
            return nil
        }
        var access: SecAccess?
        guard SecAccessCreate(service as CFString, [selfTrusted, agentTrusted] as CFArray, &access)
            == errSecSuccess
        else {
            return nil
        }
        return access
    }

    /// Removes the item for `account`; a missing item counts as success.
    static func delete(account: String) throws {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account
        ]
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw StoreError.writeFailed(status)
        }
    }

    /// Recovery cleanup is never allowed to raise a Keychain authorization
    /// dialog. If securityd would require interaction, retain the migration
    /// intent for an explicit, user-initiated retry instead.
    static func deleteWithoutInteraction(account: String) throws {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecUseAuthenticationUI: kSecUseAuthenticationUIFail
        ]
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw StoreError.writeFailed(status)
        }
    }

    /// The keychain account for a configuration: the effective OpenCode
    /// username, matching OpenCodeServerAgent's `effective_username`
    /// (a blank username falls back to the product default).
    static func account(forUsername username: String) -> String {
        username.isEmpty ? "opencode" : username
    }
}
