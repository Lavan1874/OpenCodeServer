@testable import OpenCodeServer
import Foundation
import ServiceManagement
import XCTest

enum TestError: Error {
    case rejected
}

@MainActor
final class FakeAppService: AppServiceControlling {
    var status: SMAppService.Status
    var registerCount = 0
    var unregisterCount = 0
    var completesUnregisterImmediately = true
    var registerError: Error?
    var unregisterError: Error?
    var onUnregister: (() -> Void)?

    private var pendingUnregisterCompletion: ((Error?) -> Void)?

    init(status: SMAppService.Status) {
        self.status = status
    }

    func register() throws {
        registerCount += 1
        if let registerError {
            throw registerError
        }
        status = .enabled
    }

    func unregister(completionHandler: @escaping (Error?) -> Void) {
        unregisterCount += 1
        onUnregister?()
        if completesUnregisterImmediately {
            if unregisterError == nil {
                status = .notRegistered
            }
            completionHandler(unregisterError)
        } else {
            pendingUnregisterCompletion = completionHandler
        }
    }

    func completeUnregistration(transitionToNotRegistered: Bool = true) {
        if unregisterError == nil, transitionToNotRegistered {
            status = .notRegistered
        }
        let completion = pendingUnregisterCompletion
        pendingUnregisterCompletion = nil
        completion?(unregisterError)
    }
}

@MainActor
final class ManualServiceUpdateScheduler: ServiceUpdateScheduling {
    private var actions: [() -> Void] = []

    var hasPendingActions: Bool {
        !actions.isEmpty
    }

    func schedule(after _: TimeInterval, action: @escaping () -> Void) {
        actions.append(action)
    }

    func runNext() {
        guard !actions.isEmpty else { return }
        actions.removeFirst()()
    }

    func runAll(limit: Int = 100) {
        var remaining = limit
        while !actions.isEmpty, remaining > 0 {
            remaining -= 1
            runNext()
        }
        XCTAssertTrue(
            actions.isEmpty,
            "OpenCodeServerAgent scheduled actions exceeded the test limit"
        )
    }
}

@MainActor
func makeServiceControllerForTest(
    defaults: UserDefaults,
    version: String,
    openCodeServerAgent: FakeAppService,
    scheduler: ManualServiceUpdateScheduler,
    systemUptime: @escaping () -> TimeInterval = { 3600 }
) -> ServiceController {
    ServiceController(
        defaults: defaults,
        openCodeServerAgent: openCodeServerAgent,
        openCodeServer: FakeAppService(status: .enabled),
        scheduler: scheduler,
        applicationPath: "/Applications/OpenCodeServer.app",
        bundleVersion: version,
        systemUptime: systemUptime
    )
}

func makeAgentStatusForTest(bundleVersion: String) -> AgentStatus {
    AgentStatus(
        protocolVersion: ipcProtocolVersion,
        agentVersion: "0.1.0",
        agentUptimeSeconds: 1,
        desiredState: .running,
        serverState: .healthy,
        health: .healthy,
        fda: .verified,
        uptimeSeconds: 1,
        endpoint: "127.0.0.1:4096",
        username: "test",
        passwordState: .notConfigured,
        authenticationEnabled: false,
        actionCapabilities: .unavailable,
        installedVersion: "1.0",
        runningVersion: "1.0",
        versionPending: false,
        configPending: false,
        configError: nil,
        lastError: nil,
        pid: 42,
        stopGraceRemainingSeconds: nil,
        notification: nil,
        processStartedAtUnixSeconds: nil,
        bundleVersion: bundleVersion
    )
}

func loadRegistrationTransactionForTest(
    defaults: UserDefaults
) -> OpenCodeServerAgentRegistrationTransaction? {
    guard case let .valid(transaction) =
        OpenCodeServerAgentRegistrationTransactionStore(defaults: defaults).load()
    else {
        return nil
    }
    return transaction
}

func saveRegistrationTransactionForTest(
    defaults: UserDefaults,
    version: String,
    purpose: OpenCodeServerAgentRegistrationPurpose,
    phase: OpenCodeServerAgentRegistrationTransactionPhase,
    attempt: Int
) {
    let store = OpenCodeServerAgentRegistrationTransactionStore(defaults: defaults)
    try! store.save(
        OpenCodeServerAgentRegistrationTransaction(
            version: version,
            purpose: purpose,
            phase: phase,
            attempt: attempt
        )
    )
}
