@testable import OpenCodeServer
import AppKit
import Foundation
import XCTest

@MainActor
final class SettingsCredentialSessionTests: XCTestCase {
    func testClosingSettingsClearsEditingCredentialState() throws {
        let (root, controller) = try makeController(
            keychainContains: { _ in true },
            keychainLoad: { _ in "editing-value" }
        )
        defer { cleanup(root, controller: controller) }

        controller.present()
        XCTAssertTrue(pumpMainRunLoop(until: { isStored(controller) }))
        controller.editPasswordButton.performClick(nil)
        XCTAssertTrue(pumpMainRunLoop(until: { isEditing(controller) }))
        XCTAssertEqual(controller.securePasswordField.stringValue, "editing-value")
        XCTAssertEqual(controller.plainPasswordField.stringValue, "editing-value")

        controller.close()

        assertCleared(controller)
        XCTAssertTrue(isStored(controller))
        XCTAssertNil(controller.loadedAccount)
    }

    func testClosingSettingsBeforeDecryptReturnsDiscardsLateEditResult() throws {
        let readStarted = expectation(description: "explicit decrypt started")
        let readFinished = expectation(description: "late decrypt finished")
        let releaseRead = DispatchSemaphore(value: 0)
        let (root, controller) = try makeController(
            keychainContains: { _ in true },
            keychainLoad: { _ in
                readStarted.fulfill()
                releaseRead.wait()
                readFinished.fulfill()
                return "late-edit-value"
            }
        )
        defer {
            releaseRead.signal()
            cleanup(root, controller: controller)
        }

        controller.present()
        XCTAssertTrue(pumpMainRunLoop(until: { isStored(controller) }))
        controller.editPasswordButton.performClick(nil)
        wait(for: [readStarted], timeout: 1)
        if case .loading = controller.credentialEditorState {
            // The worker is still blocked before returning the decrypted value.
        } else {
            XCTFail("Edit should show loading while decrypt is pending")
        }

        controller.close()
        assertCleared(controller)
        XCTAssertTrue(isStored(controller))
        releaseRead.signal()
        wait(for: [readFinished], timeout: 1)
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))

        assertCleared(controller)
        XCTAssertTrue(isStored(controller))
    }

    func testClosingSettingsBeforeCopyReturnsPreservesFeedbackAndSkipsCopySink() throws {
        let readStarted = expectation(description: "explicit copy read started")
        let readFinished = expectation(description: "late copy read finished")
        let releaseRead = DispatchSemaphore(value: 0)
        var copyValues = [String]()
        let (root, controller) = try makeController(
            keychainContains: { _ in true },
            keychainLoad: { _ in
                readStarted.fulfill()
                releaseRead.wait()
                readFinished.fulfill()
                return "late-copy-value"
            },
            copyPasswordToPasteboard: { copyValues.append($0) }
        )
        defer {
            releaseRead.signal()
            cleanup(root, controller: controller)
        }

        controller.present()
        XCTAssertTrue(pumpMainRunLoop(until: { isStored(controller) }))
        controller.updateFeedbackText("Keep this feedback")
        controller.copyPassword()
        wait(for: [readStarted], timeout: 1)

        controller.close()
        releaseRead.signal()
        wait(for: [readFinished], timeout: 1)
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))

        XCTAssertTrue(copyValues.isEmpty)
        XCTAssertEqual(controller.feedbackLabel.stringValue, "Keep this feedback")
        XCTAssertTrue(isStored(controller))
    }

    func testReopeningSettingsDoesNotDisplayPasswordFromPreviousPresentation() throws {
        let probeCount = LockedCounter()
        let decryptCount = LockedCounter()
        let (root, controller) = try makeController(
            keychainContains: { _ in
                probeCount.increment()
                return true
            },
            keychainLoad: { _ in
                decryptCount.increment()
                return "previous-presentation-value"
            }
        )
        defer { cleanup(root, controller: controller) }

        controller.present()
        XCTAssertTrue(pumpMainRunLoop(until: { isStored(controller) }))
        controller.editPasswordButton.performClick(nil)
        XCTAssertTrue(pumpMainRunLoop(until: { isEditing(controller) }))
        XCTAssertEqual(controller.securePasswordField.stringValue, "previous-presentation-value")
        XCTAssertEqual(decryptCount.value, 1)

        controller.close()
        controller.present()
        XCTAssertTrue(pumpMainRunLoop(until: { isStored(controller) }))

        XCTAssertEqual(probeCount.value, 2)
        XCTAssertEqual(decryptCount.value, 1)
        XCTAssertEqual(controller.securePasswordField.stringValue, "")
        XCTAssertEqual(controller.plainPasswordField.stringValue, "")
    }

    private func makeController(
        keychainContains: @escaping @Sendable (String) throws -> Bool,
        keychainLoad: @escaping @Sendable (String) throws -> String?,
        copyPasswordToPasteboard: @escaping (String) -> Void = { _ in }
    ) throws -> (URL, SettingsWindowController) {
        let root = URL(
            filePath: "/private/tmp/ocs-settings-credential-session-\(UUID().uuidString.prefix(8))",
            directoryHint: .isDirectory
        )
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
            keychainContains: keychainContains,
            keychainLoad: keychainLoad,
            copyPasswordToPasteboard: copyPasswordToPasteboard
        )
        return (root, controller)
    }

    private func makeCredentialMutationCoordinator(root: URL) throws -> CredentialMutationCoordinator {
        try CredentialMutationCoordinator(
            fileURL: root.appending(path: "credential-notification.plist"),
            sender: { _ in throw IPCError.emptyResponse }
        )
    }

    private func isStored(_ controller: SettingsWindowController) -> Bool {
        if case .stored = controller.credentialEditorState { return true }
        return false
    }

    private func isEditing(_ controller: SettingsWindowController) -> Bool {
        if case .editingExisting = controller.credentialEditorState { return true }
        return false
    }

    private func assertCleared(
        _ controller: SettingsWindowController,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertEqual(controller.securePasswordField.stringValue, "", file: file, line: line)
        XCTAssertEqual(controller.plainPasswordField.stringValue, "", file: file, line: line)
        XCTAssertEqual(controller.showPasswordButton.state, .off, file: file, line: line)
    }

    private func cleanup(_ root: URL, controller: SettingsWindowController) {
        controller.window?.close()
        try? FileManager.default.removeItem(at: root)
    }

    private func pumpMainRunLoop(timeout: TimeInterval = 2, until condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private final class LockedCounter: @unchecked Sendable {
        private let lock = NSLock()
        private var storage = 0

        var value: Int {
            lock.lock()
            defer { lock.unlock() }
            return storage
        }

        func increment() {
            lock.lock()
            storage += 1
            lock.unlock()
        }
    }
}
