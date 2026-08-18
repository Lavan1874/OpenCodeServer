@testable import OpenCodeServer
import Foundation
import XCTest

private final class FixtureKeychain: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: String] = [:]
    var failedCreates = false
    var failedDeletes = Set<String>()

    func seed(_ account: String) {
        lock.lock()
        values[account] = "fixture"
        lock.unlock()
    }

    func contains(_ account: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return values[account] != nil
    }

    func create(_ account: String, _ password: String) throws -> KeychainStore.SaveOutcome {
        lock.lock()
        defer { lock.unlock() }
        if failedCreates {
            throw NSError(domain: "FixtureKeychain", code: 1)
        }
        values[account] = password
        return .created
    }

    func delete(_ account: String) throws {
        lock.lock()
        defer { lock.unlock() }
        if failedDeletes.contains(account) {
            throw NSError(domain: "FixtureKeychain", code: 2)
        }
        values.removeValue(forKey: account)
    }
}

private final class FixtureCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var valueStorage = 0

    func increment() {
        lock.lock()
        valueStorage += 1
        lock.unlock()
    }

    var value: Int {
        lock.lock()
        defer { lock.unlock() }
        return valueStorage
    }
}

private final class FixtureConfigurations: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [AppConfig]
    private var nextIndex = 0

    init(_ values: [AppConfig]) {
        self.values = values
    }

    func load() throws -> AppConfig {
        lock.lock()
        defer { lock.unlock() }
        let index = min(nextIndex, values.count - 1)
        nextIndex += 1
        return values[index]
    }
}

@MainActor
final class CredentialMigrationTests: XCTestCase {
    func testSuccessfulMigrationKeepsNoticeOnNewAccount() throws {
        let root = makeRoot("success")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        keychain.seed("old")
        let coordinator = try makeCoordinator(root)
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: AppConfig(username: "new"),
            mutation: .create(account: "new", password: "new-secret", oldAccount: "old")
        )

        guard case .succeeded(_, let warning) = result else {
            return XCTFail("expected successful migration")
        }
        XCTAssertNil(warning)
        XCTAssertEqual(try store.load().username, "new")
        XCTAssertTrue(keychain.contains("new"))
        XCTAssertFalse(keychain.contains("old"))
        XCTAssertNil(coordinator.migration)
        XCTAssertEqual(coordinator.pendingAccount, "new")
    }

    func testCreateFailureLeavesOldConfigurationAndCredential() throws {
        let root = makeRoot("create-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        keychain.seed("old")
        keychain.failedCreates = true
        let coordinator = try makeCoordinator(root)
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: AppConfig(username: "new"),
            mutation: .create(account: "new", password: "secret", oldAccount: "old")
        )

        guard case .failed(_, let configurationSaved) = result else {
            return XCTFail("expected create failure")
        }
        XCTAssertFalse(configurationSaved)
        XCTAssertEqual(try store.load().username, "old")
        XCTAssertTrue(keychain.contains("old"))
        XCTAssertFalse(keychain.contains("new"))
        XCTAssertNil(coordinator.migration)
        XCTAssertNil(coordinator.pendingAccount)
    }

    func testConfigurationFailureCompensationFailureConvergesAfterRestartRecovery() throws {
        let root = makeRoot("config-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        keychain.seed("old")
        keychain.failedDeletes.insert("new")
        let coordinator = try makeCoordinator(root)
        let invalidConfig = AppConfig(username: "new:")
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: invalidConfig,
            mutation: .create(account: "new", password: "secret", oldAccount: "old")
        )

        guard case .failed = result else { return XCTFail("expected config failure") }
        XCTAssertEqual(try store.load().username, "old")
        XCTAssertTrue(keychain.contains("old"))
        XCTAssertTrue(keychain.contains("new"))
        XCTAssertEqual(coordinator.migration?.phase, .cleanupNew)

        keychain.failedDeletes.remove("new")
        let recovered = try makeCoordinator(root)
        let settled = expectation(description: "inactive new item cleanup settled")
        recovered.onPendingStateChange = {
            if recovered.migration == nil { settled.fulfill() }
        }
        recovered.recoverMigration(
            currentConfiguration: { AppConfig(username: "old") },
            contains: { keychain.contains($0) },
            delete: { try keychain.delete($0) }
        )
        wait(for: [settled], timeout: 2)
        XCTAssertFalse(keychain.contains("new"))
        XCTAssertTrue(keychain.contains("old"))
        XCTAssertNil(recovered.migration)
        XCTAssertNil(recovered.pendingAccount)
    }

    func testConfigurationFailureWithSameAccountDoesNotUseUnintendedSettings() throws {
        let root = makeRoot("config-same-account")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        let oldConfig = AppConfig(username: "old")
        try store.save(oldConfig)
        let keychain = FixtureKeychain()
        let coordinator = try makeCoordinator(root)
        let intendedConfig = AppConfig(
            hostname: "localhost",
            port: 0,
            username: "old"
        )
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: intendedConfig,
            mutation: .create(account: "old", password: "secret", oldAccount: nil)
        )

        guard case .failed = result else {
            return XCTFail("expected invalid host/port configuration")
        }
        XCTAssertEqual(try store.load(), oldConfig)
        XCTAssertFalse(keychain.contains("old"))
        XCTAssertNil(coordinator.migration)
    }

    func testSameAccountCreateUsesConfigFirstRegularTransaction() throws {
        let root = makeRoot("same-account-create")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig(username: "same"))
        let keychain = FixtureKeychain()
        let coordinator = try makeCoordinator(root)
        let intended = AppConfig(hostname: "localhost", port: 4097, username: "same")

        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: intended,
            mutation: .create(account: "same", password: "secret", oldAccount: nil)
        )

        guard case .succeeded = result else {
            return XCTFail("expected same-account creation to succeed")
        }
        XCTAssertEqual(try store.load(), intended)
        XCTAssertTrue(keychain.contains("same"))
        XCTAssertNil(coordinator.migration)
        XCTAssertEqual(coordinator.pendingAccount, "same")
    }

    func testAbsentCredentialUsernameChangeUsesRegularCreate() throws {
        let root = makeRoot("absent-account-change")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        let coordinator = try makeCoordinator(root)
        let intended = AppConfig(username: "new")

        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: intended,
            mutation: .create(account: "new", password: "secret", oldAccount: nil)
        )

        guard case .succeeded = result else {
            return XCTFail("expected a first create on the changed account")
        }
        XCTAssertEqual(try store.load(), intended)
        XCTAssertTrue(keychain.contains("new"))
        XCTAssertFalse(keychain.contains("old"))
        XCTAssertNil(coordinator.migration)
        XCTAssertEqual(coordinator.pendingAccount, "new")
    }

    func testMigrationRequiresARealAccountSwitch() throws {
        let root = makeRoot("migration-account-guard")
        defer { try? FileManager.default.removeItem(at: root) }
        let coordinator = try makeCoordinator(root)

        XCTAssertThrowsError(
            try coordinator.stageMigration(oldAccount: nil, newAccount: "same")
        )
        XCTAssertThrowsError(
            try coordinator.stageMigration(oldAccount: "same", newAccount: "same")
        )
        XCTAssertNil(coordinator.migration)
    }

    func testSameAccountCreateStageFailureOccursAfterConfigurationSave() throws {
        let root = makeRoot("same-account-stage-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig(username: "same"))
        let keychain = FixtureKeychain()
        let coordinator = try CredentialMutationCoordinator(
            fileURL: paths.credentialMutationFile,
            sender: { _ in throw IPCError.emptyResponse },
            fileWrite: { _, _ in false }
        )
        let intended = AppConfig(hostname: "localhost", port: 4097, username: "same")

        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: intended,
            mutation: .create(account: "same", password: "secret", oldAccount: nil)
        )

        guard case .failed(_, let configurationSaved) = result else {
            return XCTFail("expected journal stage failure")
        }
        XCTAssertTrue(configurationSaved)
        XCTAssertEqual(try store.load(), intended)
        XCTAssertFalse(keychain.contains("same"))
        XCTAssertNil(coordinator.migration)
        XCTAssertNil(coordinator.pendingAccount)
    }

    func testJournalStageFailureDoesNotCreateCredential() throws {
        let root = makeRoot("stage-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        _ = FileManager.default.createFile(atPath: root.path, contents: Data())
        let configRoot = makeRoot("stage-config")
        defer { try? FileManager.default.removeItem(at: configRoot) }
        let store = ConfigStore(paths: makePaths(configRoot))
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        let coordinator = CredentialMutationCoordinator.unavailable(
            fileURL: makePaths(root).credentialMutationFile,
            sender: { _ in throw IPCError.emptyResponse },
            error: NSError(domain: "FixtureJournal", code: 1)
        )
        let result = try runTransaction(
            root: configRoot,
            coordinator: coordinator,
            keychain: keychain,
            config: AppConfig(username: "new"),
            mutation: .create(account: "new", password: "secret", oldAccount: "old")
        )

        guard case .failed(_, let configurationSaved) = result else {
            return XCTFail("expected journal stage failure")
        }
        XCTAssertFalse(configurationSaved)
        XCTAssertFalse(keychain.contains("new"))
        XCTAssertEqual(try store.load().username, "old")
        XCTAssertNil(coordinator.migration)
    }

    func testPostRenameJournalFailureKeepsMemoryAndDiskStateAligned() throws {
        let root = makeRoot("post-rename-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        let fileURL = makePaths(root).credentialMutationFile
        let journal = try CredentialMutationJournal(
            fileURL: fileURL,
            postRenameCheck: { false }
        )

        XCTAssertThrowsError(
            try journal.stageMigration(oldAccount: "old", newAccount: "new")
        ) { error in
            XCTAssertEqual(
                (error as? CredentialMutationJournalError).map {
                    if case .durabilityUncertain = $0 { return true }
                    return false
                },
                .some(true)
            )
        }
        XCTAssertEqual(journal.migration?.phase, .staged)
        let reopened = try CredentialMutationJournal(fileURL: fileURL)
        XCTAssertEqual(reopened.migration?.phase, .staged)
    }

    func testJournalCommitFailureKeepsConfigurationAndMigrationIntent() throws {
        let root = makeRoot("commit-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = ConfigStore(paths: makePaths(root))
        try store.save(AppConfig(username: "old"))
        var writes = 0
        let coordinator = try CredentialMutationCoordinator(
            fileURL: makePaths(root).credentialMutationFile,
            sender: { _ in throw IPCError.emptyResponse },
            fileWrite: { path, data in
                writes += 1
                guard writes < 4 else { return false }
                return FileManager.default.createFile(atPath: path, contents: data)
            }
        )
        let keychain = FixtureKeychain()
        keychain.seed("old")
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: AppConfig(username: "new"),
            mutation: .create(account: "new", password: "secret", oldAccount: "old")
        )

        guard case .failed(_, let configurationSaved) = result else {
            return XCTFail("expected journal commit failure")
        }
        XCTAssertTrue(configurationSaved)
        XCTAssertEqual(try store.load().username, "new")
        XCTAssertTrue(keychain.contains("new"))
        XCTAssertNotNil(coordinator.migration)
        XCTAssertNil(coordinator.pendingAccount)
    }

    func testOldCleanupFailureIsNonBlockingAndRecoveryDoesNotResendNotice() throws {
        let root = makeRoot("old-cleanup")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = ConfigStore(paths: makePaths(root))
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        keychain.seed("old")
        keychain.failedDeletes.insert("old")
        let sends = FixtureCounter()
        let coordinator = try CredentialMutationCoordinator(
            fileURL: makePaths(root).credentialMutationFile,
            sender: { _ in
                sends.increment()
                return Self.response(account: "new", passwordState: .accessPending)
            }
        )
        coordinator.observe(Self.status(account: "new", passwordState: .notConfigured))
        let action = expectation(description: "restart action released after notice ACK")
        coordinator.performAfterAcknowledgement { action.fulfill() }
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: AppConfig(username: "new"),
            mutation: .create(account: "new", password: "secret", oldAccount: "old")
        )
        wait(for: [action], timeout: 2)
        guard case .succeeded = result else { return XCTFail("expected saved config") }
        waitUntil { !coordinator.deliveryInFlight }
        XCTAssertEqual(sends.value, 1)
        XCTAssertEqual(coordinator.migration?.phase, .cleanupOld)
        XCTAssertFalse(coordinator.hasUnacknowledgedMutation)
        XCTAssertTrue(keychain.contains("old"))

        let updateGeneration = try coordinator.stage(account: "new")
        try coordinator.rollback(generation: updateGeneration)

        keychain.failedDeletes.remove("old")
        let recovered = try CredentialMutationCoordinator(
            fileURL: makePaths(root).credentialMutationFile,
            sender: { _ in
                sends.increment()
                return Self.response(account: "new", passwordState: .accessPending)
            }
        )
        let settled = expectation(description: "old cleanup retried")
        recovered.onPendingStateChange = {
            if recovered.migration == nil { settled.fulfill() }
        }
        recovered.recoverMigration(
            currentConfiguration: { AppConfig(username: "new") },
            contains: { keychain.contains($0) },
            delete: { try keychain.delete($0) }
        )
        wait(for: [settled], timeout: 2)
        XCTAssertFalse(keychain.contains("old"))
        XCTAssertNil(recovered.migration)
        XCTAssertEqual(sends.value, 1, "cleanupOld recovery must not resend credential_changed")
    }

    func testNotificationFailureLeavesConsistentNewAccountState() throws {
        let root = makeRoot("notice-fail")
        defer { try? FileManager.default.removeItem(at: root) }
        let store = ConfigStore(paths: makePaths(root))
        try store.save(AppConfig(username: "old"))
        let keychain = FixtureKeychain()
        keychain.seed("old")
        let sends = FixtureCounter()
        let attempted = expectation(description: "notification attempted")
        let coordinator = try CredentialMutationCoordinator(
            fileURL: makePaths(root).credentialMutationFile,
            sender: { _ in
                sends.increment()
                attempted.fulfill()
                throw IPCError.systemCall("fixture notification failure")
            }
        )
        coordinator.observe(Self.status(account: "new", passwordState: .notConfigured))
        let result = try runTransaction(
            root: root,
            coordinator: coordinator,
            keychain: keychain,
            config: AppConfig(username: "new"),
            mutation: .create(account: "new", password: "secret", oldAccount: "old")
        )
        wait(for: [attempted], timeout: 2)
        guard case .succeeded = result else { return XCTFail("expected saved config") }
        XCTAssertEqual(try store.load().username, "new")
        XCTAssertTrue(keychain.contains("new"))
        XCTAssertFalse(keychain.contains("old"))
        XCTAssertNil(coordinator.migration)
        XCTAssertEqual(coordinator.pendingAccount, "new")
        XCTAssertEqual(sends.value, 1)
    }

    func testRecoveryHoldsWhenConfigurationNamesAnUnexpectedAccount() {
        let record = CredentialMigrationRecord(
            generation: 1,
            oldAccount: "old",
            newAccount: "new",
            phase: .staged
        )
        let decision = CredentialMigrationRecovery.decide(
            record: record,
            currentAccount: "unrelated",
            probe: .observed(newExists: true, oldExists: true)
        )
        XCTAssertEqual(decision, .hold)
        XCTAssertEqual(
            CredentialMigrationRecovery.decide(
                record: record,
                currentAccount: "old",
                probe: .observed(newExists: true, oldExists: false)
            ),
            .removeNew
        )

        XCTAssertEqual(
            CredentialMigrationRecovery.decide(
                record: CredentialMigrationRecord(
                    generation: 2,
                    oldAccount: "old",
                    newAccount: "new",
                    phase: .cleanupNew
                ),
                currentAccount: "old",
                probe: .observed(newExists: true, oldExists: true)
            ),
            .removeNew
        )
        XCTAssertEqual(
            CredentialMigrationRecovery.decide(
                record: CredentialMigrationRecord(
                    generation: 3,
                    oldAccount: nil,
                    newAccount: "same-account",
                    phase: .staged
                ),
                currentAccount: "same-account",
                probe: .observed(newExists: true, oldExists: nil)
            ),
            .hold
        )
        XCTAssertEqual(
            CredentialMigrationRecovery.decide(
                record: record,
                currentAccount: "old",
                probe: .observed(newExists: false, oldExists: false)
            ),
            .discard
        )
    }

    func testRecoveryDiscardsStagedMigrationWhenBothItemsAreAbsent() throws {
        let root = makeRoot("recovery-absent-items")
        defer { try? FileManager.default.removeItem(at: root) }
        let coordinator = try makeCoordinator(root)
        _ = try coordinator.stageMigration(oldAccount: "old", newAccount: "new")
        let recovered = try makeCoordinator(root)
        let settled = expectation(description: "damaged migration discarded")
        recovered.onPendingStateChange = {
            if recovered.migration == nil { settled.fulfill() }
        }

        recovered.recoverMigration(
            currentConfiguration: { AppConfig(username: "old") },
            contains: { _ in false },
            delete: { _ in XCTFail("no inactive item should be deleted") }
        )

        wait(for: [settled], timeout: 2)
        XCTAssertNil(recovered.migration)
        XCTAssertFalse(recovered.recoveryInFlight)
    }

    func testRecoveryRechecksConfigurationBeforeRemovingNewItem() throws {
        let root = makeRoot("recovery-recheck")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let coordinator = try makeCoordinator(root)
        _ = try coordinator.stageMigration(oldAccount: "old", newAccount: "new")
        let keychain = FixtureKeychain()
        keychain.seed("old")
        keychain.seed("new")
        let deleteCalls = FixtureCounter()
        let latestConfigurationChecked = expectation(description: "latest configuration checked")
        let configurations = FixtureConfigurations([
            AppConfig(hostname: "127.0.0.1", port: 4096, username: "old"),
            AppConfig(hostname: "localhost", port: 4097, username: "new")
        ])
        let recovered = try CredentialMutationCoordinator(
            fileURL: paths.credentialMutationFile,
            sender: { _ in throw IPCError.emptyResponse }
        )
        recovered.recoverMigration(
            currentConfiguration: {
                let configuration = try configurations.load()
                if configuration.username == "new" {
                    latestConfigurationChecked.fulfill()
                }
                return configuration
            },
            contains: { keychain.contains($0) },
            delete: {
                deleteCalls.increment()
                try keychain.delete($0)
            }
        )
        wait(for: [latestConfigurationChecked], timeout: 2)
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        XCTAssertTrue(keychain.contains("new"))
        XCTAssertEqual(recovered.migration?.phase, .cleanupNew)
        XCTAssertEqual(deleteCalls.value, 0)
    }

    func testRecoveryRechecksBeforeCommittingNewNotice() throws {
        let root = makeRoot("recovery-notice-race")
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = makePaths(root)
        let staged = try makeCoordinator(root)
        _ = try staged.stageMigration(oldAccount: "old", newAccount: "new")
        let keychain = FixtureKeychain()
        keychain.seed("old")
        keychain.seed("new")
        let configurations = FixtureConfigurations([
            AppConfig(hostname: "localhost", port: 4097, username: "new"),
            AppConfig(hostname: "127.0.0.1", port: 4096, username: "old")
        ])
        let sends = FixtureCounter()
        let recovered = try CredentialMutationCoordinator(
            fileURL: paths.credentialMutationFile,
            sender: { _ in
                sends.increment()
                throw IPCError.emptyResponse
            }
        )
        let settled = expectation(description: "stale new notice is discarded")
        recovered.onPendingStateChange = {
            if recovered.migration == nil { settled.fulfill() }
        }
        recovered.recoverMigration(
            currentConfiguration: { try configurations.load() },
            contains: { keychain.contains($0) },
            delete: { try keychain.delete($0) }
        )
        wait(for: [settled], timeout: 2)
        XCTAssertFalse(keychain.contains("new"))
        XCTAssertTrue(keychain.contains("old"))
        XCTAssertNil(recovered.pendingAccount)
        XCTAssertEqual(sends.value, 0)
        XCTAssertFalse(recovered.recoveryInFlight)
    }

    private func runTransaction(
        root: URL,
        coordinator: CredentialMutationCoordinator,
        keychain: FixtureKeychain,
        config: AppConfig,
        mutation: CredentialMutation
    ) throws -> CredentialSaveTransactionResult {
        let transaction = CredentialSaveTransaction(
            configStore: ConfigStore(paths: makePaths(root)),
            credentialMutations: coordinator,
            keychainCreate: { try keychain.create($0, $1) },
            keychainUpdate: { _, _, _ in .updatedExisting },
            keychainDelete: { try keychain.delete($0) },
            keychainContains: { keychain.contains($0) }
        )
        var result: CredentialSaveTransactionResult?
        let completed = expectation(description: "transaction completed")
        transaction.start(mutation: mutation, config: config) {
            result = $0
            completed.fulfill()
        }
        wait(for: [completed], timeout: 3)
        return try XCTUnwrap(result)
    }

    private func makeCoordinator(_ root: URL) throws -> CredentialMutationCoordinator {
        try CredentialMutationCoordinator(
            fileURL: makePaths(root).credentialMutationFile,
            sender: { _ in throw IPCError.emptyResponse }
        )
    }

    private func makeRoot(_ suffix: String) -> URL {
        FileManager.default.temporaryDirectory.appending(
            path: "ocs-migration-\(suffix)-\(UUID().uuidString)",
            directoryHint: .isDirectory
        )
    }

    private func makePaths(_ root: URL) -> ApplicationPaths {
        ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
    }

    private func waitUntil(
        timeout: TimeInterval = 2,
        condition: () -> Bool
    ) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        XCTAssertTrue(condition())
    }

    nonisolated private static func response(account: String, passwordState: PasswordState) -> AgentResponse {
        AgentResponse(
            version: ipcProtocolVersion,
            ok: true,
            error: nil,
            status: status(account: account, passwordState: passwordState),
            validation: nil
        )
    }

    nonisolated private static func status(account: String, passwordState: PasswordState) -> AgentStatus {
        AgentStatus(
            protocolVersion: ipcProtocolVersion,
            agentVersion: "fixture",
            agentUptimeSeconds: 1,
            desiredState: .stopped,
            serverState: .stopped,
            health: .unknown,
            fda: .unableToDetermine,
            uptimeSeconds: nil,
            endpoint: "127.0.0.1:4096",
            username: account,
            passwordState: passwordState,
            authenticationEnabled: passwordState == .configured,
            actionCapabilities: .unavailable,
            installedVersion: nil,
            runningVersion: nil,
            versionPending: false,
            configPending: false,
            configError: nil,
            lastError: nil,
            pid: nil,
            stopGraceRemainingSeconds: nil,
            notification: nil,
            processStartedAtUnixSeconds: nil,
            bundleVersion: "fixture"
        )
    }
}
