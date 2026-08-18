@testable import OpenCodeServer
import Darwin
import Foundation
import XCTest

final class ModelsTests: XCTestCase {
    func testUnavailableOpenCodeServerAgentStatusPresentationIsGray() {
        let presentation = StatusPresentation.from(status: nil)
        XCTAssertEqual(presentation.color, .gray)
        XCTAssertEqual(
            presentation.label,
            "OpenCodeServerAgent Temporarily Unavailable"
        )
    }

    func testAgentRequestEncodingUsesCurrentProtocolVersion() throws {
        let data = try JSONEncoder().encode(AgentRequest(command: .forceStop))
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        XCTAssertEqual(object["version"] as? Int, ipcProtocolVersion)
        XCTAssertEqual(object["command"] as? String, "force_stop")
    }

    func testHealthPresentationDoesNotUsePermissionOrPendingConfiguration() throws {
        let data = """
        {
          "protocol_version": 6,
          "agent_version": "0.1.0",
          "agent_uptime_seconds": 5,
          "desired_state": "running",
          "server_state": "healthy",
          "health": "healthy",
          "fda": "not_verified",
          "uptime_seconds": 10,
          "endpoint": "127.0.0.1:4096",
          "username": "opencode",
          "password_state": "not_configured",
          "authentication_enabled": false,
          "action_capabilities": {
            "start": true,
            "stop": false,
            "restart": true,
            "continue_stop": false,
            "force_stop": false
          },
          "installed_version": "2.0",
          "running_version": "1.0",
          "version_pending": true,
          "config_pending": true,
          "config_error": null,
          "last_error": null,
          "pid": 42,
          "stop_grace_remaining_seconds": null,
          "notification": null,
          "process_started_at_unix_seconds": 1700000000,
          "bundle_version": "57"
        }
        """.data(using: .utf8)!
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let status = try decoder.decode(AgentStatus.self, from: data)
        XCTAssertEqual(
            status.actionCapabilities,
            ActionCapabilities(
                start: true,
                stop: false,
                restart: true,
                continueStop: false,
                forceStop: false
            )
        )
        XCTAssertEqual(StatusPresentation.from(status: status).color, .green)
        XCTAssertEqual(StatusPresentation.from(status: status).label, "Healthy")
    }

    func testDurationFormattingAlwaysShowsSeconds() {
        XCTAssertEqual(formatDuration(42), "42s")
        XCTAssertEqual(formatDuration(125), "2m 5s")
        XCTAssertEqual(formatDuration(7_500), "2h 5m 0s")
        XCTAssertEqual(formatDuration(90_000), "1d 1h 0m 0s")
    }

    func testDefaultConfigurationMatchesProductDecisions() {
        let config = AppConfig()
        XCTAssertEqual(config.hostname, "127.0.0.1")
        XCTAssertEqual(config.port, 4096)
        XCTAssertEqual(config.username, "opencode")
        XCTAssertFalse(config.mdns)
    }

    func testArm64MachODetectionAcceptsThinArm64Binary() {
        var bytes = [UInt8](repeating: 0, count: 32)
        bytes.replaceSubrange(0 ..< 4, with: [0xcf, 0xfa, 0xed, 0xfe])
        bytes.replaceSubrange(4 ..< 8, with: [0x0c, 0x00, 0x00, 0x01])

        XCTAssertTrue(ConfigStore.isArm64MachOForTesting(Data(bytes)))
    }

    func testArm64MachODetectionRejectsScriptHeader() {
        let bytes = "#!/bin/sh\n".data(using: .utf8)!

        XCTAssertFalse(ConfigStore.isArm64MachOForTesting(bytes))
    }

    func testArm64MachODetectionRejectsShortData() {
        let bytes = Data([0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00])

        XCTAssertFalse(ConfigStore.isArm64MachOForTesting(bytes))
    }

    func testArm64MachODetectionAcceptsFatBinaryContainingArm64Slice() {
        var bytes = [UInt8](repeating: 0, count: 28)
        bytes.replaceSubrange(0 ..< 4, with: [0xca, 0xfe, 0xba, 0xbe])
        bytes.replaceSubrange(4 ..< 8, with: [0x00, 0x00, 0x00, 0x01])
        bytes.replaceSubrange(8 ..< 12, with: [0x01, 0x00, 0x00, 0x0c])

        XCTAssertTrue(ConfigStore.isArm64MachOForTesting(Data(bytes)))
    }

    func testArm64MachODetectionRejectsFatBinaryWithoutArm64Slice() {
        var bytes = [UInt8](repeating: 0, count: 28)
        bytes.replaceSubrange(0 ..< 4, with: [0xca, 0xfe, 0xba, 0xbe])
        bytes.replaceSubrange(4 ..< 8, with: [0x00, 0x00, 0x00, 0x01])
        bytes.replaceSubrange(8 ..< 12, with: [0x00, 0x00, 0x00, 0x07])

        XCTAssertFalse(ConfigStore.isArm64MachOForTesting(Data(bytes)))
    }

    func testAbsentSupportDirectoryOverrideUsesApplicationSupport() throws {
        XCTAssertEqual(
            try ApplicationPaths.resolveSupportDirectory(
                override: nil,
                applicationSupportDirectory: URL(filePath: "/fixture-app-support")
            ).path,
            "/fixture-app-support/OpenCodeServer"
        )
    }

    func testEmptySupportDirectoryOverrideUsesApplicationSupport() throws {
        XCTAssertEqual(
            try ApplicationPaths.resolveSupportDirectory(
                override: "",
                applicationSupportDirectory: URL(filePath: "/fixture-app-support")
            ).path,
            "/fixture-app-support/OpenCodeServer"
        )
    }

    func testWhitespaceSupportDirectoryOverrideUsesApplicationSupport() throws {
        XCTAssertEqual(
            try ApplicationPaths.resolveSupportDirectory(
                override: " \t\n",
                applicationSupportDirectory: URL(filePath: "/fixture-app-support")
            ).path,
            "/fixture-app-support/OpenCodeServer"
        )
    }

    func testRelativeSupportDirectoryOverrideIsRejected() {
        XCTAssertThrowsError(
            try ApplicationPaths.resolveSupportDirectory(
                override: "relative/support",
                applicationSupportDirectory: URL(filePath: "/fixture-app-support")
            )
        ) { error in
            guard case ApplicationPathsError.supportDirectoryOverrideMustBeAbsolute = error else {
                return XCTFail("Expected an absolute-path error, got \(error)")
            }
        }
    }

    func testAbsoluteSupportDirectoryOverrideIsUsedVerbatim() throws {
        XCTAssertEqual(
            try ApplicationPaths.resolveSupportDirectory(
                override: "/fixture-support",
                applicationSupportDirectory: URL(filePath: "/fixture-app-support")
            ).path,
            "/fixture-support"
        )
    }

    func testNonLoopbackWithoutPasswordIsPermittedButDetectableForWarning() {
        XCTAssertFalse(ConfigStore.isLoopback("10.0.0.254"))
        let config = AppConfig(hostname: "10.0.0.254")
        XCTAssertTrue(ConfigStore.validationIssues(config).isEmpty)
    }

    func testLoopbackDetectionCoversNonCanonicalForms() {
        XCTAssertTrue(ConfigStore.isLoopback("localhost"))
        XCTAssertTrue(ConfigStore.isLoopback("127.0.0.1"))
        XCTAssertTrue(ConfigStore.isLoopback("::1"))
        XCTAssertTrue(ConfigStore.isLoopback("[::1]"))
        XCTAssertTrue(
            ConfigStore.isLoopback("0:0:0:0:0:0:0:1"),
            "the full-form IPv6 loopback is still loopback"
        )
        XCTAssertTrue(
            ConfigStore.isLoopback("127.0.0.2"),
            "the whole 127.0.0.0/8 range is loopback, as the agent classifies it"
        )
        XCTAssertFalse(ConfigStore.isLoopback("10.0.0.254"))
        XCTAssertFalse(ConfigStore.isLoopback("::"))
        XCTAssertFalse(
            ConfigStore.isLoopback("::ffff:127.0.0.1"),
            "an IPv4-mapped IPv6 address is not the loopback literal"
        )
    }

    func testConfigurationValidationMirrorsAgentHostnameAndUsernameRules() {
        // Control characters beyond newline are rejected in both fields.
        XCTAssertFalse(
            ConfigStore.validationIssues(AppConfig(hostname: "example\u{07}.com")).isEmpty
        )
        XCTAssertFalse(
            ConfigStore.validationIssues(AppConfig(username: "open\u{07}code")).isEmpty
        )
        // DNS-label shape: leading/trailing hyphen, empty label, underscore,
        // oversized label, and a zone ID the agent's IPv6 parser rejects.
        for invalid in [
            "-example.com",
            "example.com-",
            "example..com",
            "exa_mple.com",
            "\(String(repeating: "a", count: 64)).com",
            "fe80::1%lo0",
            "has space.com",
            "slash/path"
        ] {
            XCTAssertFalse(
                ConfigStore.validationIssues(AppConfig(hostname: invalid)).isEmpty,
                invalid
            )
        }
        // Valid DNS names, IP literals, and bracketed IPv6 stay accepted.
        for valid in [
            "example.com",
            "a-b.example.com",
            "mymac",
            "localhost",
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.254",
            "::",
            "::1",
            "[::1]",
            "0:0:0:0:0:0:0:1",
            "127.00.0.1"
        ] {
            XCTAssertTrue(
                ConfigStore.validationIssues(AppConfig(hostname: valid)).isEmpty,
                valid
            )
        }
        // The username budget is measured in UTF-8 bytes, as the agent
        // measures it: 100 two-byte characters exceed it.
        XCTAssertFalse(
            ConfigStore.validationIssues(
                AppConfig(username: String(repeating: "\u{00e9}", count: 100))
            ).isEmpty
        )
        XCTAssertTrue(
            ConfigStore.validationIssues(
                AppConfig(username: String(repeating: "a", count: 128))
            ).isEmpty
        )
    }

    func testPasswordMenuLabelCoversAllThreeStatesWithoutRevealingSecrets() {
        XCTAssertEqual(passwordMenuLabel(.configured), "Password: ••••••••••••  Configured")
        XCTAssertEqual(
            passwordMenuLabel(.accessPending),
            "Password: Access not granted — open Settings"
        )
        XCTAssertEqual(passwordMenuLabel(.notConfigured), "Password: Not configured")
        XCTAssertEqual(passwordMenuLabel(nil), "Password: Unable to determine")
        XCTAssertEqual(authenticationMenuLabel(true), "Authentication: Enabled")
        XCTAssertEqual(authenticationMenuLabel(false), "Authentication: Not enabled")
        XCTAssertEqual(authenticationMenuLabel(nil), "Authentication: Unable to determine")
    }

    func testAgentNotificationDecodesGlobalEventID() throws {
        let data = Data(
            #"{"event_id":"9014e07c-64db-4d25-84be-11fbc87f3b07","kind":"recovered","title":"Recovered","message":"Healthy again"}"#.utf8
        )
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let event = try decoder.decode(AgentNotification.self, from: data)
        XCTAssertEqual(event.eventID, "9014e07c-64db-4d25-84be-11fbc87f3b07")
        XCTAssertEqual(event.kind, .recovered)
    }

    func testNotificationDeliveryLedgerDeduplicatesOnlyAcceptedEventIDs() {
        let suite = "OpenCodeServerTests.notifications.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let ledger = NotificationDeliveryLedger(defaults: defaults, capacity: 2)

        XCTAssertTrue(ledger.begin(eventID: "event-a"))
        XCTAssertFalse(ledger.begin(eventID: "event-a"), "an in-flight push must not duplicate")
        ledger.finish(eventID: "event-a", accepted: false)
        XCTAssertTrue(ledger.begin(eventID: "event-a"), "a rejected request must remain retryable")
        ledger.finish(eventID: "event-a", accepted: true)
        XCTAssertFalse(ledger.begin(eventID: "event-a"))

        XCTAssertTrue(ledger.begin(eventID: "event-b"))
        ledger.finish(eventID: "event-b", accepted: true)
        XCTAssertTrue(ledger.begin(eventID: "event-c"))
        ledger.finish(eventID: "event-c", accepted: true)
        XCTAssertTrue(ledger.begin(eventID: "event-a"), "the bounded ledger evicts its oldest ID")

        let relaunched = NotificationDeliveryLedger(defaults: defaults, capacity: 2)
        XCTAssertFalse(relaunched.begin(eventID: "event-b"), "accepted IDs survive relaunch")
        XCTAssertFalse(relaunched.begin(eventID: "event-c"), "accepted IDs survive relaunch")
    }

    func testUnchangedKeychainUpdateReturnsBeforeSecurityFrameworkWrite() throws {
        // The equality guard runs before any Security.framework call. This
        // keeps the test suite and an unchanged Save completely non-interactive
        // and prevents a redundant SecItemUpdate from revoking the grant.
        let value = String(repeating: "x", count: 16)
        XCTAssertEqual(
            try KeychainStore.update(
                account: "unused-test-account",
                password: value,
                knownCurrentPassword: value
            ),
            .unchanged
        )
    }

    func testKeychainStoreRejectsInvalidPasswords() {
        let account = "ocs-keychain-test-invalid"
        let tooLong = String(repeating: "a", count: KeychainStore.maximumPasswordLength + 1)
        for invalidPassword in [tooLong, "a\0b"] {
            XCTAssertThrowsError(
                try KeychainStore.create(account: account, password: invalidPassword)
            ) { error in
                guard case KeychainStore.StoreError.invalidLength = error else {
                    XCTFail("expected invalidLength, got \(error)")
                    return
                }
            }
        }
    }

    func testKeychainErrorsUseHumanFacingCopyAndRecognizeCancellation() {
        let canceled = KeychainStore.StoreError.readFailed(errSecUserCanceled)
        XCTAssertTrue(KeychainStore.isUserCancellation(canceled))
        XCTAssertFalse(canceled.localizedDescription.contains("OSStatus"))

        let denied = KeychainStore.StoreError.readFailed(errSecAuthFailed)
        XCTAssertFalse(KeychainStore.isUserCancellation(denied))
        XCTAssertTrue(KeychainStore.userFacingReadFailure(denied).contains("try again"))
        XCTAssertFalse(KeychainStore.userFacingReadFailure(denied).contains("OSStatus"))
    }

    func testKeychainAccountFallsBackToDefaultUsername() {
        XCTAssertEqual(KeychainStore.account(forUsername: ""), "opencode")
        XCTAssertEqual(KeychainStore.account(forUsername: "alice"), "alice")
    }

    func testCredentialMutationJournalRecoversStagedWriteWithoutSensitiveData() throws {
        let root = FileManager.default.temporaryDirectory.appending(
            path: "ocs-credential-journal-\(UUID().uuidString)",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let file = root.appending(path: "credential-notification.plist")
        let account = "fixture-account"

        let journal = try CredentialMutationJournal(fileURL: file)
        let generation = try journal.stage(account: account)
        XCTAssertTrue(journal.hasUnacknowledgedMutation)

        let persisted = try String(contentsOf: file, encoding: .utf8)
        XCTAssertTrue(persisted.contains(account))
        XCTAssertTrue(persisted.contains(String(generation)))
        for forbiddenKey in ["password", "hash", "length"] {
            XCTAssertFalse(persisted.localizedCaseInsensitiveContains(forbiddenKey))
        }
        let mode = try XCTUnwrap(
            FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions]
                as? NSNumber
        )
        XCTAssertEqual(mode.intValue & 0o777, 0o600)

        let recovered = try CredentialMutationJournal(fileURL: file)
        XCTAssertEqual(
            recovered.pending,
            CredentialMutationRecord(
                account: account,
                notice: .changed,
                generation: generation
            )
        )
    }

    @MainActor
    func testCredentialChangedPersistsAcrossIPCOutageAndClearsOnlyOnAck() throws {
        let root = FileManager.default.temporaryDirectory.appending(
            path: "ocs-credential-retry-\(UUID().uuidString)",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let account = "fixture-account"
        let keychain = InMemoryKeychain()
        let outcome = keychain.create(
            account: account,
            value: String(repeating: "v", count: 16)
        )
        let firstAttempt = expectation(description: "first notice attempted while IPC is unavailable")
        let acknowledged = expectation(description: "notice acknowledged after reconnect")
        let lock = NSLock()
        var ipcAvailable = false
        var attempts = 0

        let coordinator = try CredentialMutationCoordinator(
            fileURL: root.appending(path: "credential-notification.plist"),
            sender: { command in
                lock.lock()
                attempts += 1
                let available = ipcAvailable
                lock.unlock()
                XCTAssertEqual(command, .credentialChanged)
                guard available else {
                    firstAttempt.fulfill()
                    throw IPCError.systemCall("fixture IPC unavailable")
                }
                return Self.makeCredentialResponse(
                    account: account,
                    passwordState: .accessPending
                )
            }
        )
        coordinator.onPendingStateChange = {
            if !coordinator.hasUnacknowledgedMutation {
                acknowledged.fulfill()
            }
        }

        let generation = try coordinator.stage(account: account)
        try coordinator.commit(generation: generation, outcome: outcome)
        var authorizationStarted = false
        coordinator.performAfterAcknowledgement {
            authorizationStarted = true
        }
        coordinator.observe(Self.makeCredentialStatus(account: account, passwordState: .notConfigured))

        wait(for: [firstAttempt], timeout: 2)
        waitUntil { !coordinator.deliveryInFlight }
        XCTAssertTrue(coordinator.hasUnacknowledgedMutation)
        XCTAssertFalse(authorizationStarted)

        lock.lock()
        ipcAvailable = true
        lock.unlock()
        // This models the first pushed status after the subscription reconnects.
        coordinator.observe(Self.makeCredentialStatus(account: account, passwordState: .notConfigured))

        wait(for: [acknowledged], timeout: 2)
        XCTAssertFalse(coordinator.hasUnacknowledgedMutation)
        XCTAssertTrue(authorizationStarted)
        lock.lock()
        let finalAttempts = attempts
        lock.unlock()
        XCTAssertEqual(finalAttempts, 2)
    }

    @MainActor
    func testNewCredentialGenerationCannotBeClearedByOlderAck() throws {
        let root = FileManager.default.temporaryDirectory.appending(
            path: "ocs-credential-generation-\(UUID().uuidString)",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let account = "fixture-account"
        let firstStarted = expectation(description: "first generation started")
        let secondStarted = expectation(description: "second generation started")
        let allAcknowledged = expectation(description: "newest generation acknowledged")
        let releaseFirst = DispatchSemaphore(value: 0)
        let releaseSecond = DispatchSemaphore(value: 0)
        let lock = NSLock()
        var invocation = 0

        let coordinator = try CredentialMutationCoordinator(
            fileURL: root.appending(path: "credential-notification.plist"),
            sender: { _ in
                lock.lock()
                invocation += 1
                let current = invocation
                lock.unlock()
                if current == 1 {
                    firstStarted.fulfill()
                    releaseFirst.wait()
                } else {
                    secondStarted.fulfill()
                    releaseSecond.wait()
                }
                return Self.makeCredentialResponse(
                    account: account,
                    passwordState: .accessPending
                )
            }
        )
        coordinator.onPendingStateChange = {
            if !coordinator.hasUnacknowledgedMutation {
                allAcknowledged.fulfill()
            }
        }
        coordinator.observe(Self.makeCredentialStatus(account: account, passwordState: .configured))

        let first = try coordinator.stage(account: account)
        try coordinator.commit(generation: first, outcome: .updatedExisting)
        wait(for: [firstStarted], timeout: 2)

        let second = try coordinator.stage(account: account)
        try coordinator.commit(generation: second, outcome: .created)
        XCTAssertEqual(coordinator.pendingGeneration, second)
        releaseFirst.signal()

        wait(for: [secondStarted], timeout: 2)
        XCTAssertEqual(
            coordinator.pendingGeneration,
            second,
            "the first generation's ACK must not clear a newer pending notice"
        )
        releaseSecond.signal()

        wait(for: [allAcknowledged], timeout: 2)
        XCTAssertNil(coordinator.pendingGeneration)
        lock.lock()
        let finalInvocation = invocation
        lock.unlock()
        XCTAssertEqual(finalInvocation, 2)
    }

    func testSavingUnchangedConfigurationPreservesFileIdentity() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-config-\(Darwin.getpid())-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let store = ConfigStore(paths: paths)
        try store.save(AppConfig())
        let first = try FileManager.default.attributesOfItem(
            atPath: paths.configFile.path
        )[.systemFileNumber] as? NSNumber
        try store.save(AppConfig())
        let second = try FileManager.default.attributesOfItem(
            atPath: paths.configFile.path
        )[.systemFileNumber] as? NSNumber
        XCTAssertNotNil(first)
        XCTAssertEqual(first, second)
    }

    func testIPCClientConnectsToDarwinUnixSocket() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        let served = expectation(description: "IPC response served")
        DispatchQueue.global().async {
            defer { served.fulfill() }
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            let request = Self.readRequestLine(from: connection)
            XCTAssertEqual(request?["version"] as? Int, 6)
            XCTAssertEqual(request?["command"] as? String, "status")
            Self.writeSuccessfulResponse(to: connection)
        }

        let response = try fixture.client.send(.status)
        XCTAssertTrue(response.ok)
        wait(for: [served], timeout: 2)
    }

    func testIPCClientRejectsNonCurrentResponseProtocol() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        DispatchQueue.global().async {
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            let response = Data(
                #"{"version":2,"ok":true,"error":null,"status":null,"validation":null}"#.utf8
            ) + Data([0x0A])
            response.withUnsafeBytes {
                _ = Darwin.write(connection, $0.baseAddress, $0.count)
            }
        }

        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            guard case let IPCError.protocolMismatch(version) = error else {
                return XCTFail("Expected a protocol mismatch, got \(error)")
            }
            XCTAssertEqual(version, 2)
        }
    }

    func testIPCClientRejectsSuccessfulStatusResponseWithoutStatus() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        DispatchQueue.global().async {
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeResponseBody(
                #"{"version":6,"ok":true,"error":null,"status":null,"validation":null}"#,
                to: connection
            )
        }

        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            guard case IPCError.missingStatus = error else {
                return XCTFail("Expected a missing-status error, got \(error)")
            }
        }
    }

    func testIPCClientAcceptsInvalidValidationReportWithStatus() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        DispatchQueue.global().async {
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeResponseBody(
                """
                {"version":6,"ok":false,"error":null,"status":\(Self.statusJSON(state: "stopped")),\
                "validation":{"valid":false,"issues":["fixture issue"],\
                "selected_executable":null,"candidates":[]}}
                """,
                to: connection
            )
        }

        let response = try fixture.client.send(.validateConfig)
        XCTAssertFalse(response.ok)
        XCTAssertNil(response.error)
        XCTAssertEqual(response.status?.serverState, .stopped)
        XCTAssertEqual(response.validation?.valid, false)
        XCTAssertEqual(response.validation?.issues, ["fixture issue"])
    }

    func testIPCClientRejectsUnterminatedEOFResponse() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        DispatchQueue.global().async {
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeResponseBody(
                Self.successfulStatusResponseBody(),
                terminated: false,
                to: connection
            )
        }

        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            guard case IPCError.invalidFraming = error else {
                return XCTFail("Expected an invalid-framing error, got \(error)")
            }
        }
    }

    func testIPCClientEnforcesCompleteResponseWireLengthBoundaries() throws {
        for wireLength in [65_535, 65_536] {
            let fixture = try UnixSocketFixture()
            let listener = fixture.listener
            DispatchQueue.global().async {
                let connection = Darwin.accept(listener, nil, nil)
                guard connection >= 0 else { return }
                defer { Darwin.close(connection) }
                Self.readRequest(from: connection)
                Self.writeResponseWithWireLength(wireLength, to: connection)
            }
            XCTAssertTrue(try fixture.client.send(.status).ok)
            fixture.cleanUp()
        }

        let oversized = try UnixSocketFixture()
        defer { oversized.cleanUp() }
        let listener = oversized.listener
        DispatchQueue.global().async {
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeResponseWithWireLength(65_537, to: connection)
        }
        XCTAssertThrowsError(try oversized.client.send(.status)) { error in
            guard case IPCError.responseTooLarge = error else {
                return XCTFail("Expected a response-too-large error, got \(error)")
            }
        }
    }

    func testIPCClientReturnsRecoverableConnectionFailure() throws {
        let fixture = try UnixSocketFixture()
        fixture.closeListener()
        defer { fixture.cleanUp() }

        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            self.assertSystemError(error, prefix: "Unable to connect")
        }
    }

    func testIPCClientConnectTimeoutIsRecoverable() throws {
        // A full listen backlog makes connect() block indefinitely; the
        // client must fail within its timeout budget instead of hanging.
        let fixture = try UnixSocketFixture(timeoutMilliseconds: 200)
        defer { fixture.cleanUp() }
        try fixture.fillBacklog()

        let started = Date()
        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            self.assertSystemError(error, prefix: "Unable to connect")
        }
        XCTAssertLessThan(
            Date().timeIntervalSince(started),
            2,
            "a full listen backlog must fail within the connect timeout, not hang"
        )
    }

    func testIPCClientSuppressesSIGPIPEWhenPeerClosesBeforeWrite() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        let peerClosed = DispatchSemaphore(value: 0)
        let served = expectation(description: "Peer closed before request write")
        DispatchQueue.global().async {
            defer { served.fulfill() }
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else {
                peerClosed.signal()
                return
            }
            _ = Darwin.shutdown(connection, SHUT_RDWR)
            Darwin.close(connection)
            peerClosed.signal()
        }

        XCTAssertThrowsError(
            try fixture.client.send(.status) {
                XCTAssertEqual(peerClosed.wait(timeout: .now() + 1), .success)
            }
        ) { error in
            self.assertSystemError(error, prefix: "Unable to write")
        }
        wait(for: [served], timeout: 2)
    }

    func testIPCClientReturnsRecoverableErrorWhenPeerClosesDuringRead() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        let served = expectation(description: "Peer closed while client read")
        DispatchQueue.global().async {
            defer { served.fulfill() }
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            Self.readRequest(from: connection)
            _ = Darwin.shutdown(connection, SHUT_RDWR)
            Darwin.close(connection)
        }

        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            guard case IPCError.emptyResponse = error else {
                return XCTFail("Expected an empty-response IPC error, got \(error)")
            }
        }
        wait(for: [served], timeout: 2)
    }

    func testIPCClientReadTimeoutIsRecoverable() throws {
        let fixture = try UnixSocketFixture(timeoutMilliseconds: 100)
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        let releasePeer = DispatchSemaphore(value: 0)
        let served = expectation(description: "Peer held response past timeout")
        DispatchQueue.global().async {
            defer { served.fulfill() }
            let connection = Darwin.accept(listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            _ = releasePeer.wait(timeout: .now() + 1)
        }

        XCTAssertThrowsError(try fixture.client.send(.status)) { error in
            self.assertSystemError(error, prefix: "Unable to read")
        }
        releasePeer.signal()
        wait(for: [served], timeout: 2)
    }

    func testIPCClientRecoversOnNextPollAfterPeerCloses() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let listener = fixture.listener
        let served = expectation(description: "Failure and recovery served")
        DispatchQueue.global().async {
            defer { served.fulfill() }
            let first = Darwin.accept(listener, nil, nil)
            guard first >= 0 else { return }
            Self.readRequest(from: first)
            _ = Darwin.shutdown(first, SHUT_RDWR)
            Darwin.close(first)

            let second = Darwin.accept(listener, nil, nil)
            guard second >= 0 else { return }
            defer { Darwin.close(second) }
            Self.readRequest(from: second)
            Self.writeSuccessfulResponse(to: second)
        }

        XCTAssertThrowsError(try fixture.client.send(.status))
        XCTAssertTrue(try fixture.client.send(.status).ok)
        wait(for: [served], timeout: 2)
    }

    func testAgentStatusDecodesCurrentStartAnchor() throws {
        let base = """
        {
          "protocol_version": 6,
          "agent_version": "0.1.0",
          "agent_uptime_seconds": 5,
          "desired_state": "running",
          "server_state": "healthy",
          "health": "healthy",
          "fda": "verified",
          "uptime_seconds": 10,
          "endpoint": "127.0.0.1:4096",
          "username": "opencode",
          "password_state": "not_configured",
          "authentication_enabled": false,
          "action_capabilities": {
            "start": true,
            "stop": false,
            "restart": true,
            "continue_stop": false,
            "force_stop": false
          },
          "installed_version": null,
          "running_version": null,
          "version_pending": false,
          "config_pending": false,
          "config_error": null,
          "last_error": null,
          "pid": 42,
          "stop_grace_remaining_seconds": null,
          "notification": null,
          "process_started_at_unix_seconds": null,
          "bundle_version": "57"
        }
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let stopped = try decoder.decode(AgentStatus.self, from: Data(base.utf8))
        XCTAssertNil(stopped.processStartedAtUnixSeconds)

        let anchored = try decoder.decode(
            AgentStatus.self,
            from: Data(
                base.replacingOccurrences(
                    of: "\"process_started_at_unix_seconds\": null",
                    with: "\"process_started_at_unix_seconds\": 1700000000"
                ).utf8
            )
        )
        XCTAssertEqual(anchored.processStartedAtUnixSeconds, 1_700_000_000)
    }

    func testSubscriptionFramingSplitsLinesAndEnforcesBound() throws {
        var framing = SubscriptionFraming()
        var lines = try framing.append(Array("{\"a\":1}\n{\"b\":".utf8))
        XCTAssertEqual(lines.count, 1)
        XCTAssertEqual(String(data: lines[0], encoding: .utf8), "{\"a\":1}")
        lines = try framing.append(Array("2}\n".utf8))
        XCTAssertEqual(lines.count, 1)
        XCTAssertEqual(String(data: lines[0], encoding: .utf8), "{\"b\":2}")

        var oversized = SubscriptionFraming()
        XCTAssertThrowsError(
            try oversized.append(
                [UInt8](repeating: 0x61, count: SubscriptionFraming.maximumMessageBytes + 1)
            )
        ) { error in
            guard case IPCError.responseTooLarge = error else {
                return XCTFail("Expected a response-too-large error, got \(error)")
            }
        }
    }

    func testSubscriptionReceivesPushedStatuses() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let served = expectation(description: "subscription served")
        DispatchQueue.global().async {
            defer { served.fulfill() }
            let connection = Darwin.accept(fixture.listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            guard let request = Self.readRequestLine(from: connection),
                  let version = request["version"] as? Int,
                  let command = request["command"] as? String
            else { return }
            XCTAssertEqual(version, 6)
            XCTAssertEqual(command, "subscribe")
            for state in ["starting", "healthy"] {
                Self.writeStatusPush(state: state, to: connection)
                Thread.sleep(forTimeInterval: 0.05)
            }
            Thread.sleep(forTimeInterval: 0.2)
        }

        let subscription = AgentStatusSubscription(socketPath: fixture.socketURL.path)
        let received = expectation(description: "two pushed statuses")
        received.expectedFulfillmentCount = 2
        var states: [ServerState] = []
        subscription.onStatus = { status in
            states.append(status.serverState)
            received.fulfill()
        }
        subscription.start()
        defer { subscription.invalidate() }
        wait(for: [received, served], timeout: 5)
        XCTAssertEqual(states, [.starting, .healthy])
    }

    func testSubscriptionMissingStatusRetriesWithoutStartingStream() throws {
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let served = expectation(description: "missing status and recovery served")
        DispatchQueue.global().async {
            let first = Darwin.accept(fixture.listener, nil, nil)
            if first >= 0 {
                Self.readRequest(from: first)
                Self.writeResponseBody(
                    #"{"version":6,"ok":true,"error":null,"status":null,"validation":null}"#,
                    to: first
                )
                Darwin.close(first)
            }

            let second = Darwin.accept(fixture.listener, nil, nil)
            if second >= 0 {
                Self.readRequest(from: second)
                Self.writeStatusPush(state: "healthy", to: second)
                // Keep the valid stream open through the assertions. The
                // client-side invalidate below shuts this descriptor down,
                // allowing the fixture to finish without manufacturing a
                // post-stream disconnect.
                var byte: UInt8 = 0
                _ = Darwin.read(second, &byte, 1)
                Darwin.close(second)
            }
            served.fulfill()
        }

        let subscription = AgentStatusSubscription(
            socketPath: fixture.socketURL.path,
            timing: SubscriptionTiming(
                heartbeatTolerance: 5,
                readTimeoutMilliseconds: 200,
                backoff: [0.1]
            )
        )
        let received = expectation(description: "valid status received after retry")
        var states: [ServerState] = []
        var unreachableCount = 0
        subscription.onStatus = { status in
            states.append(status.serverState)
            received.fulfill()
        }
        subscription.onUnreachable = { unreachableCount += 1 }
        subscription.start()

        wait(for: [received], timeout: 5)
        XCTAssertEqual(states, [.healthy])
        XCTAssertEqual(unreachableCount, 0)

        subscription.invalidate()
        wait(for: [served], timeout: 5)
    }

    func testSubscriptionFramingCountsTheTerminatingNewlineTowardTheBound() throws {
        // Wire semantics: a complete message is body + newline and the whole
        // line must fit the bound. The longest legal body is one byte short.
        var framing = SubscriptionFraming()
        let legalBody = [UInt8](repeating: 0x61, count: SubscriptionFraming.maximumMessageBytes - 1)
        let lines = try framing.append(legalBody + [0x0A])
        XCTAssertEqual(lines.count, 1)
        XCTAssertEqual(lines[0].count, SubscriptionFraming.maximumMessageBytes - 1)

        // One byte more is rejected even though the line is terminated: this
        // exact shape bypassed the previous length check.
        var oversized = SubscriptionFraming()
        XCTAssertThrowsError(
            try oversized.append(
                [UInt8](repeating: 0x61, count: SubscriptionFraming.maximumMessageBytes) + [0x0A]
            )
        ) { error in
            guard case IPCError.responseTooLarge = error else {
                return XCTFail("Expected a response-too-large error, got \(error)")
            }
        }

        // A single read well beyond the bound is rejected even when terminated.
        var singleRead = SubscriptionFraming()
        XCTAssertThrowsError(
            try singleRead.append([UInt8](repeating: 0x61, count: 70_000) + [0x0A])
        ) { error in
            guard case IPCError.responseTooLarge = error else {
                return XCTFail("Expected a response-too-large error, got \(error)")
            }
        }

        // The bound is enforced across chunk boundaries: one byte short of
        // the limit is a valid pending buffer, the next byte is not.
        var chunked = SubscriptionFraming()
        XCTAssertEqual(
            try chunked.append(
                [UInt8](repeating: 0x61, count: SubscriptionFraming.maximumMessageBytes - 1)
            ),
            []
        )
        XCTAssertThrowsError(try chunked.append([0x61])) { error in
            guard case IPCError.responseTooLarge = error else {
                return XCTFail("Expected a response-too-large error, got \(error)")
            }
        }
        var chunkedLegal = SubscriptionFraming()
        XCTAssertEqual(
            try chunkedLegal.append(
                [UInt8](repeating: 0x61, count: SubscriptionFraming.maximumMessageBytes - 1)
            ),
            []
        )
        let completed = try chunkedLegal.append([0x0A])
        XCTAssertEqual(completed.count, 1)
        XCTAssertEqual(completed[0].count, SubscriptionFraming.maximumMessageBytes - 1)

        // Multiple complete messages in one read are all delivered.
        var multi = SubscriptionFraming()
        let multiLines = try multi.append(Array("{\"a\":1}\n{\"b\":2}\n".utf8))
        XCTAssertEqual(multiLines.count, 2)
        XCTAssertEqual(String(data: multiLines[1], encoding: .utf8), "{\"b\":2}")

        // An oversized message rejects the current framing instance, but a
        // fresh instance processes a legal message normally (the connection
        // ends and a new framing is created on reconnect).
        var afterOversized = SubscriptionFraming()
        XCTAssertThrowsError(
            try afterOversized.append(
                [UInt8](repeating: 0x61, count: SubscriptionFraming.maximumMessageBytes) + [0x0A]
            )
        )
        var fresh = SubscriptionFraming()
        let legalLines = try fresh.append(Array("{\"ok\":true}\n".utf8))
        XCTAssertEqual(legalLines.count, 1)
        XCTAssertEqual(String(data: legalLines[0], encoding: .utf8), "{\"ok\":true}")
    }

    func testSubscriptionInitialFailureRetriesAndRecoversWithoutUnreachable() throws {
        // P1-1: a connection that never streamed is retried silently; the
        // menu must not flap gray for an agent that simply is not up yet.
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }
        fixture.closeListener()
        Darwin.unlink(fixture.socketURL.path)

        let subscription = AgentStatusSubscription(
            socketPath: fixture.socketURL.path,
            timing: SubscriptionTiming(
                heartbeatTolerance: 2,
                readTimeoutMilliseconds: 200,
                backoff: [0.1]
            )
        )
        var unreachableCount = 0
        subscription.onUnreachable = { unreachableCount += 1 }
        let received = expectation(description: "status received after recovery")
        subscription.onStatus = { _ in received.fulfill() }
        subscription.start()
        defer { subscription.invalidate() }

        // The agent "starts" a moment later: the silent retry must connect.
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.4) {
            guard let _ = try? fixture.reopenListener() else { return }
            let connection = Darwin.accept(fixture.listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeStatusPush(state: "healthy", to: connection)
            Thread.sleep(forTimeInterval: 0.5)
        }

        wait(for: [received], timeout: 5)
        XCTAssertEqual(unreachableCount, 0)
    }

    func testSubscriptionDropAfterStreamingReportsUnreachableOnceAndRecovers() throws {
        // P1-1: a drop after streaming turns the menu gray immediately, the
        // backoff resets, and the next connection resumes streaming.
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        let served = expectation(description: "two connections served")
        served.expectedFulfillmentCount = 2
        DispatchQueue.global().async {
            // Connection 1 streams a status and then drops.
            let first = Darwin.accept(fixture.listener, nil, nil)
            if first >= 0 {
                Self.readRequest(from: first)
                Self.writeStatusPush(state: "starting", to: first)
                Thread.sleep(forTimeInterval: 0.2)
                Darwin.close(first)
            }
            served.fulfill()
            // Connection 2 streams a status and stays open, so the
            // assertions run while no second drop can occur.
            let second = Darwin.accept(fixture.listener, nil, nil)
            if second >= 0 {
                Self.readRequest(from: second)
                Self.writeStatusPush(state: "healthy", to: second)
                served.fulfill()
                Thread.sleep(forTimeInterval: 2)
                Darwin.close(second)
            } else {
                served.fulfill()
            }
        }

        let subscription = AgentStatusSubscription(
            socketPath: fixture.socketURL.path,
            timing: SubscriptionTiming(
                heartbeatTolerance: 5,
                readTimeoutMilliseconds: 200,
                backoff: [0.1]
            )
        )
        let received = expectation(description: "two streamed statuses")
        received.expectedFulfillmentCount = 2
        var unreachableCount = 0
        subscription.onStatus = { _ in received.fulfill() }
        subscription.onUnreachable = { unreachableCount += 1 }
        subscription.start()
        defer { subscription.invalidate() }

        wait(for: [received, served], timeout: 5)
        XCTAssertEqual(unreachableCount, 1)
    }

    func testSubscriptionMidStreamTruncationReportsUnreachable() throws {
        // P1-1: a connection that streamed and then drops mid-message is a
        // disconnect, not a silent retry.
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        DispatchQueue.global().async {
            let connection = Darwin.accept(fixture.listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeStatusPush(state: "healthy", to: connection)
            // Truncate the next message: no terminating newline, then EOF.
            let partial = Data("{\"version\":4,\"ok\":tr".utf8)
            partial.withUnsafeBytes { _ = Darwin.write(connection, $0.baseAddress, $0.count) }
        }

        let subscription = AgentStatusSubscription(
            socketPath: fixture.socketURL.path,
            timing: SubscriptionTiming(
                heartbeatTolerance: 5,
                readTimeoutMilliseconds: 200,
                backoff: [10]
            )
        )
        let unreachable = expectation(description: "unreachable reported")
        subscription.onUnreachable = { unreachable.fulfill() }
        subscription.start()
        defer { subscription.invalidate() }

        wait(for: [unreachable], timeout: 5)
    }

    func testSubscriptionHeartbeatTimeoutReportsUnreachable() throws {
        // P1-1: a silent connection past the heartbeat tolerance is a drop.
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        DispatchQueue.global().async {
            let connection = Darwin.accept(fixture.listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeStatusPush(state: "healthy", to: connection)
            // Stay connected but silent past the heartbeat tolerance.
            Thread.sleep(forTimeInterval: 2)
        }

        let subscription = AgentStatusSubscription(
            socketPath: fixture.socketURL.path,
            timing: SubscriptionTiming(
                heartbeatTolerance: 0.4,
                readTimeoutMilliseconds: 200,
                backoff: [10]
            )
        )
        let unreachable = expectation(description: "heartbeat timeout reported")
        subscription.onUnreachable = { unreachable.fulfill() }
        subscription.start()
        defer { subscription.invalidate() }

        wait(for: [unreachable], timeout: 5)
    }

    func testSubscriptionInvalidateIsQuietAndStopsTheWorker() throws {
        // P1-1: invalidate() must not produce a false unreachable report and
        // the worker thread must actually exit.
        let fixture = try UnixSocketFixture()
        defer { fixture.cleanUp() }

        DispatchQueue.global().async {
            let connection = Darwin.accept(fixture.listener, nil, nil)
            guard connection >= 0 else { return }
            defer { Darwin.close(connection) }
            Self.readRequest(from: connection)
            Self.writeStatusPush(state: "healthy", to: connection)
            Thread.sleep(forTimeInterval: 2)
        }

        let subscription = AgentStatusSubscription(
            socketPath: fixture.socketURL.path,
            timing: SubscriptionTiming(
                heartbeatTolerance: 5,
                readTimeoutMilliseconds: 200,
                backoff: [0.1]
            )
        )
        let received = expectation(description: "status streamed")
        subscription.onStatus = { _ in received.fulfill() }
        var unreachableCount = 0
        subscription.onUnreachable = { unreachableCount += 1 }
        subscription.start()
        wait(for: [received], timeout: 5)

        subscription.invalidate()
        let deadline = Date().addingTimeInterval(3)
        while subscription.thread?.isFinished == false, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        XCTAssertEqual(subscription.thread?.isFinished, true)
        XCTAssertEqual(unreachableCount, 0)
    }

    private func assertSystemError(
        _ error: Error,
        prefix: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        guard case let IPCError.systemCall(message) = error else {
            return XCTFail("Expected a system-call IPC error, got \(error)", file: file, line: line)
        }
        XCTAssertTrue(message.hasPrefix(prefix), "Unexpected error: \(message)", file: file, line: line)
    }

    @MainActor
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

    private static func makeCredentialResponse(
        account: String,
        passwordState: PasswordState
    ) -> AgentResponse {
        AgentResponse(
            version: ipcProtocolVersion,
            ok: true,
            error: nil,
            status: makeCredentialStatus(account: account, passwordState: passwordState),
            validation: nil
        )
    }

    private static func makeCredentialStatus(
        account: String,
        passwordState: PasswordState
    ) -> AgentStatus {
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

    private static func readRequest(from descriptor: Int32) {
        var request = [UInt8](repeating: 0, count: 1_024)
        _ = request.withUnsafeMutableBytes {
            Darwin.read(descriptor, $0.baseAddress, $0.count)
        }
    }

    private static func writeSuccessfulResponse(to descriptor: Int32) {
        writeResponseBody(successfulStatusResponseBody(), to: descriptor)
    }

    private static func readRequestLine(from descriptor: Int32) -> [String: Any]? {
        var request = Data()
        var byte: UInt8 = 0
        while byte != 0x0A {
            let count = Darwin.read(descriptor, &byte, 1)
            guard count > 0 else { return nil }
            request.append(byte)
        }
        return try? JSONSerialization.jsonObject(
            with: Data(request.dropLast())
        ) as? [String: Any]
    }

    private static func writeStatusPush(state: String, to descriptor: Int32) {
        writeResponseBody(
            """
            {"version":6,"ok":true,"error":null,"status":\(statusJSON(state: state)),\
            "validation":null}
            """,
            to: descriptor
        )
    }

    private static func successfulStatusResponseBody() -> String {
        """
        {"version":6,"ok":true,"error":null,"status":\(statusJSON(state: "healthy")),\
        "validation":null}
        """
    }

    private static func statusJSON(state: String) -> String {
        """
        {"protocol_version":6,"agent_version":"test","agent_uptime_seconds":1,\
        "desired_state":"running","server_state":"\(state)","health":"healthy",\
        "fda":"verified","endpoint":"127.0.0.1:4096","username":"opencode",\
        "password_state":"not_configured","authentication_enabled":false,\
        "action_capabilities":{"start":true,"stop":false,"restart":true,\
        "continue_stop":false,"force_stop":false},\
        "version_pending":false,"config_pending":false,"bundle_version":"57"}
        """
    }

    private static func writeResponseWithWireLength(_ wireLength: Int, to descriptor: Int32) {
        var body = Data(successfulStatusResponseBody().utf8)
        precondition(body.count < wireLength)
        body.append(Data(repeating: 0x20, count: wireLength - body.count - 1))
        body.append(0x0A)
        try? IPCClient.writeAll(body, to: descriptor)
    }

    private static func writeResponseBody(
        _ body: String,
        terminated: Bool = true,
        to descriptor: Int32
    ) {
        var response = Data(body.utf8)
        if terminated {
            response.append(0x0A)
        }
        try? IPCClient.writeAll(response, to: descriptor)
    }
}

private final class InMemoryKeychain {
    private let lock = NSLock()
    private var values: [String: String] = [:]

    func create(account: String, value: String) -> KeychainStore.SaveOutcome {
        lock.lock()
        defer { lock.unlock() }
        precondition(values[account] == nil)
        values[account] = value
        return .created
    }
}

private final class UnixSocketFixture {
    let root: URL
    let socketURL: URL
    private(set) var listener: Int32
    private var backlogFiller: Int32 = -1
    let client: IPCClient

    init(timeoutMilliseconds: Int = 3_000) throws {
        let nonce = UUID().uuidString.prefix(8)
        root = URL(
            filePath: "/private/tmp/ocs-ipc-\(Darwin.getpid())-\(nonce)",
            directoryHint: .isDirectory
        )
        let run = root.appending(path: "run")
        try FileManager.default.createDirectory(
            at: run,
            withIntermediateDirectories: true
        )
        socketURL = run.appending(path: "control.sock")
        listener = try Self.makeUnixListener(path: socketURL.path)
        client = IPCClient(
            socketPath: socketURL.path,
            timeoutMilliseconds: timeoutMilliseconds
        )
    }

    func closeListener() {
        if listener >= 0 {
            Darwin.close(listener)
            listener = -1
        }
        if backlogFiller >= 0 {
            Darwin.close(backlogFiller)
            backlogFiller = -1
        }
    }

    /// Opens one raw connection and keeps it pending, filling the backlog-1
    /// listen queue so the next connect exercises the bounded non-blocking
    /// connect path instead of completing instantly.
    func fillBacklog() throws {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw POSIXError(.EIO)
        }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let maximumPathLength = MemoryLayout.size(ofValue: address.sun_path)
        withUnsafeMutablePointer(to: &address.sun_path) { pointer in
            pointer.withMemoryRebound(
                to: CChar.self,
                capacity: maximumPathLength
            ) { buffer in
                socketURL.path.withCString { source in
                    _ = strlcpy(buffer, source, maximumPathLength)
                }
            }
        }
        let length = socklen_t(
            MemoryLayout.size(ofValue: address.sun_len) +
                MemoryLayout.size(ofValue: address.sun_family) +
                socketURL.path.utf8.count + 1
        )
        address.sun_len = UInt8(length)
        let connectResult = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, length)
            }
        }
        guard connectResult == 0 else {
            Darwin.close(descriptor)
            throw POSIXError(.EIO)
        }
        backlogFiller = descriptor
    }

    /// Rebinds the fixture socket path after it was closed/unlinked, so a
    /// test can model an agent that starts after the client.
    func reopenListener() throws {
        closeListener()
        Darwin.unlink(socketURL.path)
        listener = try Self.makeUnixListener(path: socketURL.path)
    }

    func cleanUp() {
        closeListener()
        try? FileManager.default.removeItem(at: root)
    }

    private static func makeUnixListener(path: String) throws -> Int32 {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw POSIXError(.EIO)
        }
        do {
            var address = sockaddr_un()
            address.sun_family = sa_family_t(AF_UNIX)
            let maximumPathLength = MemoryLayout.size(ofValue: address.sun_path)
            guard path.utf8.count + 1 <= maximumPathLength else {
                throw POSIXError(.ENAMETOOLONG)
            }
            withUnsafeMutablePointer(to: &address.sun_path) { pointer in
                pointer.withMemoryRebound(
                    to: CChar.self,
                    capacity: maximumPathLength
                ) { buffer in
                    path.withCString { source in
                        _ = strlcpy(buffer, source, maximumPathLength)
                    }
                }
            }
            let length = socklen_t(
                MemoryLayout.size(ofValue: address.sun_len) +
                    MemoryLayout.size(ofValue: address.sun_family) +
                    path.utf8.count + 1
            )
            address.sun_len = UInt8(length)
            let bindResult = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.bind(descriptor, $0, length)
                }
            }
            guard bindResult == 0, Darwin.listen(descriptor, 1) == 0 else {
                throw POSIXError(.EIO)
            }
            return descriptor
        } catch {
            Darwin.close(descriptor)
            throw error
        }
    }
}
