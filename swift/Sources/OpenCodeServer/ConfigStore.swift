import Darwin
import Foundation

struct AppConfig: Codable, Equatable {
    var schemaVersion: Int = 1
    var hostname: String = "127.0.0.1"
    var port: Int = 4096
    var username: String = "opencode"
    var mdns = false
    var executablePath: String = ""

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "SchemaVersion"
        case hostname = "Hostname"
        case port = "Port"
        case username = "Username"
        case mdns = "MDNS"
        case executablePath = "ExecutablePath"
    }
}

enum ConfigStoreError: LocalizedError {
    case invalid([String])
    case privateFileReadFailed(String)
    case writeFailed(String)

    var errorDescription: String? {
        switch self {
        case let .invalid(issues):
            return issues.joined(separator: "\n")
        case let .privateFileReadFailed(message):
            return "The configuration could not be read safely: \(message)"
        case let .writeFailed(message):
            return message
        }
    }
}

final class ConfigStore {
    static let maximumPrivateFileBytes = 64 * 1024

    private let paths: ApplicationPaths
    private let fileManager = FileManager.default

    init(paths: ApplicationPaths) {
        self.paths = paths
    }

    var applicationPaths: ApplicationPaths { paths }

    func ensureDefault() throws {
        do {
            _ = try PrivateStateFileReader.read(
                at: paths.configFile,
                maxBytes: Self.maximumPrivateFileBytes
            )
        } catch PrivateStateFileReadError.notFound {
            try save(AppConfig())
        }
    }

    func load() throws -> AppConfig {
        do {
            try ensureDefault()
        } catch let error as ConfigStoreError {
            throw error
        } catch {
            throw ConfigStoreError.privateFileReadFailed(error.localizedDescription)
        }
        let data: Data
        do {
            data = try PrivateStateFileReader.read(
                at: paths.configFile,
                maxBytes: Self.maximumPrivateFileBytes
            )
        } catch {
            throw ConfigStoreError.privateFileReadFailed(error.localizedDescription)
        }
        return try PropertyListDecoder().decode(AppConfig.self, from: data)
    }

    func save(_ config: AppConfig) throws {
        let issues = Self.validationIssues(config)
        guard issues.isEmpty else {
            throw ConfigStoreError.invalid(issues)
        }
        try fileManager.createDirectory(
            at: paths.supportDirectory,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        try fileManager.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: paths.supportDirectory.path
        )
        let existingData: Data?
        do {
            existingData = try PrivateStateFileReader.read(
                at: paths.configFile,
                maxBytes: Self.maximumPrivateFileBytes
            )
        } catch PrivateStateFileReadError.notFound {
            existingData = nil
        } catch {
            // An unsafe existing file is never silently replaced by Save. A
            // user or an explicit repair flow must first resolve its state.
            throw ConfigStoreError.privateFileReadFailed(error.localizedDescription)
        }
        if let existingData,
           let existing = try? PropertyListDecoder().decode(AppConfig.self, from: existingData),
           existing == config {
            return
        }

        let encoder = PropertyListEncoder()
        encoder.outputFormat = .xml
        let data = try encoder.encode(config)
        let temporary = paths.supportDirectory
            .appending(path: ".config.\(UUID().uuidString).tmp")
        guard fileManager.createFile(
            atPath: temporary.path,
            contents: data,
            attributes: [.posixPermissions: 0o600]
        ) else {
            throw ConfigStoreError.writeFailed("Unable to create a private temporary configuration file.")
        }
        do {
            let handle = try FileHandle(forWritingTo: temporary)
            try handle.synchronize()
            try handle.close()
            if Darwin.rename(temporary.path, paths.configFile.path) != 0 {
                throw ConfigStoreError.writeFailed(
                    String(cString: strerror(errno))
                )
            }
            try fileManager.setAttributes(
                [.posixPermissions: 0o600],
                ofItemAtPath: paths.configFile.path
            )
            // Flush the containing directory too: the file fsync makes the
            // contents durable, but only the directory flush preserves the
            // rename itself across a crash.
            let directoryDescriptor = Darwin.open(paths.supportDirectory.path, O_RDONLY)
            guard directoryDescriptor >= 0 else {
                throw ConfigStoreError.writeFailed(String(cString: strerror(errno)))
            }
            let directorySyncResult = Darwin.fsync(directoryDescriptor)
            let directorySyncError = errno
            Darwin.close(directoryDescriptor)
            guard directorySyncResult == 0 else {
                throw ConfigStoreError.writeFailed(String(cString: strerror(directorySyncError)))
            }
        } catch {
            try? fileManager.removeItem(at: temporary)
            throw error
        }
    }

    static func validationIssues(_ config: AppConfig) -> [String] {
        var issues: [String] = []
        if config.schemaVersion != 1 {
            issues.append("Unsupported configuration schema.")
        }
        let hostname = config.hostname
        let hostnameHasInvalidCharacter =
            hostname.contains(where: { $0.isWhitespace || $0.isNewline }) ||
            hostname.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) ||
            hostname.contains("/")
        if hostname.isEmpty || hostname.count > 253 || hostnameHasInvalidCharacter ||
            !isIPOrDNSName(hostname)
        {
            issues.append("Enter a valid host name or IP address.")
        }
        if !(1 ... 65_535).contains(config.port) {
            issues.append("Port must be between 1 and 65535.")
        }
        if config.username.utf8.count > 128 ||
            config.username.contains(":") ||
            config.username.contains(where: { $0.isNewline }) ||
            config.username.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
        {
            issues.append(
                "Username must not contain a colon, newline, or control character and must be at most 128 characters."
            )
        }
        if !config.executablePath.isEmpty && !config.executablePath.hasPrefix("/") {
            issues.append("The OpenCode executable path must be absolute.")
        }
        return issues
    }

    /// Mirrors OpenCodeServerAgent's hostname rule (rust `validate_hostname`)
    /// so a Save cannot be rejected agent-side afterwards: a bracketed or
    /// bare IPv4/IPv6 literal, "localhost", or a DNS name whose dot-separated
    /// labels are 1–63 ASCII alphanumerics/hyphens and do not begin or end
    /// with a hyphen.
    private static func isIPOrDNSName(_ hostname: String) -> Bool {
        let unbracketed: Substring
        if hostname.hasPrefix("["), hostname.hasSuffix("]") {
            unbracketed = hostname.dropFirst().dropLast()
        } else {
            unbracketed = Substring(hostname)
        }
        if hostname == "localhost" || isIPLiteral(String(unbracketed)) {
            return true
        }
        return hostname.split(separator: ".", omittingEmptySubsequences: false)
            .allSatisfy { label in
                !label.isEmpty && label.utf8.count <= 63 &&
                    !label.hasPrefix("-") && !label.hasSuffix("-") &&
                    label.allSatisfy { character in
                        character.isASCII &&
                            (character.isLetter || character.isNumber || character == "-")
                    }
            }
    }

    private static func isIPLiteral(_ value: String) -> Bool {
        var ipv4Address = in_addr()
        if value.withCString({ Darwin.inet_pton(AF_INET, $0, &ipv4Address) }) == 1 {
            return true
        }
        // inet_pton also accepts a %zone suffix that the agent's IPv6 parser
        // does not; a listen address with a zone ID would be rejected
        // agent-side, so it is rejected here first. (Zero-padded IPv4
        // octets need no such guard: they fail the agent's IP parse but
        // pass its DNS-label fallback, exactly as in isIPOrDNSName.)
        guard !value.contains("%") else { return false }
        var ipv6Address = in6_addr()
        return value.withCString { Darwin.inet_pton(AF_INET6, $0, &ipv6Address) } == 1
    }

    static func discoverExecutableCandidates() -> [String] {
        var candidates = ["/opt/homebrew/bin/opencode"]
        candidates.append(
            FileManager.default.homeDirectoryForCurrentUser
                .appending(path: ".opencode/bin/opencode").path
        )
        if let path = ProcessInfo.processInfo.environment["PATH"] {
            candidates.append(
                contentsOf: path.split(separator: ":").map {
                    URL(filePath: String($0), directoryHint: .isDirectory)
                        .appending(path: "opencode").path
                }
            )
        }
        var seen = Set<String>()
        return candidates.filter { candidate in
            guard seen.insert(candidate).inserted else { return false }
            guard FileManager.default.isExecutableFile(atPath: candidate) else {
                return false
            }
            // Match OpenCodeServerAgent's discovery (rust
            // `discover_executables`): resolve symlinks before reading the
            // header so both components classify the same canonical target.
            let resolved = URL(filePath: candidate).resolvingSymlinksInPath()
            do {
                let handle = try FileHandle(forReadingFrom: resolved)
                defer { try? handle.close() }
                guard let header = try handle.read(upToCount: 4_096) else {
                    return false
                }
                return isArm64MachO(header)
            } catch {
                return false
            }
        }
    }

    private static func isArm64MachO(_ data: Data) -> Bool {
        let bytes = [UInt8](data)
        guard bytes.count >= 8 else { return false }

        let readUInt32: (Int, Bool) -> UInt32 = { offset, bigEndian in
            if bigEndian {
                return UInt32(bytes[offset]) << 24 |
                    UInt32(bytes[offset + 1]) << 16 |
                    UInt32(bytes[offset + 2]) << 8 |
                    UInt32(bytes[offset + 3])
            }
            return UInt32(bytes[offset]) |
                UInt32(bytes[offset + 1]) << 8 |
                UInt32(bytes[offset + 2]) << 16 |
                UInt32(bytes[offset + 3]) << 24
        }

        let cpuTypeArm64: UInt32 = 0x0100_000c
        let littleEndianMagic = readUInt32(0, false)
        if littleEndianMagic == 0xfeed_face ||
            littleEndianMagic == 0xfeed_facf
        {
            return readUInt32(4, false) == cpuTypeArm64
        }

        let fatFormat: (is64Bit: Bool, isBigEndian: Bool)
        switch readUInt32(0, true) {
        case 0xcafe_babe:
            fatFormat = (false, true)
        case 0xcafe_babf:
            fatFormat = (true, true)
        case 0xbeba_feca:
            fatFormat = (false, false)
        case 0xbfba_feca:
            fatFormat = (true, false)
        default:
            return false
        }

        let count = Int(readUInt32(4, fatFormat.isBigEndian))
        guard count <= 64 else { return false }
        let entrySize = fatFormat.is64Bit ? 32 : 20
        guard bytes.count >= 8 + count * entrySize else { return false }
        for index in 0 ..< count {
            let offset = 8 + index * entrySize
            if readUInt32(offset, fatFormat.isBigEndian) == cpuTypeArm64 {
                return true
            }
        }
        return false
    }

    #if DEBUG
        static func isArm64MachOForTesting(_ data: Data) -> Bool {
            isArm64MachO(data)
        }
    #endif

    /// Semantic loopback check matching OpenCodeServerAgent's
    /// `IpAddr::is_loopback`: any 127.0.0.0/8 IPv4 address or exactly ::1,
    /// bracketed or bare, plus the "localhost" name. Plain string equality
    /// missed non-canonical forms such as 0:0:0:0:0:0:0:1.
    static func isLoopback(_ hostname: String) -> Bool {
        let unbracketed: String
        if hostname.hasPrefix("["), hostname.hasSuffix("]") {
            unbracketed = String(hostname.dropFirst().dropLast())
        } else {
            unbracketed = hostname
        }
        if unbracketed == "localhost" {
            return true
        }
        var ipv4Address = in_addr()
        if unbracketed.withCString({ Darwin.inet_pton(AF_INET, $0, &ipv4Address) }) == 1 {
            // Network byte order: the first in-memory octet decides
            // 127.0.0.0/8 regardless of host endianness.
            return withUnsafeBytes(of: &ipv4Address) { $0[0] == 127 }
        }
        var ipv6Address = in6_addr()
        guard unbracketed.withCString({ Darwin.inet_pton(AF_INET6, $0, &ipv6Address) }) == 1 else {
            return false
        }
        return withUnsafeBytes(of: &ipv6Address) { bytes in
            bytes.prefix(15).allSatisfy { $0 == 0 } && bytes[15] == 1
        }
    }

}
