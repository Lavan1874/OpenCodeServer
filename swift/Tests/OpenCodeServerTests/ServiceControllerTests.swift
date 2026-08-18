@testable import OpenCodeServer
import Darwin
import Foundation
import XCTest

@MainActor
final class ServiceControllerTests: XCTestCase {
    private var defaults: UserDefaults!
    private var defaultsSuite: String!

    override func setUp() {
        super.setUp()
        defaultsSuite = "ai.opencode.server.tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: defaultsSuite)
        defaults.removePersistentDomain(forName: defaultsSuite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: defaultsSuite)
        defaults = nil
        defaultsSuite = nil
        super.tearDown()
    }

    func testFirstOpenCodeServerAgentRegistrationCommitsVersionOnlyAfterIPC() {
        let openCodeServerAgent = FakeAppService(status: .notRegistered)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "1",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
        XCTAssertNil(defaults.string(forKey: "RegisteredBundleVersion"))
        XCTAssertEqual(
            registrationTransaction(),
            OpenCodeServerAgentRegistrationTransaction(
                version: "1",
                purpose: .initialRegistration,
                phase: .awaitingIPC,
                attempt: 0
            )
        )

        controller.observeOpenCodeServerAgentReachability(nil)
        XCTAssertNil(defaults.string(forKey: "RegisteredBundleVersion"))
        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "1"))
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertNil(registrationTransaction())
        scheduler.runAll()
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testSameBundleVersionOpenCodeServerAgentRegistrationIsIdempotent() {
        defaults.set("2", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        for _ in 0 ..< 100 {
            controller.observeOpenCodeServerAgentReachability(nil)
        }
        scheduler.runAll()

        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "2")
        XCTAssertEqual(
            controller.openCodeServerAgentStatusLabel(
                openCodeServerAgentReachable: false
            ),
            "Temporarily Unavailable"
        )

        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "2"))
        XCTAssertEqual(
            controller.openCodeServerAgentStatusLabel(
                openCodeServerAgentReachable: true
            ),
            "Enabled and Reachable"
        )
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
    }

    func testColdOpenCodeServerAgentStartHasNoSixOrSixteenSecondRepairBoundary() {
        defaults.set("5", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "5",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        for _ in 0 ..< 1_000 {
            controller.observeOpenCodeServerAgentReachability(nil)
            scheduler.runNext()
        }

        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
        XCTAssertFalse(scheduler.hasPendingActions)

        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "5"))
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
    }

    func testBundleVersionUpgradeReplacesOpenCodeServerAgentExactlyOnce() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertEqual(
            registrationTransaction(),
            OpenCodeServerAgentRegistrationTransaction(
                version: "2",
                purpose: .bundleUpgrade,
                phase: .awaitingRegistration,
                attempt: 0
            )
        )
        scheduler.runNext()
        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
        XCTAssertEqual(registrationTransaction()?.phase, .awaitingIPC)

        for _ in 0 ..< 100 {
            controller.observeOpenCodeServerAgentReachability(nil)
        }
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(openCodeServerAgent.registerCount, 1)

        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "2"))
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "2")
        XCTAssertNil(registrationTransaction())
    }

    func testBundleVersionUpgradeWaitsForAsynchronousUnregistration() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        openCodeServerAgent.completesUnregisterImmediately = false
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        scheduler.runAll()
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)

        openCodeServerAgent.completeUnregistration()
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        scheduler.runNext()
        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
    }

    func testBundleVersionUpgradeWaitsForNotRegisteredStatus() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        openCodeServerAgent.completesUnregisterImmediately = false
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        openCodeServerAgent.completeUnregistration(
            transitionToNotRegistered: false
        )
        scheduler.runNext()
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)

        openCodeServerAgent.status = .notRegistered
        scheduler.runNext()
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        scheduler.runNext()
        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
    }

    func testAcceptedOpenCodeServerAgentRegistrationPersistsPendingVerification() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let firstOpenCodeServerAgent = FakeAppService(status: .enabled)
        let firstScheduler = ManualServiceUpdateScheduler()
        let firstController = makeController(
            version: "2",
            openCodeServerAgent: firstOpenCodeServerAgent,
            scheduler: firstScheduler
        )
        firstController.bootstrapInstalledApplication()
        firstScheduler.runNext()

        XCTAssertEqual(firstOpenCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(firstOpenCodeServerAgent.registerCount, 1)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertEqual(
            registrationTransaction(),
            OpenCodeServerAgentRegistrationTransaction(
                version: "2",
                purpose: .bundleUpgrade,
                phase: .awaitingIPC,
                attempt: 0
            )
        )

        let nextOpenCodeServerAgent = FakeAppService(status: .enabled)
        let nextController = makeController(
            version: "2",
            openCodeServerAgent: nextOpenCodeServerAgent,
            scheduler: ManualServiceUpdateScheduler()
        )
        nextController.bootstrapInstalledApplication()
        for _ in 0 ..< 100 {
            nextController.observeOpenCodeServerAgentReachability(nil)
        }

        XCTAssertEqual(nextOpenCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(nextOpenCodeServerAgent.registerCount, 0)
        XCTAssertEqual(
            nextController.openCodeServerAgentStatusLabel(
                openCodeServerAgentReachable: false
            ),
            "Starting"
        )

        nextController.observeOpenCodeServerAgentReachability(agentStatus(build: "2"))
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "2")
        XCTAssertNil(registrationTransaction())
    }

    func testBundleVersionUpgradeHasBoundedRetriesAfterIPCVerificationTimeout() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        scheduler.runAll()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 3)
        XCTAssertEqual(openCodeServerAgent.registerCount, 3)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertNil(registrationTransaction())
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testPendingBundleVersionUpgradePreservesRetryBudgetAcrossOpenCodeServerRestart() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        saveRegistrationTransaction(
            version: "2",
            purpose: .bundleUpgrade,
            phase: .awaitingIPC,
            attempt: 1
        )
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        scheduler.runAll()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertNil(registrationTransaction())
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testRejectedOpenCodeServerAgentRegistrationRetriesOnlyOnLaterLaunch() {
        saveRegistrationTransaction(
            version: "2",
            purpose: .bundleUpgrade,
            phase: .awaitingRegistration,
            attempt: 0
        )
        let firstOpenCodeServerAgent = FakeAppService(status: .notRegistered)
        firstOpenCodeServerAgent.registerError = TestError.rejected
        let firstScheduler = ManualServiceUpdateScheduler()
        let firstController = makeController(
            version: "2",
            openCodeServerAgent: firstOpenCodeServerAgent,
            scheduler: firstScheduler
        )

        firstController.bootstrapInstalledApplication()
        firstScheduler.runAll()
        for _ in 0 ..< 100 {
            firstController.observeOpenCodeServerAgentReachability(nil)
        }

        XCTAssertEqual(firstOpenCodeServerAgent.registerCount, 1)
        XCTAssertEqual(firstOpenCodeServerAgent.unregisterCount, 0)
        XCTAssertNil(registrationTransaction())

        let nextOpenCodeServerAgent = FakeAppService(status: .notRegistered)
        let nextScheduler = ManualServiceUpdateScheduler()
        let nextController = makeController(
            version: "2",
            openCodeServerAgent: nextOpenCodeServerAgent,
            scheduler: nextScheduler
        )
        nextController.bootstrapInstalledApplication()
        nextScheduler.runAll()

        XCTAssertEqual(nextOpenCodeServerAgent.registerCount, 1)
        XCTAssertEqual(nextOpenCodeServerAgent.unregisterCount, 0)
    }

    func testExplicitRepairReportsUnregistrationFailure() {
        defaults.set("5", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        openCodeServerAgent.unregisterError = TestError.rejected
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "5",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )
        controller.bootstrapInstalledApplication()

        var completionCalled = false
        var completionError: Error?
        controller.repairOpenCodeServerAgent {
            completionCalled = true
            completionError = $0
        }

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        XCTAssertTrue(completionCalled)
        XCTAssertNotNil(completionError)
        scheduler.runAll()
        XCTAssertEqual(
            openCodeServerAgent.registerCount,
            0,
            "a failed unregistration must not proceed to re-registration"
        )
    }

    func testExplicitRepairReportsRegistrationFailure() {
        let openCodeServerAgent = FakeAppService(status: .notRegistered)
        openCodeServerAgent.registerError = TestError.rejected
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "5",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        var completionCalled = false
        var completionError: Error?
        controller.repairOpenCodeServerAgent {
            completionCalled = true
            completionError = $0
        }

        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
        XCTAssertTrue(completionCalled)
        XCTAssertNotNil(completionError)
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testExplicitRepairExhaustsAttemptsAndReportsCompletionError() {
        // The bounded replacement transaction (three attempts) expires when
        // no build ever proves the expected version over IPC; the explicit
        // repair completion must then report the failure, not hang.
        defaults.set("5", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "5",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )
        controller.bootstrapInstalledApplication()

        var completionCalled = false
        var completionError: Error?
        controller.repairOpenCodeServerAgent {
            completionCalled = true
            completionError = $0
        }
        scheduler.runAll()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 3)
        XCTAssertEqual(openCodeServerAgent.registerCount, 3)
        XCTAssertTrue(completionCalled)
        XCTAssertEqual(
            completionError as? OpenCodeServerAgentServiceError,
            .acceptedRegistrationDidNotBecomeReachable
        )
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "5")
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testUnregisterCancelsInFlightOpenCodeServerAgentRegistration() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )
        controller.bootstrapInstalledApplication()
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)

        var completionCalled = false
        var completionError: Error?
        controller.unregisterOpenCodeServerAgent {
            completionCalled = true
            completionError = $0
        }

        XCTAssertTrue(completionCalled)
        XCTAssertNil(completionError)
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 2)
        scheduler.runAll()
        XCTAssertEqual(
            openCodeServerAgent.registerCount,
            0,
            "the canceled transaction's scheduled re-registration must not fire"
        )
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testExplicitRepairIsOnlySameVersionReplacementPath() {
        defaults.set("5", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "5",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)

        var completionCalled = false
        var completionError: Error?
        controller.repairOpenCodeServerAgent {
            completionCalled = true
            completionError = $0
        }
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        scheduler.runNext()

        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
        XCTAssertFalse(completionCalled)
        XCTAssertNil(completionError)
        XCTAssertEqual(registrationTransaction()?.phase, .awaitingIPC)
        XCTAssertEqual(registrationTransaction()?.purpose, .explicitRepair)

        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "5"))

        XCTAssertTrue(completionCalled)
        XCTAssertNil(completionError)
    }

    func testOpenCodeServerAgentUpdateDoesNotSignalUnrelatedOpenCode() throws {
        let openCode = Process()
        openCode.executableURL = URL(filePath: "/bin/sleep")
        openCode.arguments = ["30"]
        try openCode.run()
        defer {
            if openCode.isRunning {
                openCode.terminate()
                openCode.waitUntilExit()
            }
        }

        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )
        controller.bootstrapInstalledApplication()
        scheduler.runNext()
        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "2"))

        XCTAssertTrue(openCode.isRunning)
        XCTAssertEqual(Darwin.kill(openCode.processIdentifier, 0), 0)
    }

    func testLatePreviousBuildResponseCannotCommitThePendingUpgrade() {
        // P1-3: Service Management may keep the previous build answering IPC
        // while already accepting the new registration. A status response
        // from the old build must not commit the pending transaction.
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )
        controller.bootstrapInstalledApplication()
        scheduler.runNext()
        XCTAssertEqual(registrationTransaction()?.phase, .awaitingIPC)
        XCTAssertEqual(registrationTransaction()?.attempt, 0)

        // The previous build (identity "1") answers: not a proof of build 2.
        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "1"))
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertEqual(registrationTransaction()?.phase, .awaitingIPC)
        XCTAssertEqual(registrationTransaction()?.attempt, 0)

        // The new build proves itself: the transaction commits.
        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "2"))
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "2")
        XCTAssertNil(registrationTransaction())
    }

    func testOnlyStaleBuildAnswersExhaustTheBoundedRetriesWithoutCommitting() {
        // P1-3: when every answering build fails to prove the pending
        // version, the bounded transaction exhausts and nothing commits.
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()
        // The stale build keeps answering while the checks and retries run.
        for _ in 0 ..< 200 {
            controller.observeOpenCodeServerAgentReachability(agentStatus(build: "1"))
            scheduler.runNext()
        }

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 3)
        XCTAssertEqual(openCodeServerAgent.registerCount, 3)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertNil(registrationTransaction())
    }

    func testInteractiveSystemUsesShortIPCVerificationWindow() {
        // On an interactive system (uptime >= 10 minutes) a failed
        // verification window costs 6 x 2s, sized so attempt 2's register
        // lands after the ~10s BTM invalidation that follows attempt 1's
        // register (ADR 0006, 2026-08-03 addendum): attempt 2 unregisters
        // after settle + 6 checks + retry scheduling = 8 scheduler turns.
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler,
            systemUptime: { 3600 }
        )

        controller.bootstrapInstalledApplication()
        scheduler.runNext() // settle -> register attempt 1
        for _ in 0 ..< 6 {
            scheduler.runNext() // 6 verification checks, last one schedules the retry
        }
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)

        scheduler.runNext() // retry -> attempt 2 unregisters
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 2)
    }

    func testColdSystemKeepsLongIPCVerificationWindow() {
        // A cold system (uptime < 10 minutes, login-storm latency per
        // ADR 0012) keeps the full 15 x 2s window: attempt 2 unregisters
        // only after settle + 15 checks + retry scheduling = 17 turns.
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler,
            systemUptime: { 60 }
        )

        controller.bootstrapInstalledApplication()
        scheduler.runNext() // settle -> register attempt 1
        for _ in 0 ..< 15 {
            scheduler.runNext() // 15 verification checks
        }
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)

        scheduler.runNext() // retry -> attempt 2 unregisters
        XCTAssertEqual(openCodeServerAgent.unregisterCount, 2)
    }

    func testRegistrationVerificationPendingCallbackTracksTransaction() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )
        var events: [Bool] = []
        controller.onRegistrationVerificationPendingChange = { events.append($0) }

        controller.bootstrapInstalledApplication()
        scheduler.runNext() // settle -> register attempt 1 enters awaitingIPC
        XCTAssertEqual(events, [true])

        controller.observeOpenCodeServerAgentReachability(agentStatus(build: "2"))
        XCTAssertEqual(events, [true, false])
    }

    private func agentStatus(build bundleVersion: String) -> AgentStatus {
        makeAgentStatusForTest(bundleVersion: bundleVersion)
    }

    private func makeController(
        version: String,
        openCodeServerAgent: FakeAppService,
        scheduler: ManualServiceUpdateScheduler,
        systemUptime: @escaping () -> TimeInterval = { 3600 }
    ) -> ServiceController {
        makeServiceControllerForTest(
            defaults: defaults,
            version: version,
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler,
            systemUptime: systemUptime
        )
    }

    private func registrationTransaction()
        -> OpenCodeServerAgentRegistrationTransaction?
    {
        loadRegistrationTransactionForTest(defaults: defaults)
    }

    private func saveRegistrationTransaction(
        version: String,
        purpose: OpenCodeServerAgentRegistrationPurpose,
        phase: OpenCodeServerAgentRegistrationTransactionPhase,
        attempt: Int
    ) {
        saveRegistrationTransactionForTest(
            defaults: defaults,
            version: version,
            purpose: purpose,
            phase: phase,
            attempt: attempt
        )
    }
}
