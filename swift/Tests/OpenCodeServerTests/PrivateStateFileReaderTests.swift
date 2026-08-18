@testable import OpenCodeServer
import Darwin
import Foundation
import XCTest

@MainActor
final class PrivateStateFileReaderTests: XCTestCase {
    func testMissingFileIsDistinctFromUnsafeFile() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let path = root.appending(path: "state")

        XCTAssertThrowsError(try PrivateStateFileReader.read(at: path, maxBytes: 64)) { error in
            XCTAssertEqual(error as? PrivateStateFileReadError, .notFound)
        }

        try write(Data("state".utf8), to: path)
        try FileManager.default.createSymbolicLink(
            at: root.appending(path: "link"),
            withDestinationURL: path
        )
        XCTAssertThrowsError(
            try PrivateStateFileReader.read(
                at: root.appending(path: "link"),
                maxBytes: 64
            )
        ) { error in
            XCTAssertEqual(error as? PrivateStateFileReadError, .symbolicLink)
        }
    }

    func testRegularCurrentUser0600FileReadsNormally() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let path = root.appending(path: "state")
        let expected = Data("private state".utf8)
        try write(expected, to: path)

        XCTAssertEqual(
            try PrivateStateFileReader.read(at: path, maxBytes: 64),
            expected
        )
    }

    func testDirectoryIsNotReadAsPrivateState() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let directory = root.appending(path: "directory", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: false
        )
        XCTAssertThrowsError(
            try PrivateStateFileReader.read(at: directory, maxBytes: 64)
        ) { error in
            XCTAssertEqual(error as? PrivateStateFileReadError, .notRegular)
        }
    }

    func testOversizedFileIsRejectedBeforeUnboundedRead() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let path = root.appending(path: "state")
        try write(Data(repeating: 0x41, count: 65), to: path)

        XCTAssertThrowsError(
            try PrivateStateFileReader.read(at: path, maxBytes: 64)
        ) { error in
            XCTAssertEqual(error as? PrivateStateFileReadError, .tooLarge(limit: 64))
        }
    }

    func testGroupReadableFileIsRejected() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let path = root.appending(path: "state")
        try write(Data("state".utf8), to: path)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o640],
            ofItemAtPath: path.path
        )

        XCTAssertThrowsError(
            try PrivateStateFileReader.read(at: path, maxBytes: 64)
        ) { error in
            XCTAssertEqual(error as? PrivateStateFileReadError, .insecurePermissions)
        }
    }

    func testConfigSaveDoesNotReplaceUnsafeExistingFile() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let target = root.appending(path: "target")
        let configPath = root.appending(path: "config.plist")
        try write(Data("untouched".utf8), to: target)
        try FileManager.default.createSymbolicLink(
            at: configPath,
            withDestinationURL: target
        )
        let store = ConfigStore(
            paths: ApplicationPaths(
                supportDirectory: root,
                configFile: configPath,
                controlSocket: root.appending(path: "run/control.sock")
            )
        )

        XCTAssertThrowsError(try store.save(AppConfig())) { error in
            guard case .privateFileReadFailed = error as? ConfigStoreError else {
                return XCTFail("expected a private-file safety error, got \(error)")
            }
        }
        XCTAssertEqual(
            try String(contentsOf: target, encoding: .utf8),
            "untouched"
        )
    }

    func testUnavailableJournalFailsClosedAndExplicitRetryRecovers() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let journalPath = root.appending(path: "credential-notification.plist")
        try write(Data("not a plist".utf8), to: journalPath)
        let unavailable = CredentialMutationCoordinator.unavailable(
            fileURL: journalPath,
            sender: { _ in throw IPCError.emptyResponse },
            error: CredentialMutationJournalError.unreadable
        )

        XCTAssertFalse(unavailable.availability.isAvailable)
        XCTAssertTrue(unavailable.hasUnacknowledgedMutation)
        XCTAssertThrowsError(try unavailable.stage(account: "opencode"))
        var deferredActionRan = false
        XCTAssertFalse(
            unavailable.performAfterAcknowledgement { deferredActionRan = true }
        )
        XCTAssertFalse(deferredActionRan)
        unavailable.retryAvailability()
        XCTAssertFalse(unavailable.availability.isAvailable)

        try? FileManager.default.removeItem(at: journalPath)
        let healthy = try CredentialMutationJournal(fileURL: journalPath)
        _ = try healthy.stage(account: "opencode")
        unavailable.retryAvailability()
        XCTAssertTrue(unavailable.availability.isAvailable)
        XCTAssertTrue(unavailable.hasUnacknowledgedMutation)
        XCTAssertEqual(unavailable.pendingAccount, "opencode")
    }

    func testJournalReaderRejectsSymlinkDirectoryOversizedAndInsecureMode() throws {
        do {
            let root = try makeRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let path = root.appending(path: "journal")
            try write(Data("valid".utf8), to: root.appending(path: "target"))
            try FileManager.default.createSymbolicLink(
                at: path,
                withDestinationURL: root.appending(path: "target")
            )
            XCTAssertThrowsError(try CredentialMutationJournal(fileURL: path))
        }
        do {
            let root = try makeRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let path = root.appending(path: "journal", directoryHint: .isDirectory)
            try FileManager.default.createDirectory(
                at: path,
                withIntermediateDirectories: false
            )
            XCTAssertThrowsError(try CredentialMutationJournal(fileURL: path))
        }
        do {
            let root = try makeRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let path = root.appending(path: "journal")
            try write(Data(repeating: 0x41, count: 16 * 1024 + 1), to: path)
            XCTAssertThrowsError(try CredentialMutationJournal(fileURL: path))
        }
        do {
            let root = try makeRoot()
            defer { try? FileManager.default.removeItem(at: root) }
            let path = root.appending(path: "journal")
            try write(Data("valid".utf8), to: path)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o640],
                ofItemAtPath: path.path
            )
            XCTAssertThrowsError(try CredentialMutationJournal(fileURL: path))
        }
    }

    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appending(
            path: "ocs-private-state-\(UUID().uuidString)",
            directoryHint: .isDirectory
        )
        try FileManager.default.createDirectory(
            at: root,
            withIntermediateDirectories: false
        )
        return root
    }

    private func write(_ data: Data, to path: URL) throws {
        guard FileManager.default.createFile(
            atPath: path.path,
            contents: data,
            attributes: [.posixPermissions: 0o600]
        ) else {
            throw POSIXError(.EIO)
        }
    }
}
