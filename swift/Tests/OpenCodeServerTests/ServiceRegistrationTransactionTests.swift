@testable import OpenCodeServer
import Foundation
import XCTest

@MainActor
final class ServiceRegistrationTransactionTests: XCTestCase {
    private var defaults: UserDefaults!
    private var defaultsSuite: String!

    override func setUp() {
        super.setUp()
        defaultsSuite = "ai.opencode.server.transaction-tests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: defaultsSuite)
        defaults.removePersistentDomain(forName: defaultsSuite)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: defaultsSuite)
        defaults = nil
        defaultsSuite = nil
        super.tearDown()
    }

    func testBundleUpgradePersistsIntentBeforeCallingUnregister() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let openCodeServerAgent = FakeAppService(status: .enabled)
        openCodeServerAgent.completesUnregisterImmediately = false
        var transactionAtUnregister: OpenCodeServerAgentRegistrationTransaction?
        openCodeServerAgent.onUnregister = {
            let freshDefaults = UserDefaults(suiteName: self.defaultsSuite)!
            guard case let .valid(transaction) =
                OpenCodeServerAgentRegistrationTransactionStore(defaults: freshDefaults).load()
            else {
                return
            }
            transactionAtUnregister = transaction
        }
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: ManualServiceUpdateScheduler()
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(
            transactionAtUnregister,
            OpenCodeServerAgentRegistrationTransaction(
                version: "2",
                purpose: .bundleUpgrade,
                phase: .unregistering,
                attempt: 0
            )
        )
    }

    func testRestartAfterNotRegisteredObservationResumesBeforeRegistering() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        let firstOpenCodeServerAgent = FakeAppService(status: .enabled)
        let firstScheduler = ManualServiceUpdateScheduler()
        let firstController = makeController(
            version: "2",
            openCodeServerAgent: firstOpenCodeServerAgent,
            scheduler: firstScheduler
        )

        firstController.bootstrapInstalledApplication()

        XCTAssertEqual(firstOpenCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(firstOpenCodeServerAgent.registerCount, 0)
        XCTAssertEqual(
            registrationTransaction(),
            OpenCodeServerAgentRegistrationTransaction(
                version: "2",
                purpose: .bundleUpgrade,
                phase: .awaitingRegistration,
                attempt: 0
            )
        )

        let nextOpenCodeServerAgent = FakeAppService(status: .notRegistered)
        let nextScheduler = ManualServiceUpdateScheduler()
        let nextController = makeController(
            version: "2",
            openCodeServerAgent: nextOpenCodeServerAgent,
            scheduler: nextScheduler
        )

        nextController.bootstrapInstalledApplication()
        XCTAssertEqual(nextOpenCodeServerAgent.registerCount, 0)
        nextScheduler.runNext()

        XCTAssertEqual(nextOpenCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(nextOpenCodeServerAgent.registerCount, 1)
        XCTAssertEqual(registrationTransaction()?.purpose, .bundleUpgrade)
        XCTAssertEqual(registrationTransaction()?.attempt, 0)
        XCTAssertEqual(registrationTransaction()?.phase, .awaitingIPC)
    }

    func testRestartAfterUnregisterCrashWithNotRegisteredStatusPreservesPurposeAndAttempt() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        saveRegistrationTransaction(
            version: "2",
            purpose: .bundleUpgrade,
            phase: .unregistering,
            attempt: 1
        )
        let openCodeServerAgent = FakeAppService(status: .notRegistered)
        let scheduler = ManualServiceUpdateScheduler()
        let restartedDefaults = UserDefaults(suiteName: defaultsSuite)!
        let controller = makeServiceControllerForTest(
            defaults: restartedDefaults,
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(
            loadRegistrationTransactionForTest(defaults: restartedDefaults),
            OpenCodeServerAgentRegistrationTransaction(
                version: "2",
                purpose: .bundleUpgrade,
                phase: .awaitingRegistration,
                attempt: 1
            )
        )
        scheduler.runNext()

        XCTAssertEqual(openCodeServerAgent.registerCount, 1)
        XCTAssertEqual(
            loadRegistrationTransactionForTest(defaults: restartedDefaults)?.purpose,
            .bundleUpgrade
        )
        XCTAssertEqual(
            loadRegistrationTransactionForTest(defaults: restartedDefaults)?.attempt,
            1
        )
        XCTAssertEqual(
            loadRegistrationTransactionForTest(defaults: restartedDefaults)?.phase,
            .awaitingIPC
        )
    }

    func testRetryScheduledTransactionResumesAtPersistedAttemptAfterRestart() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        saveRegistrationTransaction(
            version: "2",
            purpose: .bundleUpgrade,
            phase: .retryScheduled,
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

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(registrationTransaction()?.attempt, 1)
        XCTAssertEqual(registrationTransaction()?.phase, .awaitingRegistration)
        scheduler.runAll()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 2)
        XCTAssertEqual(openCodeServerAgent.registerCount, 2)
        XCTAssertEqual(defaults.string(forKey: "RegisteredBundleVersion"), "1")
        XCTAssertNil(registrationTransaction())
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testInvalidCurrentVersionTransactionFailsClosedWithoutResettingBudget() {
        defaults.set("2", forKey: "RegisteredBundleVersion")
        saveRegistrationTransaction(
            version: "2",
            purpose: .bundleUpgrade,
            phase: .awaitingIPC,
            attempt: 3
        )
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(registrationTransaction()?.attempt, 3)
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testWronglyTypedTransactionKeyFailsClosedInsteadOfStartingAttemptZero() {
        defaults.set("2", forKey: "RegisteredBundleVersion")
        defaults.set(
            "not-a-transaction",
            forKey: OpenCodeServerAgentRegistrationTransactionStore.defaultsKey
        )
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "2",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(openCodeServerAgent.registerCount, 0)
        XCTAssertEqual(
            defaults.string(
                forKey: OpenCodeServerAgentRegistrationTransactionStore.defaultsKey
            ),
            "not-a-transaction"
        )
        XCTAssertFalse(scheduler.hasPendingActions)
    }

    func testStaleTransactionStartsCurrentBundleUpgradeTransaction() {
        defaults.set("1", forKey: "RegisteredBundleVersion")
        saveRegistrationTransaction(
            version: "2",
            purpose: .bundleUpgrade,
            phase: .awaitingIPC,
            attempt: 2
        )
        let openCodeServerAgent = FakeAppService(status: .enabled)
        let scheduler = ManualServiceUpdateScheduler()
        let controller = makeController(
            version: "3",
            openCodeServerAgent: openCodeServerAgent,
            scheduler: scheduler
        )

        controller.bootstrapInstalledApplication()

        XCTAssertEqual(openCodeServerAgent.unregisterCount, 1)
        XCTAssertEqual(
            registrationTransaction(),
            OpenCodeServerAgentRegistrationTransaction(
                version: "3",
                purpose: .bundleUpgrade,
                phase: .awaitingRegistration,
                attempt: 0
            )
        )
    }

    func testSameVersionExplicitRepairTransactionSurvivesRestartBeforeIPCVerification() {
        defaults.set("5", forKey: "RegisteredBundleVersion")
        let firstOpenCodeServerAgent = FakeAppService(status: .enabled)
        let firstScheduler = ManualServiceUpdateScheduler()
        let firstController = makeController(
            version: "5",
            openCodeServerAgent: firstOpenCodeServerAgent,
            scheduler: firstScheduler
        )

        firstController.bootstrapInstalledApplication()
        var completionCalled = false
        firstController.repairOpenCodeServerAgent { _ in
            completionCalled = true
        }
        firstScheduler.runNext()

        XCTAssertEqual(firstOpenCodeServerAgent.registerCount, 1)
        XCTAssertFalse(completionCalled)
        XCTAssertEqual(
            registrationTransaction(),
            OpenCodeServerAgentRegistrationTransaction(
                version: "5",
                purpose: .explicitRepair,
                phase: .awaitingIPC,
                attempt: 0
            )
        )

        let nextOpenCodeServerAgent = FakeAppService(status: .enabled)
        let nextController = makeController(
            version: "5",
            openCodeServerAgent: nextOpenCodeServerAgent,
            scheduler: ManualServiceUpdateScheduler()
        )
        nextController.bootstrapInstalledApplication()

        XCTAssertEqual(nextOpenCodeServerAgent.unregisterCount, 0)
        XCTAssertEqual(nextOpenCodeServerAgent.registerCount, 0)
        XCTAssertEqual(registrationTransaction()?.purpose, .explicitRepair)
        nextController.observeOpenCodeServerAgentReachability(
            makeAgentStatusForTest(bundleVersion: "5")
        )

        XCTAssertTrue(defaults.string(forKey: "RegisteredBundleVersion") == "5")
        XCTAssertNil(registrationTransaction())
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
