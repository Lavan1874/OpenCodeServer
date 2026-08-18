import Darwin
import Foundation
import OSLog

/// Splits a byte stream into newline-delimited IPC messages with the same
/// 64 KiB bound one-shot responses use. Wire semantics (ADR 0010 addendum):
/// a complete message is the JSON body plus its terminating newline, and the
/// whole line must fit the bound — identical to the agent's read side, which
/// measures the line including the terminator.
struct SubscriptionFraming {
    static let maximumMessageBytes = 65_536

    private var pending = Data()

    mutating func append(_ bytes: some Sequence<UInt8>) throws -> [Data] {
        pending.append(contentsOf: bytes)
        var lines: [Data] = []
        while let newline = pending.firstIndex(of: 0x0A) {
            let bodyLength = pending.distance(from: pending.startIndex, to: newline)
            // The line including its terminator must fit the bound.
            guard bodyLength + 1 <= Self.maximumMessageBytes else {
                throw IPCError.responseTooLarge
            }
            lines.append(Data(pending[pending.startIndex ..< newline]))
            pending.removeSubrange(pending.startIndex ... newline)
        }
        // An unterminated buffer may grow to at most one terminator short of
        // the bound; anything larger could never become a legal message.
        guard pending.count < Self.maximumMessageBytes else {
            throw IPCError.responseTooLarge
        }
        return lines
    }
}

/// Timing knobs for `AgentStatusSubscription`. Production uses the defaults;
/// tests inject shorter values so lifecycle behavior runs in milliseconds.
struct SubscriptionTiming {
    /// The agent heartbeats every 10 seconds; missing more than two
    /// heartbeats means the connection is silently dead.
    var heartbeatTolerance: TimeInterval = 25
    var readTimeoutMilliseconds = 5_000
    var backoff: [TimeInterval] = [1, 2, 5, 15]
}

/// A persistent subscription using the current IPC protocol that receives a pushed
/// `AgentStatus` on every OpenCodeServerAgent state change plus a heartbeat
/// (ADR 0010). Callbacks arrive on a private background thread.
///
/// The lifecycle is explicit: a connection that fails before any well-formed
/// message arrives is retried silently with accumulating backoff; a drop
/// after streaming started reports `onUnreachable` immediately (so the menu
/// turns gray at once) and reconnects without the accumulated delay;
final class AgentStatusSubscription: @unchecked Sendable {
    var onStatus: ((AgentStatus) -> Void)?
    var onUnreachable: (() -> Void)?

    private let socketPath: String
    private let timing: SubscriptionTiming
    private let logger = Logger(subsystem: "ai.opencode.server", category: "ipc")
    private let lock = NSLock()
    private var descriptor: Int32 = -1
    private var invalidated = false
    /// Readable for tests (via `@testable`) so they can await worker exit.
    private(set) var thread: Thread?

    init(socketPath: String, timing: SubscriptionTiming = SubscriptionTiming()) {
        self.socketPath = socketPath
        self.timing = timing
    }

    func start() {
        lock.lock()
        guard thread == nil, !invalidated else {
            lock.unlock()
            return
        }
        let worker = Thread(block: { [weak self] in self?.run() })
        worker.name = "ai.opencode.server.subscription"
        worker.qualityOfService = .utility
        thread = worker
        lock.unlock()
        worker.start()
    }

    func invalidate() {
        lock.lock()
        invalidated = true
        let current = descriptor
        if current >= 0 {
            // Serialized with releaseDescriptor under the same lock: while
            // the worker still lists the descriptor it also still owns it,
            // so this shutdown always lands on the live subscription socket
            // and never on an fd the kernel has already recycled.
            Darwin.shutdown(current, SHUT_RDWR)
        }
        lock.unlock()
    }

    private func isInvalidated() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return invalidated
    }

    private func setDescriptor(_ value: Int32) {
        lock.lock()
        descriptor = value
        lock.unlock()
    }

    /// The worker is the sole owner of close(): forgetting and releasing the
    /// descriptor are one locked step, so invalidate()'s shutdown can never
    /// land on an fd that was already closed and reused by another thread.
    private func releaseDescriptor(_ connectionDescriptor: Int32) {
        lock.lock()
        if descriptor == connectionDescriptor {
            descriptor = -1
        }
        Darwin.close(connectionDescriptor)
        lock.unlock()
    }

    /// How one subscription connection ended. An initial failure backs off
    /// silently, a drop after streaming reports the gap immediately, and
    /// `invalidate()` is quiet.
    private enum StreamEnd {
        case failedBeforeStreaming(Error)
        case disconnectedAfterStreaming(Error)
        case invalidated
    }

    private func run() {
        var failures = 0
        while !isInvalidated() {
            switch runConnection() {
            case .invalidated:
                return
            case let .failedBeforeStreaming(error):
                failures += 1
                logger.debug(
                    "Subscription connection failed before streaming: \(error.localizedDescription, privacy: .public)"
                )
            case let .disconnectedAfterStreaming(error):
                // A live subscription dropped: report the gap immediately so
                // the menu turns gray now, and reconnect at the first backoff
                // step instead of the accumulated failure-streak delay.
                failures = 0
                logger.debug(
                    "Subscription connection dropped: \(error.localizedDescription, privacy: .public)"
                )
                onUnreachable?()
            }
            let delay = timing.backoff[min(max(failures - 1, 0), timing.backoff.count - 1)]
            if sleepInterruptibly(delay) {
                return
            }
        }
    }

    /// Runs one connection until it ends, classifying the outcome. A
    /// connection counts as streaming once the first well-formed response
    /// was decoded; only streaming connections report `onUnreachable`.
    private func runConnection() -> StreamEnd {
        var streamed = false
        do {
            let descriptor = try IPCClient.openConnection(
                socketPath: socketPath,
                timeoutMilliseconds: timing.readTimeoutMilliseconds
            )
            setDescriptor(descriptor)
            defer { releaseDescriptor(descriptor) }

            var request = try JSONEncoder().encode(AgentRequest(command: .subscribe))
            request.append(0x0A)
            try IPCClient.writeAll(request, to: descriptor)

            var framing = SubscriptionFraming()
            var buffer = [UInt8](repeating: 0, count: 4_096)
            var lastMessage = Date()
            while !isInvalidated() {
                let count = buffer.withUnsafeMutableBytes { rawBuffer in
                    Darwin.read(descriptor, rawBuffer.baseAddress, rawBuffer.count)
                }
                if count < 0 {
                    let readError = errno
                    if readError == EINTR { continue }
                    if readError == EAGAIN {
                        // Receive-timeout tick: only the watchdog uses it.
                        if Date().timeIntervalSince(lastMessage) > timing.heartbeatTolerance {
                            throw IPCError.systemCall("OpenCodeServerAgent heartbeat timed out")
                        }
                        continue
                    }
                    throw IPCError.systemCall(
                        "Unable to read the OpenCodeServerAgent subscription: \(String(cString: strerror(readError)))"
                    )
                }
                if count == 0 {
                    throw IPCError.emptyResponse
                }
                lastMessage = Date()
                for line in try framing.append(buffer.prefix(count)) {
                    let decoder = JSONDecoder()
                    decoder.keyDecodingStrategy = .convertFromSnakeCase
                    let response = try decoder.decode(AgentResponse.self, from: line)
                    let status = try IPCClient.requireCurrentStatus(response)
                    if !response.ok {
                        let message =
                            response.error ?? "OpenCodeServerAgent rejected the subscription"
                        throw IPCError.agent(message)
                    }
                    streamed = true
                    onStatus?(status)
                }
            }
            return .invalidated
        } catch {
            if isInvalidated() {
                return .invalidated
            }
            return streamed
                ? .disconnectedAfterStreaming(error)
                : .failedBeforeStreaming(error)
        }
    }

    private func sleepInterruptibly(_ interval: TimeInterval) -> Bool {
        let deadline = Date().addingTimeInterval(interval)
        while !isInvalidated(), Date() < deadline {
            Thread.sleep(forTimeInterval: 0.1)
        }
        return isInvalidated()
    }
}
