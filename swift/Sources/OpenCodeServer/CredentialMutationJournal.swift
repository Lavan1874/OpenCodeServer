import Darwin
import Foundation

/// Durable intent for a username migration. This is deliberately smaller
/// than the credential mutation itself: it contains no password or derived
/// value, only enough information to finish or undo the account transition
/// after a crash.
enum CredentialMigrationPhase: String, Codable, Equatable, Sendable {
    case staged
    case newCredentialReady
    case configurationSaved
    case cleanupNew
    case cleanupOld
}

struct CredentialMigrationRecord: Codable, Equatable, Sendable {
    let generation: UInt64
    let oldAccount: String?
    let newAccount: String
    var phase: CredentialMigrationPhase
}

enum CredentialMutationJournalError: LocalizedError {
    case generationExhausted
    case invalidTransition
    case unreadable
    case privateFileUnreadable(String)
    case writeFailed
    case durabilityUncertain

    var errorDescription: String? {
        switch self {
        case .generationExhausted:
            "The credential notification generation is exhausted."
        case .invalidTransition:
            "The pending credential notification changed unexpectedly. Reopen Settings and try again."
        case .unreadable:
            "The pending credential notification could not be read."
        case let .privateFileUnreadable(message):
            "The pending credential notification is not safe to use: \(message)"
        case .writeFailed:
            "The pending credential notification could not be saved."
        case .durabilityUncertain:
            "The pending credential notification may have been saved and will be reconciled on the next launch."
        }
    }
}

/// Private, crash-safe state for credential notices and account migrations.
///
/// Only effective accounts, notice kind, generation, and cleanup intent are
/// persisted. In particular, this file never contains a credential, a
/// credential-derived value, or its length.
final class CredentialMutationJournal {
    static let maximumPrivateFileBytes = 16 * 1024

    typealias FileWrite = (String, Data) -> Bool
    private struct State: Codable, Equatable {
        var generation: UInt64 = 0
        var pending: CredentialMutationRecord?
        var staged: CredentialMutationRecord?
        var migration: CredentialMigrationRecord?
    }

    private let fileURL: URL
    private let fileManager: FileManager
    private let fileWrite: FileWrite?
    private let postRenameCheck: (() -> Bool)?
    private var state: State

    init(
        fileURL: URL,
        fileManager: FileManager = .default,
        fileWrite: FileWrite? = nil,
        postRenameCheck: (() -> Bool)? = nil
    ) throws {
        self.fileURL = fileURL
        self.fileManager = fileManager
        self.fileWrite = fileWrite
        self.postRenameCheck = postRenameCheck
        do {
            let data = try PrivateStateFileReader.read(
                at: fileURL,
                maxBytes: Self.maximumPrivateFileBytes
            )
            do {
                state = try PropertyListDecoder().decode(State.self, from: data)
            } catch {
                throw CredentialMutationJournalError.unreadable
            }
        } catch PrivateStateFileReadError.notFound {
            state = State()
        } catch let error as CredentialMutationJournalError {
            throw error
        } catch {
            throw CredentialMutationJournalError.privateFileUnreadable(
                error.localizedDescription
            )
        }

        // A staged write means OpenCodeServer stopped after the fail-closed
        // journal write but before recording the Security.framework outcome.
        // Replaying `changed` is conservative in both possible cases: if the
        // Keychain write landed, it invalidates stale in-memory credentials;
        // if it did not, explicit authorization converges from the actual
        // Keychain state. Treating an uncertain deletion as `removed` would
        // incorrectly permit an unauthenticated start, so recovery never does
        // that.
        if state.pending == nil, let staged = state.staged {
            state.pending = CredentialMutationRecord(
                account: staged.account,
                notice: .changed,
                generation: staged.generation
            )
            state.staged = nil
            try persist()
        }
    }

    var pending: CredentialMutationRecord? { state.pending }
    var migration: CredentialMigrationRecord? { state.migration }
    /// Work that must settle before OpenCodeServerAgent may apply/start the
    /// active configuration. A cleanupOld record with no pending notice is
    /// intentionally non-blocking: it names only an inactive legacy item.
    var hasUnacknowledgedMutation: Bool {
        state.pending != nil || state.staged != nil ||
            (state.migration != nil && state.migration?.phase != .cleanupOld)
    }

    @discardableResult
    func stage(account: String) throws -> UInt64 {
        let cleanupOnly = state.migration?.phase == .cleanupOld && state.pending == nil
        guard state.migration == nil || cleanupOnly else {
            throw CredentialMutationJournalError.invalidTransition
        }
        guard state.generation < UInt64.max else {
            throw CredentialMutationJournalError.generationExhausted
        }
        let previous = state
        state.generation += 1
        state.staged = CredentialMutationRecord(
            account: account,
            notice: .changed,
            generation: state.generation
        )
        do {
            try persist()
            return state.generation
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    @discardableResult
    func stageMigration(oldAccount: String?, newAccount: String) throws -> UInt64 {
        guard let oldAccount, oldAccount != newAccount else {
            throw CredentialMutationJournalError.invalidTransition
        }
        guard state.migration == nil, state.staged == nil, state.pending == nil else {
            throw CredentialMutationJournalError.invalidTransition
        }
        guard state.generation < UInt64.max else {
            throw CredentialMutationJournalError.generationExhausted
        }
        let previous = state
        state.generation += 1
        state.migration = CredentialMigrationRecord(
            generation: state.generation,
            oldAccount: oldAccount,
            newAccount: newAccount,
            phase: .staged
        )
        do {
            try persist()
            return state.generation
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    func setMigrationPhase(
        generation: UInt64,
        phase: CredentialMigrationPhase
    ) throws {
        guard var migration = state.migration,
              migration.generation == generation
        else {
            throw CredentialMutationJournalError.invalidTransition
        }
        let previous = state
        migration.phase = phase
        state.migration = migration
        do {
            try persist()
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    func commitMigration(generation: UInt64) throws {
        guard let migration = state.migration,
              migration.generation == generation,
              migration.phase == .configurationSaved
        else {
            throw CredentialMutationJournalError.invalidTransition
        }
        guard state.pending == nil else {
            throw CredentialMutationJournalError.invalidTransition
        }
        let previous = state
        state.pending = CredentialMutationRecord(
            account: migration.newAccount,
            notice: .changed,
            generation: generation
        )
        var committed = migration
        committed.phase = .cleanupOld
        state.migration = committed
        do {
            try persist()
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    func completeMigration(generation: UInt64) throws {
        guard state.migration?.generation == generation else {
            throw CredentialMutationJournalError.invalidTransition
        }
        let previous = state
        state.migration = nil
        do {
            try persist()
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    func rollbackMigration(generation: UInt64) throws {
        guard state.migration?.generation == generation else { return }
        let previous = state
        state.migration = nil
        do {
            try persist()
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    func commit(generation: UInt64, notice: CredentialMutationNotice) throws {
        guard let staged = state.staged, staged.generation == generation else {
            throw CredentialMutationJournalError.invalidTransition
        }
        let previous = state
        state.pending = CredentialMutationRecord(
            account: staged.account,
            notice: notice,
            generation: generation
        )
        state.staged = nil
        do {
            try persist()
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    func rollback(generation: UInt64) throws {
        guard state.staged?.generation == generation else { return }
        let previous = state
        state.staged = nil
        do {
            try persist()
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    @discardableResult
    func acknowledge(generation: UInt64) throws -> Bool {
        guard state.pending?.generation == generation else { return false }
        let previous = state
        state.pending = nil
        if let staged = state.staged {
            state.pending = CredentialMutationRecord(
                account: staged.account,
                notice: .changed,
                generation: staged.generation
            )
            state.staged = nil
        }
        do {
            try persist()
            return true
        } catch {
            throw restoreOrRetain(previous, after: error)
        }
    }

    private func restoreOrRetain(_ previous: State, after error: Error) -> Error {
        if let journalError = error as? CredentialMutationJournalError,
           case .durabilityUncertain = journalError
        {
            // rename(2) already made the new state visible. Keep the in-memory
            // state aligned with that file; a later launch reconciles whether
            // the directory flush survived a crash.
            return error
        }
        state = previous
        return error
    }

    private func persist() throws {
        let directory = fileURL.deletingLastPathComponent()
        var didRename = false
        do {
            try fileManager.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700]
            )
            try fileManager.setAttributes(
                [.posixPermissions: 0o700],
                ofItemAtPath: directory.path
            )
            let encoder = PropertyListEncoder()
            encoder.outputFormat = .xml
            let data = try encoder.encode(state)
            let temporary = directory.appending(
                path: ".credential-notification.\(UUID().uuidString).tmp"
            )
            guard
                fileWrite?(temporary.path, data) ?? fileManager.createFile(
                    atPath: temporary.path,
                    contents: data,
                    attributes: [.posixPermissions: 0o600]
                )
            else {
                throw CredentialMutationJournalError.writeFailed
            }
            do {
                let handle = try FileHandle(forWritingTo: temporary)
                try handle.synchronize()
                try handle.close()
                guard Darwin.rename(temporary.path, fileURL.path) == 0 else {
                    throw CredentialMutationJournalError.writeFailed
                }
                didRename = true
                try fileManager.setAttributes(
                    [.posixPermissions: 0o600],
                    ofItemAtPath: fileURL.path
                )
                if let postRenameCheck, !postRenameCheck() {
                    throw CredentialMutationJournalError.writeFailed
                }
                // Flush the containing directory too: the file fsync makes
                // the contents durable, but only the directory flush
                // preserves the rename itself across a crash.
                let directoryDescriptor = Darwin.open(directory.path, O_RDONLY)
                guard directoryDescriptor >= 0 else {
                    throw CredentialMutationJournalError.writeFailed
                }
                let directorySyncResult = Darwin.fsync(directoryDescriptor)
                Darwin.close(directoryDescriptor)
                guard directorySyncResult == 0 else {
                    throw CredentialMutationJournalError.writeFailed
                }
            } catch {
                try? fileManager.removeItem(at: temporary)
                if didRename {
                    throw CredentialMutationJournalError.durabilityUncertain
                }
                throw error
            }
        } catch let error as CredentialMutationJournalError {
            throw error
        } catch {
            throw CredentialMutationJournalError.writeFailed
        }
    }
}
