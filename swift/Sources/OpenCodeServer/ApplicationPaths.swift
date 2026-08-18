import Foundation

enum ApplicationPathsError: LocalizedError {
    case supportDirectoryOverrideMustBeAbsolute

    var errorDescription: String? {
        "OPENCODESERVER_SUPPORT_DIR must be an absolute path."
    }
}

struct ApplicationPaths: Sendable {
    let supportDirectory: URL
    let configFile: URL
    let controlSocket: URL

    var credentialMutationFile: URL {
        supportDirectory.appending(path: "credential-notification.plist")
    }

    static func discover() throws -> ApplicationPaths {
        let supportRoot = try resolveSupportDirectory(
            override: ProcessInfo.processInfo.environment["OPENCODESERVER_SUPPORT_DIR"],
            applicationSupportDirectory: .applicationSupportDirectory
        )
        return ApplicationPaths(
            supportDirectory: supportRoot,
            configFile: supportRoot.appending(path: "config.plist"),
            controlSocket: supportRoot
                .appending(path: "run", directoryHint: .isDirectory)
                .appending(path: "control.sock")
        )
    }

    static func resolveSupportDirectory(
        override: String?,
        applicationSupportDirectory: URL
    ) throws -> URL {
        guard let override,
              !override.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            return applicationSupportDirectory
                .appending(path: "OpenCodeServer", directoryHint: .isDirectory)
        }
        guard (override as NSString).isAbsolutePath else {
            throw ApplicationPathsError.supportDirectoryOverrideMustBeAbsolute
        }
        return URL(filePath: override, directoryHint: .isDirectory)
    }
}
