@testable import OpenCodeServer
import XCTest

@MainActor
final class MenuActionCapabilitiesTests: XCTestCase {
    func testMenuMappingPreservesAgentCapabilitiesForEachLifecycleState() {
        let cases: [(ServerState, ActionCapabilities)] = [
            (.stopped, capabilities(start: true, stop: false, restart: true)),
            (.healthy, capabilities(start: false, stop: true, restart: true)),
            (.unhealthy, capabilities(start: false, stop: true, restart: true)),
            (.starting, capabilities(start: false, stop: true, restart: true)),
            (.waitingToRestart, capabilities(start: true, stop: true, restart: true)),
            (.failed, capabilities(start: false, stop: true, restart: true)),
            // The graceful interval still applies while stopping. Force Stop
            // is offered only after OpenCodeServerAgent reports a timeout.
            (.stopping, capabilities(start: false, stop: true, restart: true)),
            (
                .stopTimedOut,
                capabilities(
                    start: false,
                    stop: true,
                    restart: true,
                    continueStop: true,
                    forceStop: true
                )
            )
        ]

        for (state, expected) in cases {
            XCTAssertEqual(
                AppDelegate.menuActionCapabilities(
                    status: makeStatus(serverState: state, actionCapabilities: expected),
                    credentialNoticeAcknowledged: true
                ),
                expected,
                "menu mapping must preserve OpenCodeServerAgent capabilities for \(state)"
            )
        }
    }

    func testMenuMappingKeepsAgentOwnedCredentialAndDurabilityGates() {
        let accessPending = makeStatus(
            passwordState: .accessPending,
            actionCapabilities: capabilities(start: false, stop: true, restart: false)
        )
        XCTAssertEqual(
            AppDelegate.menuActionCapabilities(
                status: accessPending,
                credentialNoticeAcknowledged: true
            ),
            accessPending.actionCapabilities
        )

        XCTAssertEqual(
            AppDelegate.menuActionCapabilities(
                status: makeStatus(
                    serverState: .failed,
                    actionCapabilities: .unavailable
                ),
                credentialNoticeAcknowledged: true
            ),
            .unavailable,
            "launch transaction and durability gates are OpenCodeServerAgent-owned"
        )
        XCTAssertEqual(
            AppDelegate.menuActionCapabilities(
                status: nil,
                credentialNoticeAcknowledged: true
            ),
            .unavailable,
            "an unreachable OpenCodeServerAgent disables the fixed action set"
        )
    }

    func testLocalCredentialMutationOnlyGatesStartAndRestart() {
        let capabilities = ActionCapabilities(
            start: true,
            stop: true,
            restart: true,
            continueStop: true,
            forceStop: true
        )
        XCTAssertEqual(
            AppDelegate.menuActionCapabilities(
                status: makeStatus(actionCapabilities: capabilities),
                credentialNoticeAcknowledged: false
            ),
            ActionCapabilities(
                start: false,
                stop: true,
                restart: false,
                continueStop: true,
                forceStop: true
            )
        )
    }

    private func capabilities(
        start: Bool,
        stop: Bool,
        restart: Bool,
        continueStop: Bool = false,
        forceStop: Bool = false
    ) -> ActionCapabilities {
        ActionCapabilities(
            start: start,
            stop: stop,
            restart: restart,
            continueStop: continueStop,
            forceStop: forceStop
        )
    }

    private func makeStatus(
        serverState: ServerState = .healthy,
        passwordState: PasswordState = .configured,
        actionCapabilities: ActionCapabilities
    ) -> AgentStatus {
        AgentStatus(
            protocolVersion: ipcProtocolVersion,
            agentVersion: "test",
            agentUptimeSeconds: 0,
            desiredState: .running,
            serverState: serverState,
            health: .healthy,
            fda: .verified,
            uptimeSeconds: 60,
            endpoint: "127.0.0.1:4096",
            username: "opencode",
            passwordState: passwordState,
            authenticationEnabled: true,
            actionCapabilities: actionCapabilities,
            installedVersion: "1.0.0",
            runningVersion: "1.0.0",
            versionPending: false,
            configPending: false,
            configError: nil,
            lastError: nil,
            pid: 1234,
            stopGraceRemainingSeconds: nil,
            notification: nil,
            processStartedAtUnixSeconds: nil,
            bundleVersion: "1"
        )
    }
}
