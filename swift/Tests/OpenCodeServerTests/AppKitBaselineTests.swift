@testable import OpenCodeServer
import AppKit
import Foundation
import XCTest

@MainActor
final class AppKitBaselineTests: XCTestCase {
    func testMainBundleExplainsLocalNetworkUse() throws {
        let purpose = try XCTUnwrap(
            Bundle.main.object(
                forInfoDictionaryKey: "NSLocalNetworkUsageDescription"
            ) as? String
        )
        XCTAssertEqual(
            purpose,
            "OpenCodeServer advertises your configured OpenCode service on the local network when you enable mDNS."
        )
    }

    func testApplicationMenuContainsSupportedCommands() throws {
        let applicationMenu = try submenu("OpenCodeServer")
        XCTAssertNotNil(applicationMenu.item(withTitle: "About OpenCodeServer"))

        let settings = try XCTUnwrap(applicationMenu.item(withTitle: "Settings…"))
        XCTAssertEqual(settings.keyEquivalent, ",")
        XCTAssertEqual(settings.action, #selector(AppDelegate.showSettings(_:)))
        XCTAssertTrue(settings.target is AppDelegate)

        XCTAssertNotNil(applicationMenu.item(withTitle: "Services"))
        XCTAssertNotNil(applicationMenu.item(withTitle: "Hide OpenCodeServer"))
        XCTAssertNotNil(applicationMenu.item(withTitle: "Hide Others"))
        XCTAssertNotNil(applicationMenu.item(withTitle: "Show All"))

        let quit = try XCTUnwrap(applicationMenu.item(withTitle: "Quit OpenCodeServer"))
        XCTAssertEqual(quit.keyEquivalent, "q")
        XCTAssertEqual(quit.action, #selector(NSApplication.terminate(_:)))
    }

    func testEditMenuRoutesStandardActionsThroughFirstResponder() throws {
        let editMenu = try submenu("Edit")
        let expected: [(String, String, Selector)] = [
            ("Undo", "z", Selector(("undo:"))),
            ("Redo", "Z", Selector(("redo:"))),
            ("Cut", "x", #selector(NSText.cut(_:))),
            ("Copy", "c", #selector(NSText.copy(_:))),
            ("Paste", "v", #selector(NSText.paste(_:))),
            ("Paste and Match Style", "V", #selector(NSTextView.pasteAsPlainText(_:))),
            ("Delete", "", #selector(NSText.delete(_:))),
            ("Select All", "a", #selector(NSText.selectAll(_:)))
        ]

        for (title, keyEquivalent, action) in expected {
            let item = try XCTUnwrap(editMenu.item(withTitle: title))
            XCTAssertEqual(item.keyEquivalent, keyEquivalent)
            XCTAssertEqual(item.action, action)
            XCTAssertNil(item.target)
        }
    }

    func testWindowMenuContainsNativeBaselineItems() throws {
        let windowMenu = try submenu("Window")
        let titles = windowMenu.items.filter { !$0.isSeparatorItem }.map(\.title)
        // macOS 26 supplies additional native tiling and arrangement commands
        // while a window is active. Preserve the standard responder-chain
        // baseline without freezing the menu to an obsolete system subset.
        XCTAssertTrue(titles.contains("Minimize"))
        XCTAssertTrue(titles.contains("Bring All to Front"))
        XCTAssertNil(windowMenu.item(withTitle: "Minimize")?.target)
        XCTAssertNil(windowMenu.item(withTitle: "Bring All to Front")?.target)
    }

    func testSettingsUsesNativeAppKitFieldsForEveryEditableValue() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-appkit-\(ProcessInfo.processInfo.processIdentifier)-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: try makeCredentialMutationCoordinator(root: root),
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {}
        )
        let contentView = try XCTUnwrap(controller.window?.contentView)
        let fields = descendants(of: contentView).compactMap { $0 as? NSTextField }

        for label in [
            "Listening address",
            "Port",
            "Username",
            "Visible password",
            "OpenCode executable"
        ] {
            let field = fields.first { $0.accessibilityLabel() == label }
            XCTAssertNotNil(field)
            XCTAssertFalse(field is NSSecureTextField)
        }

        let password = fields.first { $0.accessibilityLabel() == "Password" }
        XCTAssertTrue(password is NSSecureTextField)
    }

    func testSettingsFormKeepsAppleColumnAlignmentDuringKeychainRead() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-settings-layout-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let probed = expectation(description: "credential existence probe")
        let readStarted = expectation(description: "explicit credential read started")
        let releaseRead = DispatchSemaphore(value: 0)
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: try makeCredentialMutationCoordinator(root: root),
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainContains: { _ in
                probed.fulfill()
                return true
            },
            keychainLoad: { _ in
                readStarted.fulfill()
                releaseRead.wait()
                throw KeychainStore.StoreError.readFailed(errSecUserCanceled)
            }
        )
        defer {
            releaseRead.signal()
            controller.window?.orderOut(nil)
        }

        controller.present()
        wait(for: [probed], timeout: 1)
        let content = try XCTUnwrap(controller.window?.contentView)
        let allViews = descendants(of: content)
        let fields = allViews.compactMap { $0 as? NSTextField }
        let buttons = allViews.compactMap { $0 as? NSButton }

        let editButton = try XCTUnwrap(
            buttons.first { $0.accessibilityLabel() == "Edit saved password" }
        )
        let deadline = Date().addingTimeInterval(1)
        while editButton.isHidden, Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.01))
        }
        XCTAssertFalse(editButton.isHidden)
        content.layoutSubtreeIfNeeded()

        func formLabel(_ title: String) throws -> NSTextField {
            try XCTUnwrap(fields.first { $0.stringValue == title && !$0.isEditable })
        }
        func field(_ accessibilityLabel: String) throws -> NSTextField {
            try XCTUnwrap(fields.first { $0.accessibilityLabel() == accessibilityLabel })
        }
        func leading(_ view: NSView) -> CGFloat {
            guard let superview = view.superview else { return view.frame.minX }
            // Auto Layout aligns views by alignment rectangles, not raw
            // frames. Bordered text fields have a two-point frame inset that
            // must not be mistaken for a shifted form column.
            return superview.convert(view.alignmentRect(forFrame: view.frame), to: content).minX
        }
        func trailing(_ view: NSView) -> CGFloat {
            view.convert(view.bounds, to: content).maxX
        }
        func centerY(_ view: NSView) -> CGFloat {
            view.convert(view.bounds, to: content).midY
        }

        let labels = try [
            formLabel("Listening address"),
            formLabel("Port"),
            formLabel("Username"),
            formLabel("Password"),
            formLabel("Agent access"),
            formLabel("Startup")
        ]
        let labelTrailing = trailing(labels[0])
        for label in labels.dropFirst() {
            XCTAssertEqual(trailing(label), labelTrailing, accuracy: 0.5)
            XCTAssertEqual(label.alignment, .right)
        }
        let passwordLabel = labels[3]
        let agentAccessLabel = labels[4]
        let startupLabel = labels[5]

        let hostname = try field("Listening address")
        let port = try field("Port")
        let username = try field("Username")
        let passwordStatus = try field("Password status")
        let agentAccess = try XCTUnwrap(fields.first { $0.stringValue == "Unknown" })
        let controlLeading = leading(hostname)
        for view in [port, username, passwordStatus, agentAccess] {
            XCTAssertEqual(leading(view), controlLeading, accuracy: 0.5)
        }
        XCTAssertLessThan(port.frame.width, hostname.frame.width)
        XCTAssertGreaterThanOrEqual(
            port.frame.width,
            textFieldWidth(matching: port, anticipatedText: "65535")
        )
        XCTAssertGreaterThanOrEqual(
            hostname.frame.width,
            textFieldWidth(
                matching: hostname,
                anticipatedText: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"
            )
        )

        let agentLogin = try XCTUnwrap(
            buttons.first { $0.title == "Run OpenCodeServerAgent at login" }
        )
        let appLogin = try XCTUnwrap(
            buttons.first { $0.title == "Open OpenCodeServer at login" }
        )
        XCTAssertEqual(leading(agentLogin), controlLeading, accuracy: 0.5)
        XCTAssertEqual(leading(appLogin), controlLeading, accuracy: 0.5)
        XCTAssertFalse(fields.contains { $0.stringValue == "OpenCodeServerAgent" })
        XCTAssertFalse(fields.contains { $0.stringValue == "OpenCodeServer" })

        let advancedButton = try XCTUnwrap(buttons.first { $0.title == "Advanced" })
        let candidatePopup = try XCTUnwrap(
            allViews.compactMap { $0 as? NSPopUpButton }.first {
                $0.accessibilityLabel() == "Detected OpenCode executable"
            }
        )
        advancedButton.performClick(nil)
        content.layoutSubtreeIfNeeded()
        XCTAssertEqual(leading(candidatePopup), controlLeading, accuracy: 0.5)
        let executable = try field("OpenCode executable")
        XCTAssertGreaterThanOrEqual(
            executable.frame.width,
            textFieldWidth(
                matching: executable,
                anticipatedText: "Automatic discovery"
            )
        )

        let stableLeading = leading(hostname)
        let stablePasswordCenterY = centerY(passwordLabel)
        let stableAgentAccessCenterY = centerY(agentAccessLabel)
        let stableStartupCenterY = centerY(startupLabel)
        editButton.performClick(nil)
        wait(for: [readStarted], timeout: 1)
        content.layoutSubtreeIfNeeded()
        XCTAssertEqual(leading(hostname), stableLeading, accuracy: 0.5)
        XCTAssertEqual(leading(username), stableLeading, accuracy: 0.5)
        let spinner = try XCTUnwrap(
            descendants(of: content).compactMap { $0 as? NSProgressIndicator }.first
        )
        XCTAssertFalse(spinner.isHidden)
        XCTAssertEqual(leading(spinner), stableLeading, accuracy: 0.5)
        XCTAssertEqual(centerY(spinner), centerY(passwordLabel), accuracy: 0.5)
        XCTAssertEqual(centerY(passwordLabel), stablePasswordCenterY, accuracy: 0.5)
        XCTAssertEqual(centerY(agentAccessLabel), stableAgentAccessCenterY, accuracy: 0.5)
        XCTAssertEqual(centerY(startupLabel), stableStartupCenterY, accuracy: 0.5)

        releaseRead.signal()
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        let feedback = try XCTUnwrap(
            fields.first { $0.accessibilityLabel() == "Settings feedback" }
        )
        XCTAssertFalse(feedback.stringValue.contains("OSStatus"))
        XCTAssertTrue(feedback.stringValue.isEmpty)
        XCTAssertEqual(leading(hostname), stableLeading, accuracy: 0.5)
        XCTAssertFalse(
            controller.window?.standardWindowButton(.miniaturizeButton)?.isEnabled ?? true
        )
        XCTAssertFalse(controller.window?.standardWindowButton(.zoomButton)?.isEnabled ?? true)
        assertNoAmbiguousProductLayout(in: content)
    }

    func testSettingsRefitsForWrappedRuntimeFeedbackWithoutClipping() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-settings-feedback-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let probeStarted = expectation(description: "credential probe started")
        let releaseProbe = DispatchSemaphore(value: 0)
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: try makeCredentialMutationCoordinator(root: root),
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainContains: { _ in
                probeStarted.fulfill()
                releaseProbe.wait()
                throw NSError(
                    domain: "SettingsLayoutTest",
                    code: 1,
                    userInfo: [
                        NSLocalizedDescriptionKey:
                            "The login keychain could not be checked while another security operation was pending. Reopen Settings and retry before changing the username or password."
                    ]
                )
            },
            keychainLoad: { _ in nil }
        )
        defer {
            releaseProbe.signal()
            controller.window?.orderOut(nil)
        }

        controller.present()
        wait(for: [probeStarted], timeout: 1)
        let initialHeight = try XCTUnwrap(controller.window).contentView?.frame.height ?? 0
        releaseProbe.signal()

        let content = try XCTUnwrap(controller.window?.contentView)
        let deadline = Date().addingTimeInterval(1)
        var feedback: NSTextField?
        while Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.01))
            feedback = descendants(of: content).compactMap { $0 as? NSTextField }.first {
                $0.accessibilityLabel() == "Settings feedback" && !$0.stringValue.isEmpty
            }
            if feedback != nil { break }
        }
        let resolvedFeedback = try XCTUnwrap(feedback)
        content.layoutSubtreeIfNeeded()
        let finalHeight = content.frame.height
        XCTAssertGreaterThan(finalHeight, initialHeight)

        let feedbackRect = resolvedFeedback.convert(resolvedFeedback.bounds, to: content)
        XCTAssertGreaterThanOrEqual(feedbackRect.minY, content.bounds.minY)
        XCTAssertLessThanOrEqual(feedbackRect.maxY, content.bounds.maxY)
        let saveButton = try XCTUnwrap(
            descendants(of: content).compactMap { $0 as? NSButton }.first { $0.title == "Save" }
        )
        let saveRect = saveButton.convert(saveButton.bounds, to: content)
        XCTAssertGreaterThanOrEqual(saveRect.minY, content.bounds.minY)
        XCTAssertLessThanOrEqual(saveRect.maxY, content.bounds.maxY)
        assertNoAmbiguousProductLayout(in: content)
    }

    func testOpeningSettingsProbesButDoesNotDecryptCredential() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-settings-probe-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let probed = expectation(description: "attribute-only password probe")
        let decrypted = expectation(description: "decrypt-class password read")
        decrypted.isInverted = true
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: try makeCredentialMutationCoordinator(root: root),
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainContains: { _ in
                probed.fulfill()
                return true
            },
            keychainLoad: { _ in
                decrypted.fulfill()
                return "must-not-be-read"
            }
        )

        controller.present()
        wait(for: [probed, decrypted], timeout: 0.25)
        controller.window?.orderOut(nil)
    }

    func testSaveFeedbackRendersFromLiveCredentialState() {
        // The straight-line replacement for the v49/v53 write-then-patch
        // design: the green Save text is a pure function of the live
        // credential state, so guidance appears exactly while Allow Keychain
        // Access is actionable and vanishes the moment it completes.
        let changed = SettingsWindowController.SaveFeedbackContext(
            passwordStored: true,
            accountChanged: false
        )
        let base =
            "Saved. Restart OpenCode when you want these changes to take effect."

        XCTAssertEqual(
            SettingsWindowController.saveFeedbackText(
                context: changed,
                passwordState: .configured
            ),
            base,
            "a granted credential needs no guidance"
        )
        XCTAssertEqual(
            SettingsWindowController.saveFeedbackText(
                context: changed,
                passwordState: .notConfigured
            ),
            base,
            "no Keychain item means there is nothing to grant"
        )
        XCTAssertEqual(
            SettingsWindowController.saveFeedbackText(
                context: changed,
                passwordState: nil
            ),
            base,
            "an unreachable agent cannot consume an Allow Keychain Access click"
        )
        let pending = SettingsWindowController.saveFeedbackText(
            context: changed,
            passwordState: .accessPending
        )
        XCTAssertTrue(
            pending.hasPrefix(base),
            "the pending guidance keeps the stable save prefix"
        )
        XCTAssertTrue(
            pending.contains("Allow Keychain Access"),
            "guidance appears exactly while the button is actionable"
        )
        let accountChanged = SettingsWindowController.saveFeedbackText(
            context: SettingsWindowController.SaveFeedbackContext(
                passwordStored: true,
                accountChanged: true
            ),
            passwordState: .accessPending
        )
        XCTAssertTrue(
            accountChanged.contains("new Keychain item"),
            "a username change adds its reason while access is pending"
        )
        XCTAssertFalse(
            SettingsWindowController.saveFeedbackText(
                context: SettingsWindowController.SaveFeedbackContext(
                    passwordStored: true,
                    accountChanged: true
                ),
                passwordState: .configured
            ).contains("new Keychain item"),
            "the username-change note retires with the grant as well"
        )
        XCTAssertEqual(
            SettingsWindowController.saveFeedbackText(
                context: SettingsWindowController.SaveFeedbackContext(
                    passwordStored: false,
                    accountChanged: false
                ),
                passwordState: .accessPending
            ),
            base,
            "a blank password keeps OpenCode's native behavior, no guidance"
        )
    }

    func testCredentialAuthorizationOfferCoversFirstCreationAndRealChanges() {
        XCTAssertTrue(
            SettingsWindowController.needsCredentialAuthorization(
                passwordIsEmpty: false,
                outcome: .created,
                passwordState: .notConfigured
            ),
            "the first saved password needs an explicit agent grant"
        )
        XCTAssertTrue(
            SettingsWindowController.needsCredentialAuthorization(
                passwordIsEmpty: false,
                outcome: .updatedExisting,
                passwordState: .configured
            ),
            "a real password update invalidates the prior grant"
        )
        XCTAssertTrue(
            SettingsWindowController.needsCredentialAuthorization(
                passwordIsEmpty: false,
                outcome: .unchanged,
                passwordState: .accessPending
            ),
            "a pending grant remains actionable when another setting changes"
        )
        XCTAssertFalse(
            SettingsWindowController.needsCredentialAuthorization(
                passwordIsEmpty: false,
                outcome: .unchanged,
                passwordState: .configured
            )
        )
        XCTAssertFalse(
            SettingsWindowController.needsCredentialAuthorization(
                passwordIsEmpty: true,
                outcome: .deleted,
                passwordState: .accessPending
            ),
            "clearing the password must not offer Keychain authorization"
        )
        XCTAssertTrue(
            SettingsWindowController.shouldOfferRestart(
                settingsChanged: false,
                credentialChanged: true,
                openCodeIsRunning: true
            ),
            "first-password creation is contextual while OpenCode is running"
        )
        XCTAssertFalse(
            SettingsWindowController.shouldOfferRestart(
                settingsChanged: true,
                credentialChanged: true,
                openCodeIsRunning: false
            ),
            "a stopped OpenCode must reveal inline authorization without a modal"
        )
        XCTAssertEqual(
            SettingsWindowController.credentialNotice(for: .updatedExisting),
            .credentialChanged
        )
        XCTAssertEqual(
            SettingsWindowController.credentialNotice(for: .deleted),
            .credentialRemoved
        )
        XCTAssertEqual(
            SettingsWindowController.credentialNotice(for: .created),
            .credentialChanged
        )
        XCTAssertNil(SettingsWindowController.credentialNotice(for: .unchanged))
    }

    func testSaveRollsBackStagedCredentialNoticeWhenKeychainWriteFails() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-save-keychain-fail-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let coordinator = try makeCredentialMutationCoordinator(root: root)
        let failure = NSError(
            domain: "SettingsSaveFlowTest",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "simulated Keychain write failure"]
        )
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: coordinator,
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainCreate: { _, _ in throw failure }
        )
        controller.hostnameField.stringValue = "127.0.0.1"
        controller.portField.integerValue = 4096
        controller.usernameField.stringValue = "opencode"
        controller.credentialEditorState = .absent
        controller.securePasswordField.stringValue = "test-password"
        // Keep the "disable OpenCodeServerAgent" guard out of the way
        // regardless of the host machine's registration state.
        controller.openCodeServerAgentLoginButton.state = .on

        controller.save()
        // save() disables Save and shows "Saving…" synchronously;
        // finishSave re-enables the button as its first act on the main
        // queue, so that — not the intermediate text — is the terminal
        // signal for the asynchronous round trip.
        XCTAssertTrue(pumpMainRunLoop(until: { controller.saveButton.isEnabled }))

        XCTAssertEqual(
            controller.feedbackLabel.stringValue,
            "simulated Keychain write failure"
        )
        XCTAssertFalse(
            coordinator.hasUnacknowledgedMutation,
            "a failed Keychain write must roll the staged notice back"
        )
        XCTAssertTrue(controller.saveButton.isEnabled)
        XCTAssertNotNil(controller.loadedConfig)
    }

    func testSaveKeepsStagedCredentialNoticeWhenJournalCommitFails() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-save-commit-fail-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        var journalWrites = 0
        let coordinator = try CredentialMutationCoordinator(
            fileURL: root.appending(path: "credential-notification.plist"),
            sender: { _ in throw IPCError.emptyResponse },
            fileWrite: { path, data in
                journalWrites += 1
                guard journalWrites < 2 else { return false }
                return FileManager.default.createFile(
                    atPath: path,
                    contents: data,
                    attributes: [.posixPermissions: 0o600]
                )
            }
        )
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: coordinator,
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainCreate: { _, _ in
                return .created
            }
        )
        controller.hostnameField.stringValue = "127.0.0.1"
        controller.portField.integerValue = 4096
        controller.usernameField.stringValue = "opencode"
        controller.credentialEditorState = .absent
        controller.securePasswordField.stringValue = "test-password"
        // Keep the "disable OpenCodeServerAgent" guard out of the way
        // regardless of the host machine's registration state.
        controller.openCodeServerAgentLoginButton.state = .on

        controller.save()
        // save() disables Save and shows "Saving…" synchronously;
        // finishSave re-enables the button as its first act on the main
        // queue, so that — not the intermediate text — is the terminal
        // signal for the asynchronous round trip.
        XCTAssertTrue(pumpMainRunLoop(until: { controller.saveButton.isEnabled }))

        XCTAssertEqual(
            controller.feedbackLabel.stringValue,
            CredentialMutationJournalError.writeFailed.localizedDescription
        )
        XCTAssertTrue(
            coordinator.hasUnacknowledgedMutation,
            "configuration and Keychain succeeded: the migration intent must remain retryable"
        )
        XCTAssertNil(coordinator.pendingGeneration)
        XCTAssertNotNil(controller.loadedConfig)
    }

    func testFailedCredentialRemovalDoesNotRenderItemAbsent() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-save-delete-fail-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let store = ConfigStore(paths: paths)
        let config = AppConfig(username: "old")
        try store.save(config)
        let coordinator = try makeCredentialMutationCoordinator(root: root)
        let failure = NSError(
            domain: "SettingsSaveFlowTest",
            code: 3,
            userInfo: [NSLocalizedDescriptionKey: "simulated Keychain removal failure"]
        )
        let controller = SettingsWindowController(
            configStore: store,
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: coordinator,
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainContains: { _ in true },
            keychainDelete: { _ in throw failure }
        )
        controller.hostnameField.stringValue = "127.0.0.1"
        controller.portField.integerValue = 4096
        controller.usernameField.stringValue = "old"
        controller.loadedAccount = "old"
        controller.loadedConfig = config
        controller.credentialEditorState = .removalPending
        controller.openCodeServerAgentLoginButton.state = .on

        controller.save()
        XCTAssertTrue(pumpMainRunLoop(until: { controller.saveButton.isEnabled }))
        XCTAssertTrue(controller.saveButton.isEnabled)
        if case .removalPending = controller.credentialEditorState {
            // expected: the item is still present and can be retried
        } else {
            XCTFail("a failed delete must remain retryable, not appear absent")
        }
        XCTAssertEqual(try store.load(), config)
        XCTAssertFalse(coordinator.hasUnacknowledgedMutation)
    }

    func testAbsentCredentialUsernameChangeUsesRegularCreate() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-save-absent-account-change-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let store = ConfigStore(paths: paths)
        let oldConfig = AppConfig(username: "old")
        try store.save(oldConfig)
        let coordinator = try makeCredentialMutationCoordinator(root: root)
        var createdAccount: String?
        let controller = SettingsWindowController(
            configStore: store,
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: coordinator,
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainCreate: { account, _ in
                createdAccount = account
                return .created
            }
        )
        controller.hostnameField.stringValue = oldConfig.hostname
        controller.portField.integerValue = oldConfig.port
        controller.usernameField.stringValue = "new"
        controller.loadedAccount = "old"
        controller.loadedConfig = oldConfig
        controller.credentialEditorState = .absent
        controller.securePasswordField.stringValue = "new-password"
        controller.openCodeServerAgentLoginButton.state = .on

        controller.save()
        XCTAssertTrue(pumpMainRunLoop(until: { controller.saveButton.isEnabled }))

        XCTAssertEqual(createdAccount, "new")
        XCTAssertEqual(try store.load().username, "new")
        XCTAssertNil(coordinator.migration)
        XCTAssertEqual(coordinator.pendingAccount, "new")
    }

    func testRemovalPendingCannotChangeUsername() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-save-delete-account-change-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let store = ConfigStore(paths: paths)
        let config = AppConfig(username: "old")
        try store.save(config)
        let coordinator = try makeCredentialMutationCoordinator(root: root)
        var deleteCalled = false
        let controller = SettingsWindowController(
            configStore: store,
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: coordinator,
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainContains: { _ in true },
            keychainDelete: { _ in deleteCalled = true }
        )
        controller.hostnameField.stringValue = config.hostname
        controller.portField.integerValue = config.port
        controller.usernameField.stringValue = "new"
        controller.loadedAccount = "old"
        controller.loadedConfig = config
        controller.credentialEditorState = .removalPending
        controller.save()

        XCTAssertFalse(deleteCalled)
        XCTAssertEqual(try store.load(), config)
        XCTAssertTrue(controller.feedbackLabel.stringValue.contains("Undo password removal"))
    }

    func testSaveFailureAfterKeychainSuccessKeepsMigrationPendingUntilConfigurationCanBeChecked() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-save-config-fail-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        // The credential journal lives in `root` and stays healthy; the
        // Configuration support directory is a plain file, so the plist write
        // fails after the new Keychain item is created. The current account
        // cannot be established, so cleanup remains a durable migration
        // intent instead of guessing which account is active.
        let brokenSupport = root.appending(path: "config-home", directoryHint: .isDirectory)
        guard FileManager.default.createFile(atPath: brokenSupport.path, contents: Data()) else {
            throw POSIXError(.EIO)
        }
        let paths = ApplicationPaths(
            supportDirectory: brokenSupport,
            configFile: brokenSupport.appending(path: "config.plist"),
            controlSocket: brokenSupport.appending(path: "run/control.sock")
        )
        let coordinator = try makeCredentialMutationCoordinator(root: root)
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: coordinator,
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainCreate: { _, _ in .created }
        )
        controller.hostnameField.stringValue = "127.0.0.1"
        controller.portField.integerValue = 4096
        controller.usernameField.stringValue = "opencode"
        controller.loadedAccount = "old"
        controller.credentialEditorState = .editingExisting(original: "old-password")
        controller.securePasswordField.stringValue = "test-password"
        // Keep the "disable OpenCodeServerAgent" guard out of the way
        // regardless of the host machine's registration state.
        controller.openCodeServerAgentLoginButton.state = .on

        controller.save()
        // save() disables Save and shows "Saving…" synchronously;
        // finishSave re-enables the button as its first act on the main
        // queue, so that — not the intermediate text — is the terminal
        // signal for the asynchronous round trip.
        XCTAssertTrue(pumpMainRunLoop(until: { controller.saveButton.isEnabled }))

        XCTAssertNotNil(coordinator.migration)
        XCTAssertNil(coordinator.pendingGeneration)
        XCTAssertNil(controller.loadedConfig)
        XCTAssertTrue(controller.saveButton.isEnabled)
    }

    func testDestructiveConfirmationsDefaultToCancel() {
        let alert = AppDelegate.makeDestructiveConfirmationAlert(
            style: .critical,
            title: "Force Stop OpenCode?",
            detail: "detail",
            actionTitle: "Force Stop"
        )
        XCTAssertEqual(alert.buttons.map(\.title), ["Force Stop", "Cancel"])
        XCTAssertEqual(alert.buttons[0].keyEquivalent, "")
        XCTAssertEqual(
            alert.buttons[1].keyEquivalent,
            "\r",
            "HIG: Cancel is the Return-key default for a destructive action"
        )
    }

    func testStatusItemSymbolsAreDistinctPerStateAndAvailable() {
        // Color alone must not communicate status: each health color pairs
        // with its own SF Symbol shape.
        let colors: [StatusColor] = [.green, .yellow, .red, .gray]
        let names = colors.map { AppDelegate.statusSymbolName(for: $0) }
        XCTAssertEqual(Set(names).count, colors.count)
        for name in names {
            XCTAssertNotNil(
                NSImage(systemSymbolName: name, accessibilityDescription: nil),
                "\(name) must resolve on the macOS 26 deployment target"
            )
        }
    }

    func testPolledStatusIsDiscardedOnceNewerPushArrived() {
        let requestedAt = Date()
        XCTAssertTrue(
            AppDelegate.shouldApplyPolledStatus(
                requestedAt: requestedAt,
                latestPushReceivedAt: .distantPast
            ),
            "before any push the one-shot response is the only status source"
        )
        XCTAssertTrue(
            AppDelegate.shouldApplyPolledStatus(
                requestedAt: requestedAt,
                latestPushReceivedAt: requestedAt
            ),
            "a push not newer than the request does not discard it"
        )
        XCTAssertFalse(
            AppDelegate.shouldApplyPolledStatus(
                requestedAt: requestedAt,
                latestPushReceivedAt: requestedAt.addingTimeInterval(0.001)
            ),
            "a push newer than the request already rendered newer agent state"
        )
    }

    func testSettingsButtonsCarryVoiceOverLabels() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-settings-a11y-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: try makeCredentialMutationCoordinator(root: root),
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {}
        )
        let contentView = try XCTUnwrap(controller.window?.contentView)
        let buttons = descendants(of: contentView).compactMap { $0 as? NSButton }
        XCTAssertNotNil(buttons.first { $0.accessibilityLabel() == "Show password" })
        XCTAssertNotNil(buttons.first { $0.accessibilityLabel() == "Choose OpenCode executable" })
    }

    func testBareIPv6LoopbackEndpointRaisesNoAuthenticationWarning() {
        XCTAssertFalse(
            AppDelegate.statusRowVisibility(
                status: makeStatus(
                    endpoint: "0:0:0:0:0:0:0:1:4096",
                    authenticationEnabled: false
                ),
                openCodeServerAgentIsNominal: true
            ).authentication,
            "a full-form IPv6 loopback listener is still loopback"
        )
    }

    func testSettingsWindowCentersOnlyOnFirstPresentation() throws {
        let root = URL(
            filePath: "/private/tmp/ocs-settings-center-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = ApplicationPaths(
            supportDirectory: root,
            configFile: root.appending(path: "config.plist"),
            controlSocket: root.appending(path: "run/control.sock")
        )
        let controller = SettingsWindowController(
            configStore: ConfigStore(paths: paths),
            services: ServiceController(),
            statusProvider: { nil },
            credentialMutations: try makeCredentialMutationCoordinator(root: root),
            credentialAuthorizationPerformer: {},
            restartPerformer: {},
            authorizeAndRestartPerformer: {},
            didSave: {},
            keychainContains: { _ in false },
            keychainLoad: { _ in nil }
        )
        defer { controller.window?.orderOut(nil) }

        controller.present()
        // Move the window off-center; a re-present must keep the user's
        // position instead of jumping back to the center of the screen.
        let movedOrigin = NSPoint(x: 120, y: 120)
        controller.window?.setFrameOrigin(movedOrigin)
        controller.present()
        XCTAssertEqual(controller.window?.frame.origin, movedOrigin)
    }

    func testStatusRowVisibilityFollowsProgressiveDisclosure() {
        // An unreachable agent is unknown, not "all is well": every row
        // stays visible so the user can see the stale values for what they
        // are.
        let unknown = AppDelegate.statusRowVisibility(
            status: nil,
            openCodeServerAgentIsNominal: false
        )
        XCTAssertTrue(unknown.openCodeServerAgent)
        XCTAssertTrue(unknown.fda)
        XCTAssertTrue(unknown.password)
        XCTAssertTrue(unknown.authentication)
        XCTAssertTrue(unknown.configuration)

        // A healthy steady state hides every conditional row — the menu
        // shows health, uptime, endpoint, and version only.
        let healthy = AppDelegate.statusRowVisibility(
            status: makeStatus(),
            openCodeServerAgentIsNominal: true
        )
        XCTAssertFalse(healthy.openCodeServerAgent)
        XCTAssertFalse(healthy.fda)
        XCTAssertFalse(healthy.password)
        XCTAssertFalse(healthy.authentication)
        XCTAssertFalse(healthy.configuration)

        XCTAssertTrue(
            AppDelegate.statusRowVisibility(
                status: makeStatus(),
                openCodeServerAgentIsNominal: false
            ).openCodeServerAgent,
            "a non-nominal registration state must stay visible"
        )
        XCTAssertTrue(
            AppDelegate.statusRowVisibility(
                status: makeStatus(fda: .notVerified),
                openCodeServerAgentIsNominal: true
            ).fda,
            "an unverified FDA probe must stay visible"
        )
        XCTAssertTrue(
            AppDelegate.statusRowVisibility(
                status: makeStatus(passwordState: .accessPending),
                openCodeServerAgentIsNominal: true
            ).password,
            "a pending Keychain authorization must stay visible"
        )
        XCTAssertTrue(
            AppDelegate.statusRowVisibility(
                status: makeStatus(
                    endpoint: "10.0.0.254:4096",
                    authenticationEnabled: false
                ),
                openCodeServerAgentIsNominal: true
            ).authentication,
            "an unauthenticated network listener is a warning"
        )
        XCTAssertFalse(
            AppDelegate.statusRowVisibility(
                status: makeStatus(authenticationEnabled: false),
                openCodeServerAgentIsNominal: true
            ).authentication,
            "an unauthenticated IPv4 loopback listener is the documented default"
        )
        XCTAssertFalse(
            AppDelegate.statusRowVisibility(
                status: makeStatus(
                    endpoint: "[::1]:4096",
                    authenticationEnabled: false
                ),
                openCodeServerAgentIsNominal: true
            ).authentication,
            "an unauthenticated IPv6 loopback listener is the documented default"
        )
        XCTAssertFalse(
            AppDelegate.statusRowVisibility(
                status: makeStatus(
                    endpoint: "localhost:4096",
                    authenticationEnabled: false
                ),
                openCodeServerAgentIsNominal: true
            ).authentication,
            "an unauthenticated localhost listener is the documented default"
        )
        XCTAssertTrue(
            AppDelegate.statusRowVisibility(
                status: makeStatus(configPending: true),
                openCodeServerAgentIsNominal: true
            ).configuration,
            "a saved-but-not-applied configuration must stay visible"
        )
        XCTAssertTrue(
            AppDelegate.statusRowVisibility(
                status: makeStatus(configError: "boom"),
                openCodeServerAgentIsNominal: true
            ).configuration,
            "a configuration error must stay visible"
        )
    }

    func testRestartMenuItemWaitsForConfiguredCredentialAccess() {
        XCTAssertFalse(
            AppDelegate.restartItemIsEnabled(
                status: makeStatus(
                    passwordState: .accessPending,
                    actionCapabilities: ActionCapabilities(
                        start: false,
                        stop: true,
                        restart: false,
                        continueStop: false,
                        forceStop: false
                    )
                ),
                credentialNoticeAcknowledged: true
            ),
            "Restart must not stop a healthy OpenCode before Keychain authorization converges"
        )
        XCTAssertTrue(
            AppDelegate.restartItemIsEnabled(
                status: makeStatus(
                    passwordState: .configured,
                    actionCapabilities: ActionCapabilities(
                        start: true,
                        stop: true,
                        restart: true,
                        continueStop: false,
                        forceStop: false
                    )
                ),
                credentialNoticeAcknowledged: true
            ),
            "the Allow & Restart promise must regain Restart after authorization succeeds"
        )

        // Preserve the pre-existing mutation-acknowledgement and lifecycle
        // gates while adding the access-pending guard.
        XCTAssertFalse(
            AppDelegate.restartItemIsEnabled(
                status: makeStatus(passwordState: .configured),
                credentialNoticeAcknowledged: false
            )
        )
        XCTAssertFalse(
            AppDelegate.restartItemIsEnabled(
                status: makeStatus(
                    serverState: .stopping,
                    actionCapabilities: ActionCapabilities(
                        start: false,
                        stop: true,
                        restart: false,
                        continueStop: false,
                        forceStop: false
                    )
                ),
                credentialNoticeAcknowledged: true
            )
        )
        XCTAssertFalse(
            AppDelegate.restartItemIsEnabled(
                status: makeStatus(
                    serverState: .stopTimedOut,
                    actionCapabilities: ActionCapabilities(
                        start: false,
                        stop: true,
                        restart: false,
                        continueStop: true,
                        forceStop: true
                    )
                ),
                credentialNoticeAcknowledged: true
            )
        )
    }

    /// A healthy steady-state status; individual tests override the one
    /// field they exercise.
    private func makeStatus(
        endpoint: String = "127.0.0.1:4096",
        fda: FDAState = .verified,
        passwordState: PasswordState = .configured,
        authenticationEnabled: Bool = true,
        configPending: Bool = false,
        configError: String? = nil,
        serverState: ServerState = .healthy,
        actionCapabilities: ActionCapabilities = .unavailable
    ) -> AgentStatus {
        AgentStatus(
            protocolVersion: ipcProtocolVersion,
            agentVersion: "test",
            agentUptimeSeconds: 0,
            desiredState: .running,
            serverState: serverState,
            health: .healthy,
            fda: fda,
            uptimeSeconds: 60,
            endpoint: endpoint,
            username: "opencode",
            passwordState: passwordState,
            authenticationEnabled: authenticationEnabled,
            actionCapabilities: actionCapabilities,
            installedVersion: "1.0.0",
            runningVersion: "1.0.0",
            versionPending: false,
            configPending: configPending,
            configError: configError,
            lastError: nil,
            pid: 1234,
            stopGraceRemainingSeconds: nil,
            notification: nil,
            processStartedAtUnixSeconds: nil,
            bundleVersion: "1"
        )
    }

    private func makeCredentialMutationCoordinator(
        root: URL
    ) throws -> CredentialMutationCoordinator {
        try CredentialMutationCoordinator(
            fileURL: root.appending(path: "credential-notification.plist"),
            sender: { _ in throw IPCError.emptyResponse }
        )
    }

    func testOpenCodeServerTerminationDoesNotSignalOpenCodeServerAgentOrOpenCode() throws {
        let supportRoot = FileManager.default.temporaryDirectory.appending(
            path: "ocs-app-delegate-\(UUID().uuidString)",
            directoryHint: .isDirectory
        )
        setenv("OPENCODESERVER_SUPPORT_DIR", supportRoot.path, 1)
        defer {
            unsetenv("OPENCODESERVER_SUPPORT_DIR")
            try? FileManager.default.removeItem(at: supportRoot)
        }
        let openCodeServerAgent = try startSentinelProcess()
        let openCode = try startSentinelProcess()
        defer {
            terminateSentinelProcess(openCodeServerAgent)
            terminateSentinelProcess(openCode)
        }

        let delegate = AppDelegate()
        delegate.applicationWillTerminate(
            Notification(name: NSApplication.willTerminateNotification)
        )

        XCTAssertTrue(openCodeServerAgent.isRunning)
        XCTAssertTrue(openCode.isRunning)
        XCTAssertEqual(Darwin.kill(openCodeServerAgent.processIdentifier, 0), 0)
        XCTAssertEqual(Darwin.kill(openCode.processIdentifier, 0), 0)
    }

    private func submenu(_ title: String) throws -> NSMenu {
        let mainMenu = try XCTUnwrap(NSApp.mainMenu)
        return try XCTUnwrap(mainMenu.item(withTitle: title)?.submenu)
    }

    /// Pumps the main run loop until the condition holds, letting queued
    /// main-queue save-flow continuations run. Returns the final condition.
    private func pumpMainRunLoop(
        timeout: TimeInterval = 2,
        until condition: () -> Bool
    ) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func descendants(of view: NSView) -> [NSView] {
        view.subviews.flatMap { [$0] + descendants(of: $0) }
    }

    private func textFieldWidth(
        matching field: NSTextField,
        anticipatedText: String
    ) -> CGFloat {
        let sizer = NSTextField()
        sizer.font = field.font
        sizer.controlSize = field.controlSize
        sizer.bezelStyle = field.bezelStyle
        sizer.stringValue = anticipatedText
        return ceil(
            sizer.sizeThatFits(
                NSSize(
                    width: CGFloat.greatestFiniteMagnitude,
                    height: CGFloat.greatestFiniteMagnitude
                )
            ).width
        )
    }

    private func assertNoAmbiguousProductLayout(
        in content: NSView,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        // NSTextField creates private field-editor/rendering descendants
        // whose internal layout intentionally reports ambiguous while the
        // editor is inactive. Audit the controls and containers owned by the
        // product, not AppKit's private implementation hierarchy.
        let productViews = [content] + descendants(of: content).filter {
            $0 is NSTextField || $0 is NSButton || $0 is NSProgressIndicator
                || $0 is NSPopUpButton || $0 is NSStackView || $0 is NSGridView
        }
        for view in productViews where !view.isHidden {
            let detail: String
            if let stack = view as? NSStackView {
                let arranged = stack.arrangedSubviews.map {
                    "\(type(of: $0))(\($0.isHidden ? "hidden" : "visible"))"
                }.joined(separator: ",")
                detail = " frame=\(view.frame) arranged=[\(arranged)]"
            } else {
                detail = " frame=\(view.frame) label=\(view.accessibilityLabel() ?? "-")"
            }
            XCTAssertFalse(
                view.hasAmbiguousLayout,
                "visible product AppKit view has ambiguous layout: \(type(of: view))\(detail)",
                file: file,
                line: line
            )
        }
    }

    private func startSentinelProcess() throws -> Process {
        let process = Process()
        process.executableURL = URL(filePath: "/bin/sleep")
        process.arguments = ["30"]
        try process.run()
        return process
    }

    private func terminateSentinelProcess(_ process: Process) {
        guard process.isRunning else { return }
        process.terminate()
        process.waitUntilExit()
    }
}
