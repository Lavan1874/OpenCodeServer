import Foundation

/// The reason for the one bounded OpenCodeServerAgent registration
/// transaction currently in flight.
enum OpenCodeServerAgentRegistrationPurpose: String, Codable, Equatable, Sendable {
    case initialRegistration
    case bundleUpgrade
    case explicitRepair
}

/// A persisted phase is deliberately less detailed than the in-memory phase:
/// scheduler check counters are transient, while this boundary is what lets a
/// new OpenCodeServer process resume the same attempt after a crash.
enum OpenCodeServerAgentRegistrationTransactionPhase: String, Codable, Equatable, Sendable {
    case unregistering
    case awaitingUnregistered
    case awaitingRegistration
    case awaitingIPC
    case retryScheduled
}

/// Current-format durable intent for one OpenCodeServerAgent registration
/// transaction. A single encoded value is stored so version, purpose, phase,
/// and attempt cannot be observed as a partially updated set of defaults.
struct OpenCodeServerAgentRegistrationTransaction: Codable, Equatable, Sendable {
    static let currentSchemaVersion = 1

    let schemaVersion: Int
    let version: String
    let purpose: OpenCodeServerAgentRegistrationPurpose
    let phase: OpenCodeServerAgentRegistrationTransactionPhase
    let attempt: Int

    init(
        version: String,
        purpose: OpenCodeServerAgentRegistrationPurpose,
        phase: OpenCodeServerAgentRegistrationTransactionPhase,
        attempt: Int
    ) {
        schemaVersion = Self.currentSchemaVersion
        self.version = version
        self.purpose = purpose
        self.phase = phase
        self.attempt = attempt
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion
        case version
        case purpose
        case phase
        case attempt
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let schemaVersion = try container.decode(Int.self, forKey: .schemaVersion)
        guard schemaVersion == Self.currentSchemaVersion else {
            throw DecodingError.dataCorruptedError(
                forKey: .schemaVersion,
                in: container,
                debugDescription: "Unsupported OpenCodeServerAgent registration transaction schema"
            )
        }
        self.schemaVersion = schemaVersion
        version = try container.decode(String.self, forKey: .version)
        purpose = try container.decode(
            OpenCodeServerAgentRegistrationPurpose.self,
            forKey: .purpose
        )
        phase = try container.decode(
            OpenCodeServerAgentRegistrationTransactionPhase.self,
            forKey: .phase
        )
        attempt = try container.decode(Int.self, forKey: .attempt)
    }
}

/// Persists one registration transaction as one UserDefaults value. The
/// ServiceController is main-actor isolated, so all reads and writes are
/// serialized with the Service Management state observations.
struct OpenCodeServerAgentRegistrationTransactionStore {
    static let defaultsKey = "OpenCodeServerAgentRegistrationTransaction"

    enum StoreError: Error {
        case persistenceSynchronizationFailed
    }

    enum LoadResult {
        case missing
        case valid(OpenCodeServerAgentRegistrationTransaction)
        case invalid
    }

    private let defaults: UserDefaults

    init(defaults: UserDefaults) {
        self.defaults = defaults
    }

    func load() -> LoadResult {
        guard defaults.object(forKey: Self.defaultsKey) != nil else {
            return .missing
        }
        guard let data = defaults.data(forKey: Self.defaultsKey) else {
            return .invalid
        }
        guard let transaction = try? PropertyListDecoder().decode(
            OpenCodeServerAgentRegistrationTransaction.self,
            from: data
        ) else {
            return .invalid
        }
        return .valid(transaction)
    }

    func save(_ transaction: OpenCodeServerAgentRegistrationTransaction) throws {
        let encoder = PropertyListEncoder()
        encoder.outputFormat = .binary
        let data = try encoder.encode(transaction)
        defaults.set(data, forKey: Self.defaultsKey)
        // UserDefaults writes asynchronously. This transaction is written
        // immediately before an external unregister side effect, so wait for
        // the persistent store to acknowledge the complete record. Apple
        // documents synchronize() as unnecessary for ordinary settings; this
        // is the narrow crash-boundary exception where losing the write would
        // reset the bounded Service Management attempt budget.
        guard defaults.synchronize() else {
            throw StoreError.persistenceSynchronizationFailed
        }
    }

    func clear() {
        defaults.removeObject(forKey: Self.defaultsKey)
    }
}

enum OpenCodeServerAgentRegistrationTransactionLookup {
    case missing
    case valid(OpenCodeServerAgentRegistrationTransaction)
    case staleVersion(OpenCodeServerAgentRegistrationTransaction)
    case invalid
}

enum OpenCodeServerAgentRegistrationTransactionRecoveryAction {
    case waitForUnregistered
    case startReplacement
    case scheduleRegistration
    case awaitIPC
    case performRegistration
}

/// Keeps persistence, validation, and phase-to-resume mapping out of
/// ServiceController. It does not call Service Management; the controller
/// remains the owner of ordering and side effects.
struct OpenCodeServerAgentRegistrationTransactionCoordinator {
    private let store: OpenCodeServerAgentRegistrationTransactionStore
    private let bundleVersion: String
    private let maximumAttempts: Int

    init(
        defaults: UserDefaults,
        bundleVersion: String,
        maximumAttempts: Int
    ) {
        store = OpenCodeServerAgentRegistrationTransactionStore(defaults: defaults)
        self.bundleVersion = bundleVersion
        self.maximumAttempts = maximumAttempts
    }

    func lookup() -> OpenCodeServerAgentRegistrationTransactionLookup {
        switch store.load() {
        case .missing:
            return .missing
        case let .valid(transaction):
            guard transaction.version == bundleVersion else {
                return .staleVersion(transaction)
            }
            guard transaction.attempt >= 0,
                  transaction.attempt < maximumAttempts
            else {
                return .invalid
            }
            return .valid(transaction)
        case .invalid:
            return .invalid
        }
    }

    func save(
        phase: OpenCodeServerAgentRegistrationTransactionPhase,
        purpose: OpenCodeServerAgentRegistrationPurpose,
        attempt: Int
    ) throws {
        try store.save(
            OpenCodeServerAgentRegistrationTransaction(
                version: bundleVersion,
                purpose: purpose,
                phase: phase,
                attempt: attempt
            )
        )
    }

    func clear() {
        store.clear()
    }

    func recoveryAction(
        for transaction: OpenCodeServerAgentRegistrationTransaction,
        serviceIsEnabled: Bool,
        serviceIsNotRegistered: Bool
    ) -> OpenCodeServerAgentRegistrationTransactionRecoveryAction {
        switch transaction.phase {
        case .unregistering:
            return serviceIsNotRegistered ? .waitForUnregistered : .startReplacement
        case .awaitingUnregistered:
            return .waitForUnregistered
        case .awaitingRegistration:
            return serviceIsEnabled ? .awaitIPC : .scheduleRegistration
        case .awaitingIPC:
            return serviceIsEnabled ? .awaitIPC : .performRegistration
        case .retryScheduled:
            return serviceIsNotRegistered ? .performRegistration : .startReplacement
        }
    }
}
