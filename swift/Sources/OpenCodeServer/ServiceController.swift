import Foundation
import OSLog
import ServiceManagement

@MainActor
protocol AppServiceControlling: AnyObject {
    var status: SMAppService.Status { get }

    func register() throws
    func unregister(completionHandler: @escaping (Error?) -> Void)
}

@MainActor
private final class SystemAppService: AppServiceControlling {
    private let service: SMAppService

    init(_ service: SMAppService) {
        self.service = service
    }

    var status: SMAppService.Status {
        service.status
    }

    func register() throws {
        try service.register()
    }

    func unregister(completionHandler: @escaping (Error?) -> Void) {
        service.unregister { error in
            DispatchQueue.main.async {
                completionHandler(error)
            }
        }
    }
}

@MainActor
protocol ServiceUpdateScheduling: AnyObject {
    func schedule(after delay: TimeInterval, action: @escaping () -> Void)
}

@MainActor
private final class MainQueueServiceUpdateScheduler: ServiceUpdateScheduling {
    func schedule(after delay: TimeInterval, action: @escaping () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            action()
        }
    }
}

enum OpenCodeServerAgentServiceError: LocalizedError {
    case operationInProgress
    case requiresApproval
    case notFound
    case statusDidNotBecomeUnregistered
    case acceptedRegistrationDidNotBecomeReachable
    case registrationTransactionCouldNotBeSaved

    var errorDescription: String? {
        switch self {
        case .operationInProgress:
            "An OpenCodeServerAgent registration operation is already in progress."
        case .requiresApproval:
            "OpenCodeServerAgent requires approval in Login Items & Extensions."
        case .notFound:
            "OpenCodeServerAgent could not be found in the OpenCodeServer bundle."
        case .statusDidNotBecomeUnregistered:
            "OpenCodeServerAgent did not reach the unregistered state."
        case .acceptedRegistrationDidNotBecomeReachable:
            "OpenCodeServerAgent registration was accepted but authenticated IPC did not become reachable."
        case .registrationTransactionCouldNotBeSaved:
            "OpenCodeServerAgent registration state could not be saved safely."
        }
    }
}

@MainActor
final class ServiceController {
    static let openCodeServerAgentPlistName = "ai.opencode.server.agent.plist"
    private static let installedApplicationPath = "/Applications/OpenCodeServer.app"

    private let logger: Logger
    private let openCodeServerAgent: any AppServiceControlling
    private let openCodeServer: any AppServiceControlling
    private let applicationPath: String
    private let registrationController: OpenCodeServerAgentRegistrationController

    var onRegistrationVerificationPendingChange: ((Bool) -> Void)? {
        get { registrationController.onRegistrationVerificationPendingChange }
        set { registrationController.onRegistrationVerificationPendingChange = newValue }
    }

    convenience init() {
        self.init(
            defaults: .standard,
            openCodeServerAgent: SystemAppService(
                SMAppService.agent(plistName: Self.openCodeServerAgentPlistName)
            ),
            openCodeServer: SystemAppService(.mainApp),
            scheduler: MainQueueServiceUpdateScheduler(),
            applicationPath: Bundle.main.bundleURL.standardizedFileURL.path,
            bundleVersion: Bundle.main.object(
                forInfoDictionaryKey: "CFBundleVersion"
            ) as? String ?? "0",
            logger: Logger(
                subsystem: "ai.opencode.server",
                category: "service"
            )
        )
    }

    init(
        defaults: UserDefaults,
        openCodeServerAgent: any AppServiceControlling,
        openCodeServer: any AppServiceControlling,
        scheduler: any ServiceUpdateScheduling,
        applicationPath: String,
        bundleVersion: String,
        systemUptime: @escaping () -> TimeInterval = {
            ProcessInfo.processInfo.systemUptime
        },
        logger: Logger = Logger(
            subsystem: "ai.opencode.server",
            category: "service"
        )
    ) {
        self.logger = logger
        self.openCodeServerAgent = openCodeServerAgent
        self.openCodeServer = openCodeServer
        self.applicationPath = applicationPath
        registrationController = OpenCodeServerAgentRegistrationController(
            defaults: defaults,
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler,
            bundleVersion: bundleVersion,
            systemUptime: systemUptime,
            logger: logger
        )
    }

    var openCodeServerAgentStatus: SMAppService.Status {
        openCodeServerAgent.status
    }

    var openCodeServerLoginStatus: SMAppService.Status {
        openCodeServer.status
    }

    func bootstrapInstalledApplication() {
        guard applicationPath == Self.installedApplicationPath else {
            logger.notice("Skipping Service Management registration outside /Applications")
            return
        }
        registerOpenCodeServerAtLogin()
        registrationController.bootstrap()
    }

    func observeOpenCodeServerAgentReachability(_ status: AgentStatus?) {
        registrationController.observeReachability(status)
    }

    func registerOpenCodeServerAgent() {
        registrationController.register()
    }

    func repairOpenCodeServerAgent(completion: @escaping (Error?) -> Void) {
        registrationController.repair(completion: completion)
    }

    func unregisterOpenCodeServerAgent(completion: @escaping (Error?) -> Void) {
        registrationController.cancel()
        openCodeServerAgent.unregister { [logger] error in
            if let error {
                logger.error(
                    "OpenCodeServerAgent unregistration failed: \(error.localizedDescription, privacy: .public)"
                )
            } else {
                logger.notice("OpenCodeServerAgent unregistered")
            }
            completion(error)
        }
    }

    func registerOpenCodeServerAtLogin() {
        guard openCodeServer.status == .notRegistered else { return }
        do {
            try openCodeServer.register()
            logger.notice("OpenCodeServer login registration succeeded")
        } catch {
            logger.error(
                "OpenCodeServer login registration failed: \(error.localizedDescription, privacy: .public)"
            )
        }
    }

    func unregisterOpenCodeServerAtLogin(completion: @escaping (Error?) -> Void) {
        openCodeServer.unregister { error in
            completion(error)
        }
    }

    func setOpenCodeServerLoginEnabled(
        _ enabled: Bool,
        completion: @escaping (Error?) -> Void
    ) {
        if enabled {
            guard openCodeServer.status != .enabled else {
                completion(nil)
                return
            }
            do {
                try openCodeServer.register()
                completion(nil)
            } catch {
                completion(error)
            }
        } else {
            unregisterOpenCodeServerAtLogin(completion: completion)
        }
    }

    func openLoginItemsSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }

    func openCodeServerAgentStatusLabel(openCodeServerAgentReachable: Bool) -> String {
        registrationController.statusLabel(
            openCodeServerAgentReachable: openCodeServerAgentReachable
        )
    }
}
