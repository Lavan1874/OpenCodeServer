import AppKit
import OSLog

@main
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let logger = Logger(subsystem: "ai.opencode.server", category: "ui")
    private let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let menu = NSMenu()
    private let paths: ApplicationPaths
    private let configStore: ConfigStore
    private let client: IPCClient
    private let credentialMutations: CredentialMutationCoordinator
    private let services = ServiceController()
    private let notifications = NotificationController()

    private var subscription: AgentStatusSubscription?
    private var verificationPollTimer: Timer?
    private var uptimeTimer: Timer?
    private var verificationPollingActive = false
    private var keychainAccessReminderSent = false
    /// Rate-limits the "poll succeeded during pending verification" notice to
    /// one entry per transaction.
    private var verificationPollSuccessLogged = false
    private var refreshInFlight = false
    /// Wall-clock time of the latest pushed subscription status. One-shot
    /// poll and command responses requested before it are stale and must not
    /// overwrite the pushed state (see shouldApplyPolledStatus).
    private var latestPushReceivedAt = Date.distantPast
    private(set) var currentStatus: AgentStatus?
    private var stopAndQuitPending = false
    private var stopTimeoutAlertVisible = false

    private let openCodeItem = NSMenuItem(title: "OpenCode: Checking…", action: nil, keyEquivalent: "")
    private let uptimeItem = NSMenuItem(title: "Uptime: —", action: nil, keyEquivalent: "")
    private let openCodeServerAgentItem = NSMenuItem(
        title: "OpenCodeServerAgent: Checking…",
        action: nil,
        keyEquivalent: ""
    )
    private let fdaItem = NSMenuItem(title: "Full Disk Access: —", action: nil, keyEquivalent: "")
    private let versionItem = NSMenuItem(title: "OpenCode: —", action: nil, keyEquivalent: "")
    private let endpointItem = NSMenuItem(title: "Listening: —", action: nil, keyEquivalent: "")
    private let passwordItem = NSMenuItem(title: "Password: Not configured", action: nil, keyEquivalent: "")
    private let authenticationItem = NSMenuItem(title: "Authentication: —", action: nil, keyEquivalent: "")
    private let configurationItem = NSMenuItem(title: "Configuration: Active", action: nil, keyEquivalent: "")
    private let detailItem = NSMenuItem(title: "", action: nil, keyEquivalent: "")

    private lazy var startItem = actionItem("Start OpenCode", #selector(startOpenCode))
    private lazy var stopItem = actionItem("Stop OpenCode…", #selector(stopOpenCode))
    private lazy var restartItem = actionItem("Restart OpenCode…", #selector(restartOpenCode))
    private lazy var continueStopItem = actionItem("Continue Waiting", #selector(continueStopping))
    private lazy var forceStopItem = actionItem("Force Stop…", #selector(forceStop))
    /// Set when the user picks "Allow & Restart": the restart fires on
    /// the status push that reports the completed re-authorization. Kept in
    /// AppDelegate (not the Settings window) so closing the window while the
    /// system consent dialog is open does not strand the pending restart.
    private var restartAfterGrantPending = false

    /// Created on first use from the menu; kept as an optional (not `lazy`)
    /// so status updates can refresh the window only when it actually exists.
    private var settingsController: SettingsWindowController?

    private func makeSettingsController() -> SettingsWindowController {
        SettingsWindowController(
            configStore: configStore,
            services: services,
            statusProvider: { [weak self] in self?.currentStatus },
            credentialMutations: credentialMutations,
            credentialAuthorizationPerformer: { [weak self] in
                self?.requestCredentialAuthorization()
            },
            restartPerformer: { [weak self] in self?.requestRestart() },
            authorizeAndRestartPerformer: { [weak self] in self?.authorizeAndRestart() },
            didSave: { [weak self] in self?.refreshStatus() }
        )
    }

    /// The "Allow & Restart" path: one click raises the system Keychain
    /// consent prompt (the only deliberately interactive read), and the
    /// restart follows automatically on the status push that reports the
    /// grant — the user never has to know a separate Grant button exists.
    private func authorizeAndRestart() {
        let accepted = credentialMutations.performAfterAcknowledgement { [weak self] in
            guard let self else { return }
            self.restartAfterGrantPending = true
            self.sendCommand(.refreshCredentials)
        }
        guard accepted else {
            showCredentialMutationUnavailable()
            return
        }
    }

    private func requestCredentialAuthorization() {
        let accepted = credentialMutations.performAfterAcknowledgement { [weak self] in
            self?.sendCommand(.refreshCredentials)
        }
        guard accepted else {
            showCredentialMutationUnavailable()
            return
        }
    }

    private func requestRestart() {
        let accepted = credentialMutations.performAfterAcknowledgement { [weak self] in
            self?.sendCommand(.restart, showFailure: true)
        }
        guard accepted else {
            showCredentialMutationUnavailable()
            return
        }
    }

    private func showCredentialMutationUnavailable() {
        showError(
            credentialMutations.availability.detail
                ?? "Credential mutation state is unavailable. Open Settings and click Retry."
        )
    }

    override init() {
        let discovered: ApplicationPaths
        do {
            discovered = try ApplicationPaths.discover()
        } catch {
            fatalError("Unable to initialize application paths: \(error)")
        }
        paths = discovered
        configStore = ConfigStore(paths: discovered)
        let ipcClient = IPCClient(paths: discovered)
        client = ipcClient
        do {
            credentialMutations = try CredentialMutationCoordinator(
                fileURL: discovered.credentialMutationFile,
                sender: { try ipcClient.send($0) }
            )
        } catch {
            // A corrupt or unsafe journal is durable evidence that must not be
            // replaced during startup. Keep the GUI available so it can show
            // the problem and offer an explicit retry instead of crashing.
            credentialMutations = CredentialMutationCoordinator.unavailable(
                fileURL: discovered.credentialMutationFile,
                sender: { try ipcClient.send($0) },
                error: error
            )
        }
        super.init()
        credentialMutations.onAcknowledgedStatus = { [weak self] status in
            self?.applyStatus(status)
        }
        credentialMutations.onPendingStateChange = { [weak self] in
            guard let self else { return }
            self.applyStatus(self.currentStatus)
        }
    }

    func applicationDidFinishLaunching(_: Notification) {
        let environment = ProcessInfo.processInfo.environment
        if environment["OPENCODESERVER_TESTING"] == "1"
            || environment["XCTestConfigurationFilePath"] != nil
        {
            return
        }
        NSApp.setActivationPolicy(.accessory)
        do {
            try configStore.ensureDefault()
        } catch {
            logger.error("Unable to create default configuration: \(error.localizedDescription, privacy: .public)")
        }
        recoverCredentialMigration()
        configureStatusItem()
        buildMenu()
        services.onRegistrationVerificationPendingChange = { [weak self] pending in
            self?.setRegistrationVerificationPolling(pending)
        }
        services.bootstrapInstalledApplication()
        notifications.explainAndRequestIfNeeded()
        // One immediate poll so the first paint does not wait for the
        // subscription handshake; afterwards pushed status replaces polling.
        refreshStatus()
        let subscription = AgentStatusSubscription(socketPath: paths.controlSocket.path)
        subscription.onStatus = { [weak self] status in
            DispatchQueue.main.async {
                guard let self else { return }
                self.latestPushReceivedAt = Date()
                self.applyStatus(status)
            }
        }
        subscription.onUnreachable = { [weak self] in
            DispatchQueue.main.async {
                self?.applyStatus(nil)
            }
        }
        subscription.start()
        self.subscription = subscription
        // First-launch setup (HIG Onboarding: give people the setup option
        // during first run instead of leaving them in front of a gray icon):
        // open Settings exactly once, on the very first launch only.
        let defaults = UserDefaults.standard
        if !defaults.bool(forKey: "ai.opencode.server.didPresentInitialSettings") {
            defaults.set(true, forKey: "ai.opencode.server.didPresentInitialSettings")
            showSettings(nil)
        }
    }

    /// Resolves a durable username-migration intent without decrypting a
    /// Keychain item. Both configuration loading and attribute-only probes
    /// stay off the AppKit main thread; an uncertain read leaves the intent
    /// in place for a later retry.
    private func recoverCredentialMigration() {
        let configurationPaths = paths
        let started = credentialMutations.recoverMigration(
            currentConfiguration: {
                try ConfigStore(paths: configurationPaths).load()
            },
            contains: { try KeychainStore.contains(account: $0) },
            delete: { try KeychainStore.deleteWithoutInteraction(account: $0) }
        )
        if !started {
            logger.error(
                "Credential migration recovery is unavailable because its journal could not be read safely"
            )
        }
    }

    func applicationWillTerminate(_: Notification) {
        subscription?.invalidate()
        verificationPollTimer?.invalidate()
        uptimeTimer?.invalidate()
    }

    func menuWillOpen(_: NSMenu) {
        sendCommand(.refreshFda, showFailure: false)
        uptimeTimer?.invalidate()
        uptimeTimer = scheduleMenuSafeTimer(interval: 1) { [weak self] _ in
            guard let self else { return }
            self.uptimeItem.title = self.uptimeTitle(for: self.currentStatus)
        }
    }

    func menuDidClose(_: NSMenu) {
        uptimeTimer?.invalidate()
        uptimeTimer = nil
    }

    private func configureStatusItem() {
        guard let button = statusItem.button else { return }
        button.image = statusSymbol(for: .gray)
        button.imagePosition = .imageOnly
        button.setAccessibilityLabel("OpenCodeServer, checking status")
        menu.delegate = self
        statusItem.menu = menu
    }

    private func buildMenu() {
        // Item availability is managed explicitly in applyStatus; automatic
        // validation would re-enable action items on every menu open (the
        // responder chain implements their actions) and fight applyStatus,
        // visibly flickering items whose correct state is disabled.
        menu.autoenablesItems = false
        // Progressive disclosure (docs/references/nng-progressive-disclosure.md):
        // the steady-state menu shows only health, uptime, endpoint, and
        // version; every other status row appears only while it deviates from
        // its nominal value (see statusRowVisibility). Per HIG menu-bar
        // guidance the ACTION set never changes — actions are disabled, not
        // hidden; only informational rows are conditional, matching the
        // platform's own menu bar extras.
        for item in [
            openCodeItem, uptimeItem, endpointItem, versionItem,
            openCodeServerAgentItem, fdaItem, passwordItem, authenticationItem,
            configurationItem, detailItem
        ] {
            item.isEnabled = false
            menu.addItem(item)
        }
        menu.addItem(.separator())
        menu.addItem(startItem)
        menu.addItem(stopItem)
        menu.addItem(restartItem)
        menu.addItem(continueStopItem)
        menu.addItem(forceStopItem)
        menu.addItem(.separator())
        menu.addItem(actionItem("Settings…", #selector(showSettings(_:))))
        // Rarely needed recovery and inspection actions live one level down
        // (HIG: use a submenu to shorten a long menu; keep it a single level
        // and within about five items).
        let advancedItem = NSMenuItem(title: "Advanced", action: nil, keyEquivalent: "")
        let advancedMenu = NSMenu()
        advancedMenu.autoenablesItems = false
        advancedMenu.addItem(actionItem("Open Logs", #selector(openLogs)))
        advancedMenu.addItem(actionItem("Recheck Full Disk Access", #selector(recheckFDA)))
        advancedMenu.addItem(actionItem("Open Full Disk Access Settings", #selector(openFDASettings)))
        advancedMenu.addItem(actionItem("Open Login Items Settings", #selector(openBackgroundSettings)))
        advancedMenu.addItem(
            actionItem("Repair OpenCodeServerAgent…", #selector(repairOpenCodeServerAgent))
        )
        advancedItem.submenu = advancedMenu
        menu.addItem(advancedItem)
        menu.addItem(.separator())
        menu.addItem(actionItem("Quit OpenCodeServer", #selector(quitOpenCodeServer)))
        menu.addItem(
            actionItem(
                "Stop OpenCode and Quit OpenCodeServer…",
                #selector(stopOpenCodeAndQuitOpenCodeServer)
            )
        )
        detailItem.isHidden = true
        // These two apply only to the transient stop-timed-out state. Per the
        // HIG the menu shows the same set of items at all times, so they stay
        // visible and are merely disabled outside that state.
        continueStopItem.isEnabled = false
        forceStopItem.isEnabled = false
    }

    /// Which conditional status rows a status warrants. Nominal values are
    /// invisible in a healthy steady state (progressive disclosure); an
    /// unreachable agent is unknown rather than "all is well", so every row
    /// stays visible then. Kept static for tests.
    static func statusRowVisibility(
        status: AgentStatus?,
        openCodeServerAgentIsNominal: Bool
    ) -> StatusRowVisibility {
        guard let status else {
            return StatusRowVisibility(
                openCodeServerAgent: true,
                fda: true,
                password: true,
                authentication: true,
                configuration: true
            )
        }
        let endpoint = status.endpoint
        let endpointHost: String
        if endpoint.hasPrefix("["), let closingBracket = endpoint.firstIndex(of: "]") {
            let hostStart = endpoint.index(after: endpoint.startIndex)
            endpointHost = String(endpoint[hostStart..<closingBracket])
        } else if let portSeparator = endpoint.lastIndex(of: ":") {
            endpointHost = String(endpoint[..<portSeparator])
        } else {
            endpointHost = endpoint
        }
        let endpointIsLoopback = ConfigStore.isLoopback(endpointHost)
        return StatusRowVisibility(
            openCodeServerAgent: !openCodeServerAgentIsNominal,
            fda: status.fda != .verified,
            password: status.passwordState == .accessPending,
            // "Not enabled" only matters as a warning on a network listener;
            // an unauthenticated loopback endpoint is the documented default.
            authentication: status.authenticationEnabled != true && !endpointIsLoopback,
            configuration: status.configPending || status.configError != nil
        )
    }

    /// Maps the single OpenCodeServerAgent-owned action capability value to
    /// the fixed menu action set. The credential mutation acknowledgement
    /// remains a local GUI gate: until OpenCodeServerAgent has acknowledged a just-saved
    /// Keychain change, Start and Restart must stay disabled even if the
    /// OpenCodeServerAgent snapshot itself is otherwise ready.
    static func menuActionCapabilities(
        status: AgentStatus?,
        credentialNoticeAcknowledged: Bool
    ) -> ActionCapabilities {
        guard let capabilities = status?.actionCapabilities else {
            return .unavailable
        }
        return ActionCapabilities(
            start: credentialNoticeAcknowledged && capabilities.start,
            stop: capabilities.stop,
            restart: credentialNoticeAcknowledged && capabilities.restart,
            continueStop: capabilities.continueStop,
            forceStop: capabilities.forceStop
        )
    }

    /// Maps the OpenCodeServerAgent-owned Restart capability, preserving the
    /// local credential-mutation acknowledgement gate. Kept static so the exact
    /// value assigned to `restartItem.isEnabled` is covered without invoking
    /// AppKit or notification side effects in tests.
    static func restartItemIsEnabled(
        status: AgentStatus?,
        credentialNoticeAcknowledged: Bool
    ) -> Bool {
        menuActionCapabilities(
            status: status,
            credentialNoticeAcknowledged: credentialNoticeAcknowledged
        ).restart
    }

    /// The pushed subscription (ADR 0010) is the freshest status channel: a
    /// one-shot response requested before the latest push carries older
    /// agent state, and applying it would briefly regress the menu until the
    /// next push. Kept static for tests.
    static func shouldApplyPolledStatus(
        requestedAt: Date,
        latestPushReceivedAt: Date
    ) -> Bool {
        requestedAt >= latestPushReceivedAt
    }

    func refreshStatus() {
        guard !refreshInFlight else { return }
        refreshInFlight = true
        let requestedAt = Date()
        let client = client
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let result = Result { try client.send(.status) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.refreshInFlight = false
                switch result {
                case let .success(response):
                    if self.verificationPollingActive, !self.verificationPollSuccessLogged {
                        self.verificationPollSuccessLogged = true
                        self.logger.notice(
                            "Status poll succeeded while registration verification was pending"
                        )
                    }
                    self.applyPolledStatus(response.status, requestedAt: requestedAt)
                case let .failure(error):
                    self.logger.debug(
                        "OpenCodeServerAgent status unavailable: \(error.localizedDescription, privacy: .public)"
                    )
                    self.applyPolledStatus(nil, requestedAt: requestedAt)
                }
            }
        }
    }

    /// Applies a one-shot poll/command response unless a pushed status has
    /// already rendered newer agent state since the request was sent.
    private func applyPolledStatus(_ status: AgentStatus?, requestedAt: Date) {
        guard Self.shouldApplyPolledStatus(
            requestedAt: requestedAt,
            latestPushReceivedAt: latestPushReceivedAt
        ) else { return }
        applyStatus(status)
    }

    /// Renders uptime between pushes. The pushed `uptimeSeconds` is a
    /// snapshot; while the menu is open the display advances locally from
    /// the process start anchor so no per-second push is needed (ADR 0010).
    private func uptimeTitle(for status: AgentStatus?) -> String {
        guard let status else { return "Uptime: —" }
        let seconds = status.processStartedAtUnixSeconds.map { startedAt in
            UInt64(max(0, Date().timeIntervalSince1970 - TimeInterval(startedAt)))
        } ?? status.uptimeSeconds
        return "Uptime: \(formatDuration(seconds))"
    }

    /// Creates a repeating timer registered in common run loop modes.
    /// `Timer.scheduledTimer` registers in the default mode only, and menu
    /// tracking spins the run loop in event-tracking mode, so such timers
    /// silently stop firing while the status menu is open.
    private func scheduleMenuSafeTimer(
        interval: TimeInterval,
        handler: @escaping @MainActor (Timer) -> Void
    ) -> Timer {
        let timer = Timer(timeInterval: interval, repeats: true) { timer in
            Task { @MainActor in
                handler(timer)
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        return timer
    }

    /// Tightens status refresh while a registration transaction is awaiting
    /// IPC verification: during that window the status subscription is still
    /// in its reconnect backoff after the upgrade replaced the socket, so
    /// poll once per second to notice the agent coming up as early as
    /// possible instead of waiting out the degraded reconnect delay.
    private func setRegistrationVerificationPolling(_ pending: Bool) {
        guard pending else {
            verificationPollTimer?.invalidate()
            verificationPollTimer = nil
            if verificationPollingActive {
                verificationPollingActive = false
                logger.notice(
                    "Registration transaction settled; stopped per-second status polling"
                )
            }
            return
        }
        // Each registration attempt re-enters the pending state; an already
        // running timer must survive so polling continues across attempts.
        guard !verificationPollingActive else { return }
        verificationPollingActive = true
        verificationPollSuccessLogged = false
        logger.notice(
            "Registration transaction pending IPC verification; polling status every second"
        )
        refreshStatus()
        verificationPollTimer = scheduleMenuSafeTimer(interval: 1) { [weak self] _ in
            self?.refreshStatus()
        }
    }

    private func applyStatus(_ status: AgentStatus?) {
        currentStatus = status
        credentialMutations.observe(status)
        settingsController?.refreshLiveStatus()
        services.observeOpenCodeServerAgentReachability(status)
        let presentation = StatusPresentation.from(status: status)
        statusItem.button?.image = statusSymbol(for: presentation.color)
        statusItem.button?.setAccessibilityLabel("OpenCodeServer, \(presentation.label)")

        openCodeItem.title = "OpenCode: \(presentation.label)"
        uptimeItem.title = uptimeTitle(for: status)
        let openCodeServerAgentLabel = services.openCodeServerAgentStatusLabel(
            openCodeServerAgentReachable: status != nil
        )
        openCodeServerAgentItem.title =
            "OpenCodeServerAgent: \(openCodeServerAgentLabel)"
        fdaItem.title = "Full Disk Access: \(fdaLabel(status?.fda))"
        versionItem.title = versionLabel(status)
        endpointItem.title = "Listening: \(status?.endpoint ?? "—")"
        passwordItem.title = passwordMenuLabel(status?.passwordState)
        authenticationItem.title = authenticationMenuLabel(status?.authenticationEnabled)
        configurationItem.title = configurationLabel(status)
        let visibility = Self.statusRowVisibility(
            status: status,
            openCodeServerAgentIsNominal: openCodeServerAgentLabel == "Enabled and Reachable"
        )
        openCodeServerAgentItem.isHidden = !visibility.openCodeServerAgent
        fdaItem.isHidden = !visibility.fda
        passwordItem.isHidden = !visibility.password
        authenticationItem.isHidden = !visibility.authentication
        configurationItem.isHidden = !visibility.configuration
        let detail = [
            credentialMutations.availability.detail,
            status?.configError,
            status?.lastError
        ]
        .compactMap { $0 }
        .first(where: { !$0.isEmpty })
        if let detail {
            detailItem.title = "Detail: \(detail)"
            detailItem.toolTip = detail
            detailItem.isHidden = false
        } else {
            detailItem.isHidden = true
        }

        let credentialNoticeAcknowledged = credentialMutations.availability.isAvailable &&
            !credentialMutations.hasUnacknowledgedMutation
        let actionCapabilities = Self.menuActionCapabilities(
            status: status,
            credentialNoticeAcknowledged: credentialNoticeAcknowledged
        )
        startItem.isEnabled = actionCapabilities.start
        stopItem.isEnabled = actionCapabilities.stop
        restartItem.isEnabled = actionCapabilities.restart
        continueStopItem.isEnabled = actionCapabilities.continueStop
        forceStopItem.isEnabled = actionCapabilities.forceStop
        if let event = status?.notification {
            notifications.deliver(event)
        }
        // One-shot nudge per access-pending episode; the flag re-arms as
        // soon as the credential state leaves accessPending again.
        if status?.passwordState == .accessPending {
            if !keychainAccessReminderSent {
                keychainAccessReminderSent = true
                notifications.deliverKeychainAccessReminder()
            }
        } else {
            keychainAccessReminderSent = false
        }
        // The "Allow & Restart" promise: the re-authorization completed,
        // so the restart the user already asked for fires now.
        if restartAfterGrantPending, status?.passwordState == .configured {
            restartAfterGrantPending = false
            sendCommand(.restart, showFailure: true)
        }
        advanceStopAndQuitIfNeeded(status)
    }

    @objc private func startOpenCode() {
        sendCommand(.start)
    }

    @objc private func stopOpenCode() {
        guard confirmInterruption(
            title: "Stop OpenCode?",
            detail: "Active OpenCode work may be interrupted. OpenCodeServerAgent will first request a graceful OpenCode stop.",
            actionTitle: "Stop"
        ) else { return }
        sendCommand(.stop)
    }

    @objc private func restartOpenCode() {
        guard confirmInterruption(
            title: "Restart OpenCode?",
            detail: "Active OpenCode work may be interrupted. Saved configuration will take effect after restart.",
            actionTitle: "Restart"
        ) else { return }
        // An explicit restart settles any pending "Allow & Restart"
        // promise; the pending grant (if any) must not restart twice.
        restartAfterGrantPending = false
        sendCommand(.restart)
    }

    @objc private func continueStopping() {
        sendCommand(.continueStop)
    }

    @objc private func forceStop() {
        let alert = Self.makeDestructiveConfirmationAlert(
            style: .critical,
            title: "Force Stop OpenCode?",
            detail: "This sends SIGKILL to the verified OpenCode process group. Unsaved work cannot finish gracefully.",
            actionTitle: "Force Stop"
        )
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        sendCommand(.forceStop)
    }

    @IBAction func showSettings(_: Any?) {
        if settingsController == nil {
            settingsController = makeSettingsController()
        }
        settingsController?.present()
        sendCommand(.refreshFda, showFailure: false)
    }

    @objc private func openLogs() {
        let consoleURL = URL(filePath: "/System/Applications/Utilities/Console.app")
        let configuration = NSWorkspace.OpenConfiguration()
        NSWorkspace.shared.openApplication(
            at: consoleURL,
            configuration: configuration
        ) { [weak self] _, error in
            if let error {
                DispatchQueue.main.async {
                    self?.showError(error.localizedDescription)
                }
            }
        }
    }

    @objc private func openFDASettings() {
        guard let url = URL(
            string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
        ) else { return }
        NSWorkspace.shared.open(url)
        sendCommand(.refreshFda, showFailure: false)
    }

    @objc private func recheckFDA() {
        sendCommand(.refreshFda)
    }

    @objc private func openBackgroundSettings() {
        services.openLoginItemsSettings()
    }

    @objc private func repairOpenCodeServerAgent() {
        guard confirmInterruption(
            title: "Repair OpenCodeServerAgent?",
            detail: "OpenCodeServer will explicitly unregister and re-register OpenCodeServerAgent. OpenCode remains independent and is not signaled.",
            actionTitle: "Repair"
        ) else { return }
        services.repairOpenCodeServerAgent { [weak self] error in
            if let error {
                self?.showError(error.localizedDescription)
            } else {
                self?.showInformation(
                    "OpenCodeServerAgent repair completed and authenticated IPC is reachable."
                )
            }
        }
    }

    @objc private func quitOpenCodeServer() {
        NSApp.terminate(nil)
    }

    @objc private func stopOpenCodeAndQuitOpenCodeServer() {
        guard confirmInterruption(
            title: "Stop OpenCode and Quit OpenCodeServer?",
            detail: "Active OpenCode work may be interrupted. OpenCodeServer will ask OpenCodeServerAgent to stop OpenCode, then unregister OpenCodeServerAgent.",
            actionTitle: "Stop and Quit"
        ) else { return }
        stopAndQuitPending = true
        if currentStatus?.serverState == .stopped {
            finishStopAndQuit()
        } else {
            sendCommand(.stop)
        }
    }

    private func sendCommand(
        _ command: AgentCommand,
        showFailure: Bool = true,
        completion: (() -> Void)? = nil
    ) {
        if command == .start || command == .restart {
            guard credentialMutations.availability.isAvailable else {
                if showFailure {
                    showError(
                        "OpenCodeServer cannot start OpenCode until the pending credential journal is safely read. \(credentialMutations.availability.detail ?? "")"
                    )
                }
                return
            }
        }
        if (command == .start || command == .restart),
           credentialMutations.hasUnacknowledgedMutation
        {
            if showFailure {
                showError(
                    "OpenCodeServerAgent must acknowledge the saved credential change before OpenCode can start."
                )
            }
            return
        }
        let requestedAt = Date()
        let client = client
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = Result { try client.send(command) }
            DispatchQueue.main.async {
                guard let self else { return }
                switch result {
                case let .success(response):
                    self.applyPolledStatus(response.status, requestedAt: requestedAt)
                    completion?()
                case let .failure(error):
                    if showFailure {
                        self.showError(error.localizedDescription)
                    }
                    self.refreshStatus()
                }
            }
        }
    }

    private func advanceStopAndQuitIfNeeded(_ status: AgentStatus?) {
        guard stopAndQuitPending, let status else { return }
        switch status.serverState {
        case .stopped:
            finishStopAndQuit()
        case .stopTimedOut where !stopTimeoutAlertVisible:
            stopTimeoutAlertVisible = true
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = "OpenCode is still running"
            alert.informativeText = "The graceful interval ended. You can continue waiting, explicitly force stop, or cancel quitting."
            alert.addButton(withTitle: "Continue Waiting")
            alert.addButton(withTitle: "Force Stop")
            alert.addButton(withTitle: "Cancel Quit")
            let response = alert.runModal()
            stopTimeoutAlertVisible = false
            if response == .alertFirstButtonReturn {
                sendCommand(.continueStop)
            } else if response == .alertSecondButtonReturn {
                sendCommand(.forceStop)
            } else {
                stopAndQuitPending = false
            }
        default:
            break
        }
    }

    private func finishStopAndQuit() {
        guard services.openCodeServerAgentStatus == .enabled else {
            stopAndQuitPending = false
            showError(
                "OpenCode stopped, but OpenCodeServerAgent is not registered through OpenCodeServer and cannot be unregistered safely."
            )
            return
        }
        services.unregisterOpenCodeServerAgent { [weak self] error in
            guard let self else { return }
            if let error {
                self.stopAndQuitPending = false
                self.showError(error.localizedDescription)
            } else {
                NSApp.terminate(nil)
            }
        }
    }

    private func versionLabel(_ status: AgentStatus?) -> String {
        guard let status else { return "OpenCode: —" }
        let running = status.runningVersion ?? "unknown"
        let installed = status.installedVersion ?? "unknown"
        if status.versionPending {
            return "OpenCode: \(running) running, \(installed) installed — restart pending"
        }
        return "OpenCode: \(running)"
    }

    private func configurationLabel(_ status: AgentStatus?) -> String {
        guard let status else { return "Configuration: —" }
        if status.configError != nil {
            return status.serverState == .healthy
                ? "Configuration: Invalid changes; current OpenCode unaffected"
                : "Configuration: Invalid"
        }
        return status.configPending
            ? "Configuration: Restart pending"
            : "Configuration: Active"
    }

    private func fdaLabel(_ state: FDAState?) -> String {
        switch state {
        case .verified: "Verified"
        case .notVerified: "Not Verified"
        case .unableToDetermine: "Unable to Determine"
        case nil: "—"
        }
    }

    /// A destructive confirmation alert. HIG: the safe choice is the
    /// Return-key default for a destructive action, so Cancel takes the
    /// Return equivalent and the destructive button requires a deliberate
    /// click. Kept static so the keyboard contract is testable without
    /// running a modal.
    static func makeDestructiveConfirmationAlert(
        style: NSAlert.Style,
        title: String,
        detail: String,
        actionTitle: String
    ) -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = style
        alert.messageText = title
        alert.informativeText = detail
        alert.addButton(withTitle: actionTitle)
        alert.addButton(withTitle: "Cancel")
        alert.buttons[0].keyEquivalent = ""
        alert.buttons[1].keyEquivalent = "\r"
        return alert
    }

    private func confirmInterruption(title: String, detail: String, actionTitle: String) -> Bool {
        let alert = Self.makeDestructiveConfirmationAlert(
            style: .warning,
            title: title,
            detail: detail,
            actionTitle: actionTitle
        )
        return alert.runModal() == .alertFirstButtonReturn
    }

    private func showError(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "OpenCodeServer"
        alert.informativeText = message
        alert.runModal()
    }

    private func showInformation(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.messageText = "OpenCodeServer"
        alert.informativeText = message
        alert.runModal()
    }

    private func actionItem(
        _ title: String,
        _ action: Selector,
        keyEquivalent: String = ""
    ) -> NSMenuItem {
        let item = NSMenuItem(
            title: title,
            action: action,
            keyEquivalent: keyEquivalent
        )
        item.target = self
        return item
    }

    /// One SF Symbol per health color: color alone must not communicate
    /// status (HIG), so every state also carries its own shape. The VoiceOver
    /// label on the button keeps stating the status in words. Kept static so
    /// tests can assert the mapping and symbol availability without AppKit.
    static func statusSymbolName(for color: StatusColor) -> String {
        switch color {
        case .green: "checkmark.circle.fill"
        case .yellow: "exclamationmark.triangle.fill"
        case .red: "xmark.octagon.fill"
        case .gray: "circle.fill"
        }
    }

    private func statusSymbol(for color: StatusColor) -> NSImage? {
        let configuration = NSImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
            .applying(NSImage.SymbolConfiguration(paletteColors: [nsColor(color)]))
        return NSImage(
            systemSymbolName: Self.statusSymbolName(for: color),
            accessibilityDescription: nil
        )?.withSymbolConfiguration(configuration)
    }

    private func nsColor(_ color: StatusColor) -> NSColor {
        switch color {
        case .green: .systemGreen
        case .yellow: .systemYellow
        case .red: .systemRed
        case .gray: .systemGray
        }
    }
}
