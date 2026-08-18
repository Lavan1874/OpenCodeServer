import Foundation
import OSLog

enum CredentialMutationNotice: String, Codable, Sendable {
    case changed
    case removed

    var command: AgentCommand {
        switch self {
        case .changed:
            .credentialChanged
        case .removed:
            .credentialRemoved
        }
    }

    func isAcknowledged(by status: AgentStatus, account: String) -> Bool {
        guard status.username == account else { return false }
        switch self {
        case .changed:
            return status.passwordState == .accessPending
        case .removed:
            return status.passwordState == .notConfigured
        }
    }
}

struct CredentialMutationRecord: Codable, Equatable, Sendable {
    let account: String
    let notice: CredentialMutationNotice
    let generation: UInt64
}

enum CredentialMutationAvailability: Equatable {
    case available
    case unavailable(String)

    var isAvailable: Bool {
        if case .available = self { return true }
        return false
    }

    var detail: String? {
        if case let .unavailable(message) = self { return message }
        return nil
    }
}

/// Serializes durable credential mutation notices over the existing IPC
/// protocol. A one-shot command response is the ACK: OpenCodeServerAgent has
/// already applied the command before it builds that response, so the expected
/// account and post-command password state prove that this exact notice landed.
@MainActor
final class CredentialMutationCoordinator {
    typealias Sender = @Sendable (AgentCommand) throws -> AgentResponse

    private let fileURL: URL
    private let fileManager: FileManager
    private let fileWrite: CredentialMutationJournal.FileWrite?
    private var journal: CredentialMutationJournal?
    private var migrationReconciler: CredentialMigrationReconciler?
    private let sender: Sender
    private let worker = DispatchQueue(
        label: "ai.opencode.server.credential-notification",
        qos: .userInitiated
    )
    private let logger = Logger(subsystem: "ai.opencode.server", category: "credentials")
    private var latestStatus: AgentStatus?
    private var inFlightGeneration: UInt64?
    private var deferredUntilAcknowledged: [() -> Void] = []
    private(set) var availability: CredentialMutationAvailability

    var onAcknowledgedStatus: ((AgentStatus) -> Void)?
    var onPendingStateChange: (() -> Void)?

    init(
        fileURL: URL,
        sender: @escaping Sender,
        fileManager: FileManager = .default,
        fileWrite: CredentialMutationJournal.FileWrite? = nil
    ) throws {
        self.fileURL = fileURL
        self.fileManager = fileManager
        self.fileWrite = fileWrite
        self.sender = sender
        self.availability = .available
        let journal = try CredentialMutationJournal(
            fileURL: fileURL,
            fileManager: fileManager,
            fileWrite: fileWrite
        )
        self.journal = journal
        self.migrationReconciler = nil
        install(journal: journal)
    }

    /// Keeps OpenCodeServer alive when the journal is missing, unsafe, or
    /// corrupt. The coordinator remains fail-closed until the user explicitly
    /// retries the same path; no unknown state is deleted or overwritten.
    static func unavailable(
        fileURL: URL,
        sender: @escaping Sender,
        fileManager: FileManager = .default,
        fileWrite: CredentialMutationJournal.FileWrite? = nil,
        error: Error
    ) -> CredentialMutationCoordinator {
        CredentialMutationCoordinator(
            fileURL: fileURL,
            sender: sender,
            fileManager: fileManager,
            fileWrite: fileWrite,
            unavailableMessage: Self.unavailableMessage(
                fileURL: fileURL,
                error: error
            )
        )
    }

    private static func unavailableMessage(fileURL: URL, error: Error) -> String {
        "Credential journal at \(fileURL.path) is unavailable: \(error.localizedDescription) OpenCodeServer will not delete or replace it. Restore a valid 0600 regular file, or move it aside only after confirming no credential change is pending, then click Retry."
    }

    private init(
        fileURL: URL,
        sender: @escaping Sender,
        fileManager: FileManager,
        fileWrite: CredentialMutationJournal.FileWrite?,
        unavailableMessage: String
    ) {
        self.fileURL = fileURL
        self.fileManager = fileManager
        self.fileWrite = fileWrite
        self.sender = sender
        self.availability = .unavailable(unavailableMessage)
        self.journal = nil
        self.migrationReconciler = nil
    }

    /// Re-reads the existing journal after an explicit user request. This is
    /// deliberately a retry only: it never removes or replaces an unsafe
    /// file, so an unknown mutation intent cannot be silently discarded.
    func retryAvailability() {
        guard !availability.isAvailable else { return }
        do {
            let journal = try CredentialMutationJournal(
                fileURL: fileURL,
                fileManager: fileManager,
                fileWrite: fileWrite
            )
            self.journal = journal
            self.availability = .available
            install(journal: journal)
            pendingStateDidChange()
            attemptDelivery()
        } catch {
            availability = .unavailable(
                Self.unavailableMessage(fileURL: fileURL, error: error)
            )
            pendingStateDidChange()
        }
    }

    private func install(journal: CredentialMutationJournal) {
        let migrationReconciler = CredentialMigrationReconciler(journal: journal)
        self.migrationReconciler = migrationReconciler
        migrationReconciler.onStateChange = { [weak self] in
            self?.pendingStateDidChange()
        }
        migrationReconciler.onNeedsDelivery = { [weak self] in
            self?.attemptDelivery()
        }
        migrationReconciler.onReadyToDrain = { [weak self] in
            self?.drainDeferredActionsIfReady()
        }
        migrationReconciler.onRecoveryFailure = { [weak self] message in
            self?.logger.warning(
                "Credential migration recovery remains pending: \(message, privacy: .public)"
            )
        }
    }

    var hasUnacknowledgedMutation: Bool {
        guard availability.isAvailable, let journal else { return true }
        return journal.hasUnacknowledgedMutation
    }

    var pendingGeneration: UInt64? {
        journal?.pending?.generation
    }

    var pendingAccount: String? {
        journal?.pending?.account
    }

    var migration: CredentialMigrationRecord? {
        journal?.migration
    }

    /// Settings saves pause while startup recovery is between its latest
    /// configuration read and the durable journal decision. This keeps a
    /// username save from racing a recovery notice; cleanup still performs a
    /// second off-main configuration check immediately before deletion.
    var recoveryInFlight: Bool {
        migrationReconciler?.recoveryInFlight ?? false
    }

    var deliveryInFlight: Bool {
        inFlightGeneration != nil
    }

    private func requireJournal() throws -> CredentialMutationJournal {
        guard availability.isAvailable, let journal else {
            throw CredentialMutationJournalError.privateFileUnreadable(
                availability.detail ?? "Credential mutation state is unavailable."
            )
        }
        return journal
    }

    private func requireMigrationReconciler() throws -> CredentialMigrationReconciler {
        guard availability.isAvailable, let migrationReconciler else {
            throw CredentialMutationJournalError.privateFileUnreadable(
                availability.detail ?? "Credential mutation state is unavailable."
            )
        }
        return migrationReconciler
    }

    @discardableResult
    func stage(account: String) throws -> UInt64 {
        let generation = try requireJournal().stage(account: account)
        pendingStateDidChange()
        return generation
    }

    @discardableResult
    func stageMigration(oldAccount: String?, newAccount: String) throws -> UInt64 {
        let generation = try requireMigrationReconciler().stage(
            oldAccount: oldAccount,
            newAccount: newAccount
        )
        pendingStateDidChange()
        return generation
    }

    func mark(
        generation: UInt64,
        phase: CredentialMigrationPhase
    ) throws {
        try requireMigrationReconciler().mark(generation: generation, phase: phase)
        pendingStateDidChange()
    }

    func commitMigration(generation: UInt64) throws {
        try requireMigrationReconciler().commit(generation: generation)
        pendingStateDidChange()
        attemptDelivery()
    }

    func completeMigration(generation: UInt64) throws {
        try requireMigrationReconciler().complete(generation: generation)
        pendingStateDidChange()
        drainDeferredActionsIfReady()
    }

    func rollbackMigration(generation: UInt64) throws {
        try requireMigrationReconciler().rollback(generation: generation)
        pendingStateDidChange()
        drainDeferredActionsIfReady()
    }

    /// Reconciles a pending username migration using only the current
    /// complete configuration and attribute-only Keychain observations. The
    /// loader and all Security.framework work run off the AppKit main thread;
    /// the worker never decrypts and never prompts for access.
    @discardableResult
    func recoverMigration(
        currentConfiguration: @escaping CredentialMigrationReconciler.ConfigurationLoad,
        contains: @escaping @Sendable (String) throws -> Bool,
        delete: @escaping @Sendable (String) throws -> Void
    ) -> Bool {
        guard let migrationReconciler, availability.isAvailable else {
            pendingStateDidChange()
            return false
        }
        migrationReconciler.recover(
            currentConfiguration: currentConfiguration,
            contains: contains,
            delete: delete
        )
        return true
    }

    func commit(generation: UInt64, outcome: KeychainStore.SaveOutcome) throws {
        guard let notice = Self.notice(for: outcome) else {
            try rollback(generation: generation)
            return
        }
        try requireJournal().commit(generation: generation, notice: notice)
        pendingStateDidChange()
        attemptDelivery()
    }

    func rollback(generation: UInt64) throws {
        try requireJournal().rollback(generation: generation)
        pendingStateDidChange()
        drainDeferredActionsIfReady()
    }

    func observe(_ status: AgentStatus?) {
        latestStatus = status
        attemptDelivery()
    }

    @discardableResult
    func performAfterAcknowledgement(_ action: @escaping () -> Void) -> Bool {
        guard availability.isAvailable else {
            pendingStateDidChange()
            return false
        }
        guard hasUnacknowledgedMutation else {
            action()
            return true
        }
        deferredUntilAcknowledged.append(action)
        return true
    }

    static func notice(for outcome: KeychainStore.SaveOutcome) -> CredentialMutationNotice? {
        switch outcome {
        case .created, .updatedExisting:
            .changed
        case .deleted:
            .removed
        case .unchanged:
            nil
        }
    }

    private func attemptDelivery() {
        guard inFlightGeneration == nil,
            let pending = journal?.pending,
            let status = latestStatus,
            status.username == pending.account
        else { return }

        inFlightGeneration = pending.generation
        let sender = sender
        worker.async { [weak self] in
            let result = Result { try sender(pending.notice.command) }
            DispatchQueue.main.async {
                self?.finishDelivery(result, pending: pending)
            }
        }
    }

    private func finishDelivery(
        _ result: Result<AgentResponse, Error>,
        pending: CredentialMutationRecord
    ) {
        guard inFlightGeneration == pending.generation else { return }
        inFlightGeneration = nil

        guard case .success(let response) = result,
            response.ok,
            let status = response.status,
            pending.notice.isAcknowledged(by: status, account: pending.account)
        else {
            // The durable record remains. A later subscription status — in
            // particular the first status after reconnect — retries it. Do not
            // spin on the same failed or malformed response here.
            return
        }

        do {
            let cleared = try requireJournal().acknowledge(generation: pending.generation)
            latestStatus = status
            if cleared {
                pendingStateDidChange()
                drainDeferredActionsIfReady()
            }
            onAcknowledgedStatus?(status)
            // A newer generation may have committed while this request was in
            // flight. Its record cannot be cleared by this ACK; send it next on
            // the same serial worker.
            attemptDelivery()
        } catch {
            logger.error(
                "Unable to clear an acknowledged credential notification: \(error.localizedDescription, privacy: .public)"
            )
        }
    }

    private func pendingStateDidChange() {
        onPendingStateChange?()
    }

    private func drainDeferredActionsIfReady() {
        guard !hasUnacknowledgedMutation else { return }
        let actions = deferredUntilAcknowledged
        deferredUntilAcknowledged.removeAll()
        actions.forEach { $0() }
    }
}
