import Darwin
import Foundation

enum IPCError: LocalizedError {
    case pathTooLong
    case systemCall(String)
    case responseTooLarge
    case emptyResponse
    case invalidFraming
    case agent(String)
    case missingStatus
    case protocolMismatch(Int)

    var errorDescription: String? {
        switch self {
        case .pathTooLong:
            return "The control socket path is too long."
        case let .systemCall(message):
            return message
        case .responseTooLarge:
            return "The OpenCodeServerAgent response exceeded 64 KiB."
        case .emptyResponse:
            return "OpenCodeServerAgent closed the connection without a response."
        case .invalidFraming:
            return "OpenCodeServerAgent returned an unterminated IPC response."
        case let .agent(message):
            return message
        case .missingStatus:
            return "OpenCodeServerAgent returned no status."
        case let .protocolMismatch(version):
            return "OpenCodeServerAgent uses IPC protocol \(version); expected \(ipcProtocolVersion)."
        }
    }
}

struct IPCClient: Sendable {
    private let socketPath: String
    private let timeoutMilliseconds: Int

    init(paths: ApplicationPaths) {
        socketPath = paths.controlSocket.path
        timeoutMilliseconds = 3_000
    }

    init(socketPath: String, timeoutMilliseconds: Int = 3_000) {
        self.socketPath = socketPath
        self.timeoutMilliseconds = timeoutMilliseconds
    }

    func send(_ command: AgentCommand) throws -> AgentResponse {
        try send(command, afterConnecting: nil)
    }

    func send(
        _ command: AgentCommand,
        afterConnecting: (() -> Void)?
    ) throws -> AgentResponse {
        let descriptor = try Self.openConnection(
            socketPath: socketPath,
            timeoutMilliseconds: timeoutMilliseconds
        )
        defer { Darwin.close(descriptor) }
        afterConnecting?()

        let encoder = JSONEncoder()
        var request = try encoder.encode(AgentRequest(command: command))
        request.append(0x0A)
        try Self.writeAll(request, to: descriptor)

        var response = Data()
        var buffer = [UInt8](repeating: 0, count: 4_096)
        while response.count <= 65_536 {
            let count = buffer.withUnsafeMutableBytes { rawBuffer in
                Darwin.read(descriptor, rawBuffer.baseAddress, rawBuffer.count)
            }
            if count < 0 {
                if errno == EINTR { continue }
                throw Self.systemError("Unable to read the OpenCodeServerAgent response")
            }
            if count == 0 { break }
            response.append(contentsOf: buffer.prefix(count))
            if response.last == 0x0A { break }
        }
        guard response.count <= 65_536 else {
            throw IPCError.responseTooLarge
        }
        guard !response.isEmpty else {
            throw IPCError.emptyResponse
        }
        guard response.last == 0x0A else {
            throw IPCError.invalidFraming
        }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let decoded = try decoder.decode(AgentResponse.self, from: response)
        _ = try Self.requireCurrentStatus(decoded)
        if !decoded.ok, let error = decoded.error {
            throw IPCError.agent(error)
        }
        return decoded
    }

    /// Creates and connects a control socket with the socket-scoped SIGPIPE
    /// protection required by ADR 0007. Shared by one-shot requests and the
    /// status subscription. The caller owns the returned descriptor.
    static func openConnection(
        socketPath: String,
        timeoutMilliseconds: Int
    ) throws -> Int32 {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw systemError("Unable to create the control socket")
        }
        do {
            var noSigPipe: Int32 = 1
            let noSigPipeResult = withUnsafePointer(to: &noSigPipe) { pointer in
                Darwin.setsockopt(
                    descriptor,
                    SOL_SOCKET,
                    SO_NOSIGPIPE,
                    pointer,
                    socklen_t(MemoryLayout<Int32>.size)
                )
            }
            guard noSigPipeResult == 0 else {
                throw systemError("Unable to configure the control socket")
            }

            let seconds = timeoutMilliseconds / 1_000
            let microseconds = (timeoutMilliseconds % 1_000) * 1_000
            var timeout = timeval(tv_sec: seconds, tv_usec: Int32(microseconds))
            let receiveTimeoutResult = withUnsafePointer(to: &timeout) { pointer in
                Darwin.setsockopt(
                    descriptor,
                    SOL_SOCKET,
                    SO_RCVTIMEO,
                    pointer,
                    socklen_t(MemoryLayout<timeval>.size)
                )
            }
            guard receiveTimeoutResult == 0 else {
                throw systemError("Unable to configure the control socket receive timeout")
            }
            let sendTimeoutResult = withUnsafePointer(to: &timeout) { pointer in
                Darwin.setsockopt(
                    descriptor,
                    SOL_SOCKET,
                    SO_SNDTIMEO,
                    pointer,
                    socklen_t(MemoryLayout<timeval>.size)
                )
            }
            guard sendTimeoutResult == 0 else {
                throw systemError("Unable to configure the control socket send timeout")
            }

            // A blocking connect() has no timeout: a listen backlog that
            // never drains would park the caller past every other budget.
            // Connect non-blocking, await writability within the request
            // timeout, then restore blocking mode for the request/response
            // exchange so the socket timeouts above keep governing it.
            let originalFlags = Darwin.fcntl(descriptor, F_GETFL)
            guard originalFlags >= 0,
                  Darwin.fcntl(descriptor, F_SETFL, originalFlags | O_NONBLOCK) == 0
            else {
                throw systemError("Unable to configure the control socket")
            }

            var address = sockaddr_un()
            address.sun_family = sa_family_t(AF_UNIX)
            let maximumPathLength = MemoryLayout.size(ofValue: address.sun_path)
            guard socketPath.utf8.count + 1 <= maximumPathLength else {
                throw IPCError.pathTooLong
            }
            withUnsafeMutablePointer(to: &address.sun_path) { pointer in
                pointer.withMemoryRebound(to: CChar.self, capacity: maximumPathLength) { buffer in
                    socketPath.withCString { source in
                        _ = strlcpy(buffer, source, maximumPathLength)
                    }
                }
            }
            let addressLength = socklen_t(
                MemoryLayout.size(ofValue: address.sun_len) +
                    MemoryLayout.size(ofValue: address.sun_family) +
                    socketPath.utf8.count + 1
            )
            address.sun_len = UInt8(addressLength)
            let connectResult = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.connect(descriptor, $0, addressLength)
                }
            }
            if connectResult != 0 {
                guard errno == EINPROGRESS else {
                    throw systemError("Unable to connect to OpenCodeServerAgent")
                }
                try awaitConnection(descriptor, timeoutMilliseconds: timeoutMilliseconds)
            }
            guard Darwin.fcntl(descriptor, F_SETFL, originalFlags) == 0 else {
                throw systemError("Unable to configure the control socket")
            }
            return descriptor
        } catch {
            Darwin.close(descriptor)
            throw error
        }
    }

    /// Waits for a non-blocking connect to settle within the timeout budget,
    /// then surfaces the pending socket error (if any) as the connect result.
    private static func awaitConnection(
        _ descriptor: Int32,
        timeoutMilliseconds: Int
    ) throws {
        let deadline = Date().addingTimeInterval(TimeInterval(timeoutMilliseconds) / 1_000)
        while true {
            var polled = pollfd(fd: descriptor, events: Int16(POLLOUT), revents: 0)
            let remaining = max(0, Int(deadline.timeIntervalSinceNow * 1_000))
            let result = Darwin.poll(&polled, 1, Int32(remaining))
            if result < 0 {
                if errno == EINTR { continue }
                throw systemError("Unable to await the OpenCodeServerAgent connection")
            }
            guard result > 0 else {
                throw IPCError.systemCall(
                    "Unable to connect to OpenCodeServerAgent: connection timed out"
                )
            }
            break
        }
        var pendingError: Int32 = 0
        var pendingErrorLength = socklen_t(MemoryLayout<Int32>.size)
        let pendingErrorResult = withUnsafeMutablePointer(to: &pendingError) { pointer in
            Darwin.getsockopt(descriptor, SOL_SOCKET, SO_ERROR, pointer, &pendingErrorLength)
        }
        guard pendingErrorResult == 0 else {
            throw systemError("Unable to connect to OpenCodeServerAgent")
        }
        guard pendingError == 0 else {
            throw IPCError.systemCall(
                "Unable to connect to OpenCodeServerAgent: \(String(cString: strerror(pendingError)))"
            )
        }
    }

    static func writeAll(_ data: Data, to descriptor: Int32) throws {
        try data.withUnsafeBytes { rawBuffer in
            guard let base = rawBuffer.baseAddress else { return }
            var offset = 0
            while offset < rawBuffer.count {
                let count = Darwin.write(
                    descriptor,
                    base.advanced(by: offset),
                    rawBuffer.count - offset
                )
                if count < 0 {
                    if errno == EINTR { continue }
                    throw systemError(
                        "Unable to write the OpenCodeServerAgent request"
                    )
                }
                if count == 0 {
                    throw IPCError.systemCall("Unable to write the complete agent request.")
                }
                offset += count
            }
        }
    }

    /// Every response in the current protocol carries the authoritative
    /// OpenCodeServerAgent status, including failed configuration validation.
    /// Keep one-shot and subscription validation identical.
    static func requireCurrentStatus(_ response: AgentResponse) throws -> AgentStatus {
        guard response.version == ipcProtocolVersion else {
            throw IPCError.protocolMismatch(response.version)
        }
        guard let status = response.status else {
            throw IPCError.missingStatus
        }
        guard status.protocolVersion == ipcProtocolVersion else {
            throw IPCError.protocolMismatch(status.protocolVersion)
        }
        return status
    }

    static func systemError(_ prefix: String) -> IPCError {
        IPCError.systemCall("\(prefix): \(String(cString: strerror(errno)))")
    }
}
