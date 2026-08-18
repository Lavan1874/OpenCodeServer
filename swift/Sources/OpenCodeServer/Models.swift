import Foundation

let ipcProtocolVersion = 6

enum AgentCommand: String, Codable {
    case status
    case start
    case stop
    case continueStop = "continue_stop"
    case forceStop = "force_stop"
    case restart
    case refreshFda = "refresh_fda"
    case refreshCredentials = "refresh_credentials"
    case credentialChanged = "credential_changed"
    case credentialRemoved = "credential_removed"
    case validateConfig = "validate_config"
    case subscribe
}

struct AgentRequest: Codable {
    let version: Int
    let command: AgentCommand

    init(command: AgentCommand) {
        version = ipcProtocolVersion
        self.command = command
    }
}

enum DesiredState: String, Codable {
    case running
    case stopped
}

enum ServerState: String, Codable {
    case stopped
    case starting
    case healthy
    case unhealthy
    case stopping
    case stopTimedOut = "stop_timed_out"
    case waitingToRestart = "waiting_to_restart"
    case failed
}

enum HealthState: String, Codable {
    case unknown
    case healthy
    case unhealthy
}

enum FDAState: String, Codable {
    case verified
    case notVerified = "not_verified"
    case unableToDetermine = "unable_to_determine"
}

/// OpenCodeServerAgent's view of the Keychain credential. `accessPending`
/// means an item may exist but the agent has not been authorized to read it;
/// the user grants access from the Settings window. It is never treated as
/// "not configured".
enum PasswordState: String, Codable {
    case notConfigured = "not_configured"
    case accessPending = "access_pending"
    case configured
}

/// The action set computed by OpenCodeServerAgent from its authoritative
/// runtime, process-identity, credential, and durability facts. OpenCodeServer
/// only maps these values to menu enabled states; it does not reconstruct the
/// OpenCodeServerAgent's lifecycle preconditions locally.
struct ActionCapabilities: Codable, Equatable {
    let start: Bool
    let stop: Bool
    let restart: Bool
    let continueStop: Bool
    let forceStop: Bool

    static let unavailable = ActionCapabilities(
        start: false,
        stop: false,
        restart: false,
        continueStop: false,
        forceStop: false
    )
}

enum NotificationKind: String, Codable {
    case failure
    case recovered
    case finalFailure = "final_failure"
}

/// Menu-row text for the credential state. Never reveals the secret: a
/// fixed-length mask when configured, and an explicit pointer to Settings
/// when OpenCodeServerAgent still needs a Keychain access grant.
func passwordMenuLabel(_ state: PasswordState?) -> String {
    switch state {
    case .configured:
        "Password: ••••••••••••  Configured"
    case .accessPending:
        "Password: Access not granted — open Settings"
    case .notConfigured:
        "Password: Not configured"
    case nil:
        "Password: Unable to determine"
    }
}

func authenticationMenuLabel(_ enabled: Bool?) -> String {
    switch enabled {
    case true:
        "Authentication: Enabled"
    case false:
        "Authentication: Not enabled"
    case nil:
        "Authentication: Unable to determine"
    }
}

struct AgentNotification: Codable, Equatable {
    let eventID: String
    let kind: NotificationKind
    let title: String
    let message: String

    private enum CodingKeys: String, CodingKey {
        // IPC decoders use convertFromSnakeCase, which normalizes event_id to
        // eventId before matching the Swift coding key. Spell out that
        // normalized wire key while keeping the public Swift acronym as ID.
        case eventID = "eventId"
        case kind
        case title
        case message
    }
}

struct AgentStatus: Codable {
    let protocolVersion: Int
    let agentVersion: String
    let agentUptimeSeconds: UInt64
    let desiredState: DesiredState
    let serverState: ServerState
    let health: HealthState
    let fda: FDAState
    let uptimeSeconds: UInt64?
    let endpoint: String
    let username: String
    let passwordState: PasswordState
    let authenticationEnabled: Bool
    let actionCapabilities: ActionCapabilities
    let installedVersion: String?
    let runningVersion: String?
    let versionPending: Bool
    let configPending: Bool
    let configError: String?
    let lastError: String?
    let pid: UInt32?
    let stopGraceRemainingSeconds: UInt64?
    let notification: AgentNotification?
    // Nil while OpenCode is not running; used to render uptime locally
    // between pushes (ADR 0010).
    let processStartedAtUnixSeconds: UInt64?
    // The parent bundle's CFBundleVersion baked into the OpenCodeServerAgent
    // binary at build time.
    let bundleVersion: String
}

struct ValidationReport: Codable {
    let valid: Bool
    let issues: [String]
    let selectedExecutable: String?
    let candidates: [String]
}

/// Which conditional status rows the status menu shows for a given status
/// (progressive disclosure; see AppDelegate.statusRowVisibility). The
/// always-visible rows — OpenCode health, uptime, endpoint, version — are
/// not part of this set.
struct StatusRowVisibility {
    let openCodeServerAgent: Bool
    let fda: Bool
    let password: Bool
    let authentication: Bool
    let configuration: Bool
}

struct AgentResponse: Codable {
    let version: Int
    let ok: Bool
    let error: String?
    let status: AgentStatus?
    let validation: ValidationReport?
}

enum StatusColor {
    case green
    case yellow
    case red
    case gray
}

struct StatusPresentation: Equatable {
    let color: StatusColor
    let label: String

    static func from(status: AgentStatus?) -> StatusPresentation {
        guard let status else {
            return StatusPresentation(
                color: .gray,
                label: "OpenCodeServerAgent Temporarily Unavailable"
            )
        }
        switch status.serverState {
        case .healthy:
            return StatusPresentation(color: .green, label: "Healthy")
        case .starting:
            return StatusPresentation(color: .yellow, label: "Starting")
        case .unhealthy:
            return StatusPresentation(color: .yellow, label: "Running, Not Healthy")
        case .stopping:
            return StatusPresentation(color: .yellow, label: "Stopping")
        case .stopTimedOut:
            return StatusPresentation(color: .yellow, label: "Waiting to Stop")
        case .waitingToRestart:
            return StatusPresentation(color: .yellow, label: "Waiting to Restart")
        case .failed:
            return StatusPresentation(color: .red, label: "Fault")
        case .stopped:
            return StatusPresentation(color: .gray, label: "Stopped")
        }
    }
}

func formatDuration(_ seconds: UInt64?) -> String {
    guard let seconds else { return "—" }
    let days = seconds / 86_400
    let hours = (seconds % 86_400) / 3_600
    let minutes = (seconds % 3_600) / 60
    let secs = seconds % 60
    if days > 0 {
        return "\(days)d \(hours)h \(minutes)m \(secs)s"
    }
    if hours > 0 {
        return "\(hours)h \(minutes)m \(secs)s"
    }
    if minutes > 0 {
        return "\(minutes)m \(secs)s"
    }
    return "\(secs)s"
}
