import AppKit
import OSLog
import ServiceManagement

@MainActor
final class SettingsWindowController: NSWindowController, NSWindowDelegate {
    let logger = Logger(subsystem: "ai.opencode.server", category: "settings")
    let configStore: ConfigStore
    let services: ServiceController
    let statusProvider: () -> AgentStatus?
    let credentialMutations: CredentialMutationCoordinator
    /// Retains the active ordered Keychain/configuration transaction until
    /// its worker callbacks finish. The Save flow itself remains UI-only.
    var credentialSaveTransaction: CredentialSaveTransaction?
    let credentialAuthorizationPerformer: () -> Void
    let copyPasswordToPasteboard: (String) -> Void
    let didSave: () -> Void
    let keychainContains: @Sendable (String) throws -> Bool
    let keychainLoad: @Sendable (String) throws -> String?
    let keychainCreate: @Sendable (String, String) throws -> KeychainStore.SaveOutcome
    let keychainUpdate: @Sendable (String, String, String) throws -> KeychainStore.SaveOutcome
    let keychainDelete: @Sendable (String) throws -> Void
    /// Performs an explicit OpenCode restart with failure alerts, offered
    /// after a configuration change is saved.
    let restartPerformer: () -> Void
    /// The "Allow & Restart" path: raises the system Keychain consent
    /// prompt and restarts OpenCode automatically once the grant lands.
    let authorizeAndRestartPerformer: () -> Void

    let hostnameField = NSTextField()
    let portField = NSTextField()
    let usernameField = NSTextField()
    let securePasswordField = NSSecureTextField()
    let plainPasswordField = NSTextField()
    let passwordStatusLabel = NSTextField(labelWithString: "Checking Keychain…")
    let credentialProgressIndicator = NSProgressIndicator()
    let showPasswordButton = NSButton(
        checkboxWithTitle: "Show",
        target: nil,
        action: nil
    )
    let editPasswordButton = NSButton(title: "Edit…", target: nil, action: nil)
    let copyPasswordButton = NSButton(title: "Copy", target: nil, action: nil)
    let removePasswordButton = NSButton(title: "Remove…", target: nil, action: nil)
    let agentAccessValueLabel = NSTextField(labelWithString: "—")
    let grantAccessButton = NSButton(title: "Allow Keychain Access…", target: nil, action: nil)
    /// The Keychain account represented by the current Settings state; an
    /// explicit Edit can move its credential to a new account on Save.
    var loadedAccount: String?
    /// The configuration the fields were loaded from, used to detect whether
    /// a Save actually changes anything that affects the running OpenCode.
    var loadedConfig: AppConfig?
    /// Opening Settings performs only an attribute-only existence probe. It
    /// never decrypts the credential and therefore never raises the legacy
    /// Keychain consent dialog. Decrypt-class reads are reserved for an
    /// explicit Edit or Copy action and always run off the main thread.
    enum CredentialEditorState {
        case checking
        case absent
        case stored
        case loading
        case editingExisting(original: String)
        case removalPending
        case unavailable
    }
    var credentialEditorState: CredentialEditorState = .checking
    /// Incremented for every probe or explicit read. Late asynchronous
    /// results that no longer belong to the visible account are discarded.
    var credentialOperationGeneration = 0
    /// A controller is retained by OpenCodeServer across window closes, so a
    /// credential operation also belongs to the particular presentation that
    /// started it. Closing the window invalidates this session before any
    /// late Keychain result can update the hidden controls.
    var credentialPresentationGeneration = 0
    var credentialPresentationIsActive = false
    let mdnsButton = NSButton(
        checkboxWithTitle: "Advertise with mDNS",
        target: nil,
        action: nil
    )
    let candidatePopup = NSPopUpButton()
    let executableField = NSTextField()
    let openCodeServerAgentLoginButton = NSButton(
        checkboxWithTitle: "Run OpenCodeServerAgent at login",
        target: nil,
        action: nil
    )
    let openCodeServerLoginButton = NSButton(
        checkboxWithTitle: "Open OpenCodeServer at login",
        target: nil,
        action: nil
    )
    let feedbackLabel = NSTextField(labelWithString: "")
    let saveButton = NSButton(title: "Save", target: nil, action: nil)
    /// Progressive disclosure in Settings: rarely changed fields (mDNS,
    /// executable selection) live behind an "Advanced" disclosure and the
    /// window resizes to fit. The area expands automatically when the loaded
    /// configuration actually uses one of those fields, so a non-default
    /// value is never invisible.
    let advancedDisclosureButton = NSButton(title: "Advanced", target: nil, action: nil)
    var advancedGrid: NSGridView?
    var rootStack: NSStackView?
    /// The Password row's second line. When a state shows no controls at
    /// all (checking/loading), the stack itself is hidden so the vertical
    /// spacing collapses and the single visible line centers in the row.
    var passwordControlsStack: NSStackView?
    /// The window centers itself on its very first presentation; afterwards
    /// it reopens where the user left it instead of jumping back to the
    /// center of the screen on every open.
    var hasPresentedOnce = false
    /// Calculated from the current system font and the widest semantic form
    /// state, not from a hand-tuned window width.
    var preferredContentWidth: CGFloat = 0
    /// What the green Save feedback should say, derived from live state on
    /// every status push — never a string patched after the fact. Guidance
    /// suffixes appear and disappear with the agent's credential state, so
    /// the text can never contradict the Agent access row above it.
    struct SaveFeedbackContext {
        var passwordStored: Bool
        var accountChanged: Bool
    }
    var saveFeedbackContext: SaveFeedbackContext?
    /// Set once the agent confirms it registered a saved change
    /// (`configPending == true`); the "restart to apply" advice retires only
    /// after that, when a later status reports the restart landed
    /// (`configPending == false`). The two-step observation keeps a fast
    /// status push from clearing the advice before the agent even noticed
    /// the save.
    var saveFeedbackSawConfigPending = false

    init(
        configStore: ConfigStore,
        services: ServiceController,
        statusProvider: @escaping () -> AgentStatus?,
        credentialMutations: CredentialMutationCoordinator,
        credentialAuthorizationPerformer: @escaping () -> Void,
        restartPerformer: @escaping () -> Void,
        authorizeAndRestartPerformer: @escaping () -> Void,
        didSave: @escaping () -> Void,
        keychainContains: @escaping @Sendable (String) throws -> Bool = {
            try KeychainStore.contains(account: $0)
        },
        keychainLoad: @escaping @Sendable (String) throws -> String? = {
            try KeychainStore.load(account: $0)
        },
        keychainCreate: @escaping @Sendable (String, String) throws -> KeychainStore.SaveOutcome = {
            try KeychainStore.create(account: $0, password: $1)
        },
        keychainUpdate: @escaping @Sendable (String, String, String) throws -> KeychainStore.SaveOutcome = {
            try KeychainStore.update(
                account: $0,
                password: $1,
                knownCurrentPassword: $2
            )
        },
        keychainDelete: @escaping @Sendable (String) throws -> Void = {
            try KeychainStore.delete(account: $0)
        },
        copyPasswordToPasteboard: @escaping (String) -> Void = { password in
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(password, forType: .string)
        }
    ) {
        self.configStore = configStore
        self.services = services
        self.statusProvider = statusProvider
        self.credentialMutations = credentialMutations
        self.credentialAuthorizationPerformer = credentialAuthorizationPerformer
        self.restartPerformer = restartPerformer
        self.authorizeAndRestartPerformer = authorizeAndRestartPerformer
        self.copyPasswordToPasteboard = copyPasswordToPasteboard
        self.didSave = didSave
        self.keychainContains = keychainContains
        self.keychainLoad = keychainLoad
        self.keychainCreate = keychainCreate
        self.keychainUpdate = keychainUpdate
        self.keychainDelete = keychainDelete
        let window = NSWindow(
            contentRect: .zero,
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "OpenCodeServer Settings"
        window.isReleasedWhenClosed = false
        window.tabbingMode = .disallowed
        // HIG: a Settings window is quick to reopen and sized for its current
        // pane, so its minimize and zoom controls are present but dimmed.
        window.standardWindowButton(.miniaturizeButton)?.isEnabled = false
        window.standardWindowButton(.zoomButton)?.isEnabled = false
        super.init(window: window)
        window.delegate = self
        buildInterface()
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    func present() {
        beginCredentialPresentationSession()
        reload()
        // A non-default advanced value must never be invisible: expand the
        // area automatically when the loaded configuration uses it.
        let usesAdvanced = loadedConfig.map { !$0.executablePath.isEmpty || $0.mdns } ?? false
        setAdvancedExpanded(usesAdvanced, resize: false)
        showWindow(nil)
        resizeWindowForContent()
        if !hasPresentedOnce {
            hasPresentedOnce = true
            window?.center()
        }
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        guard let closingWindow = notification.object as? NSWindow,
              closingWindow === window
        else { return }
        endCredentialPresentationSession()
    }

    private func reload() {
        do {
            let config = try configStore.load()
            hostnameField.stringValue = config.hostname
            portField.integerValue = config.port
            usernameField.stringValue = config.username
            let account = KeychainStore.account(forUsername: config.username)
            loadedAccount = account
            loadedConfig = config
            updateFeedbackText("")
            saveFeedbackContext = nil
            saveFeedbackSawConfigPending = false
            beginCredentialProbe(account: account)
            mdnsButton.state = config.mdns ? .on : .off
            executableField.stringValue = config.executablePath
            rebuildCandidates(selected: config.executablePath)
            openCodeServerAgentLoginButton.state =
                services.openCodeServerAgentStatus == .enabled ? .on : .off
            openCodeServerLoginButton.state =
                services.openCodeServerLoginStatus == .enabled ? .on : .off
            refreshAccessRow()
        } catch {
            updateFeedbackText(error.localizedDescription)
        }
    }

    @objc func cancel() {
        close()
    }

    func showError(_ message: String) {
        saveFeedbackContext = nil
        feedbackLabel.textColor = .systemRed
        updateFeedbackText(message)
    }

    /// Feedback is progressive content, so its wrapped height can change at
    /// runtime. Refit only when the text actually changes and the window is
    /// visible; pre-presentation changes are covered by `present()`'s normal
    /// sizing pass. The content width remains stable.
    func updateFeedbackText(_ text: String) {
        guard feedbackLabel.stringValue != text else { return }
        feedbackLabel.stringValue = text
        if window?.isVisible == true {
            resizeWindowForContent()
        }
    }
}
