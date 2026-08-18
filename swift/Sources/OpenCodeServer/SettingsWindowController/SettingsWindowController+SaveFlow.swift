import AppKit
import ServiceManagement

extension SettingsWindowController {
    private struct CredentialSavePlan: Sendable {
        let mutation: CredentialMutation
        let passwordStored: Bool
        let passwordPresenceKnown: Bool
        let accountChanged: Bool
        let credentialChanged: Bool
    }

    @objc func save() {
        guard credentialSaveTransaction == nil else {
            showError("Wait for the current credential save to finish, then save again.")
            return
        }
        guard credentialMutations.availability.isAvailable else {
            let detail = credentialMutations.availability.detail
                ?? "The pending credential mutation state is unavailable."
            showError(
                "Credential changes are disabled until the pending journal is safely read. \(detail) Click Retry in Agent access and try again."
            )
            return
        }
        guard !credentialMutations.recoveryInFlight else {
            showError("Wait for credential recovery to finish, then save again.")
            return
        }
        let config = AppConfig(
            schemaVersion: 1,
            hostname: hostnameField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            port: portField.integerValue,
            username: usernameField.stringValue,
            mdns: mdnsButton.state == .on,
            executablePath: executableField.stringValue
                .trimmingCharacters(in: .whitespacesAndNewlines)
        )
        let settingsChanged = loadedConfig != nil && loadedConfig != config
        let account = KeychainStore.account(forUsername: config.username)
        let accountChanged = loadedAccount != nil && loadedAccount != account
        let plan: CredentialSavePlan
        switch credentialEditorState {
        case .loading:
            showError("Wait for the current Keychain operation to finish, then save again.")
            return
        case .checking, .unavailable:
            guard !accountChanged else {
                showError("Wait for or retry the Keychain check before changing the username.")
                return
            }
            plan = CredentialSavePlan(
                mutation: .none,
                passwordStored: statusProvider()?.passwordState != .notConfigured,
                passwordPresenceKnown: false,
                accountChanged: false,
                credentialChanged: false
            )
        case .stored:
            guard !accountChanged else {
                showError(
                    "To change the username and keep its password, click Edit… first. This makes the Keychain access explicit."
                )
                return
            }
            plan = CredentialSavePlan(
                mutation: .none,
                passwordStored: true,
                passwordPresenceKnown: true,
                accountChanged: false,
                credentialChanged: false
            )
        case .absent:
            let password = currentPassword()
            plan = CredentialSavePlan(
                mutation: password.isEmpty
                    ? .none
                    : .create(
                        account: account,
                        password: password,
                        // `.absent` is an attribute-only proof that there is
                        // no old item to preserve. Even when the username
                        // changes, this is a first create: save the new
                        // configuration through the regular short transaction
                        // and retain the accountChanged UI semantics below.
                        oldAccount: nil
                    ),
                passwordStored: !password.isEmpty,
                passwordPresenceKnown: true,
                accountChanged: accountChanged,
                credentialChanged: !password.isEmpty
            )
        case .editingExisting(let original):
            let password = currentPassword()
            guard !password.isEmpty else {
                showError("To remove the saved password, click Remove… instead of leaving it blank.")
                return
            }
            let mutation: CredentialMutation
            if accountChanged {
                mutation = .create(
                    account: account,
                    password: password,
                    oldAccount: loadedAccount
                )
            } else if password == original {
                mutation = .none
            } else {
                mutation = .update(
                    account: account,
                    password: password,
                    original: original
                )
            }
            plan = CredentialSavePlan(
                mutation: mutation,
                passwordStored: true,
                passwordPresenceKnown: true,
                accountChanged: accountChanged,
                credentialChanged: accountChanged || password != original
            )
        case .removalPending:
            guard let loadedAccount else {
                showError("The Keychain account is unavailable; reopen Settings and try again.")
                return
            }
            guard !accountChanged else {
                showError("Undo password removal before changing the username.")
                return
            }
            plan = CredentialSavePlan(
                mutation: .delete(account: loadedAccount),
                passwordStored: false,
                passwordPresenceKnown: true,
                accountChanged: accountChanged,
                credentialChanged: true
            )
        }

        if !ConfigStore.isLoopback(config.hostname),
           plan.passwordPresenceKnown,
           !plan.passwordStored
        {
            let warning = NSAlert()
            warning.alertStyle = .warning
            warning.messageText = "Save a network listener without authentication?"
            warning.informativeText = "Anyone who can reach this address may be able to control OpenCode. A password is strongly recommended for non-loopback addresses."
            warning.addButton(withTitle: "Save Anyway")
            warning.addButton(withTitle: "Cancel")
            guard warning.runModal() == .alertFirstButtonReturn else { return }
        }

        let wantsOpenCodeServerAgent = openCodeServerAgentLoginButton.state == .on
        if !wantsOpenCodeServerAgent,
           services.openCodeServerAgentStatus == .enabled,
           statusProvider()?.serverState != .stopped
        {
            showError("Stop OpenCode before disabling OpenCodeServerAgent.")
            openCodeServerAgentLoginButton.state = .on
            return
        }

        let serviceChoicesChanged =
            wantsOpenCodeServerAgent != (services.openCodeServerAgentStatus == .enabled) ||
            (openCodeServerLoginButton.state == .on) != (services.openCodeServerLoginStatus == .enabled)

        guard case .none = plan.mutation else {
            beginCredentialMutation(
                plan: plan,
                config: config,
                settingsChanged: settingsChanged,
                serviceChoicesChanged: serviceChoicesChanged,
                wantsOpenCodeServerAgent: wantsOpenCodeServerAgent
            )
            return
        }

        finishSave(
            config: config,
            settingsChanged: settingsChanged,
            serviceChoicesChanged: serviceChoicesChanged,
            wantsOpenCodeServerAgent: wantsOpenCodeServerAgent,
            plan: plan
        )
    }

    /// Starts the non-UI credential transaction. Its completion is delivered
    /// on the main actor after all Keychain work and journal boundaries are
    /// settled.
    private func beginCredentialMutation(
        plan: CredentialSavePlan,
        config: AppConfig,
        settingsChanged: Bool,
        serviceChoicesChanged: Bool,
        wantsOpenCodeServerAgent: Bool
    ) {
        saveButton.isEnabled = false
        feedbackLabel.textColor = .secondaryLabelColor
        updateFeedbackText("Saving…")
        let transaction = CredentialSaveTransaction(
            configStore: configStore,
            credentialMutations: credentialMutations,
            keychainCreate: keychainCreate,
            keychainUpdate: keychainUpdate,
            keychainDelete: keychainDelete,
            keychainContains: keychainContains
        )
        credentialSaveTransaction = transaction
        let presentationGeneration = credentialPresentationGeneration
        transaction.start(mutation: plan.mutation, config: config) { [weak self] result in
            guard let self else { return }
            self.credentialSaveTransaction = nil
            guard self.credentialPresentationGeneration == presentationGeneration
            else {
                // The durable transaction has completed (and released its
                // owner above), but a closed presentation must not repaint
                // hidden Settings controls or surface stale feedback.
                return
            }
            self.finishCredentialMutation(
                result: result,
                config: config,
                settingsChanged: settingsChanged,
                serviceChoicesChanged: serviceChoicesChanged,
                wantsOpenCodeServerAgent: wantsOpenCodeServerAgent,
                plan: plan
            )
        }
    }

    /// Completes a Save that has no credential mutation. AppKit and Service
    /// Management state remain main-thread confined; no Keychain operation
    /// runs here.
    private func finishSave(
        config: AppConfig,
        settingsChanged: Bool,
        serviceChoicesChanged: Bool,
        wantsOpenCodeServerAgent: Bool,
        plan: CredentialSavePlan
    ) {
        do {
            try configStore.save(config)
        } catch {
            saveButton.isEnabled = true
            showError(error.localizedDescription)
            return
        }
        recordSavedConfiguration(config, plan: plan)
        finishSavedConfiguration(
            outcome: .unchanged,
            settingsChanged: settingsChanged,
            serviceChoicesChanged: serviceChoicesChanged,
            wantsOpenCodeServerAgent: wantsOpenCodeServerAgent,
            plan: plan,
            warning: nil
        )
    }

    private func finishCredentialMutation(
        result: CredentialSaveTransactionResult,
        config: AppConfig,
        settingsChanged: Bool,
        serviceChoicesChanged: Bool,
        wantsOpenCodeServerAgent: Bool,
        plan: CredentialSavePlan
    ) {
        switch result {
        case .failed(let message, let configurationSaved):
            if configurationSaved {
                // A failed delete/update did not change the Keychain item.
                // Keep the editor state visible instead of presenting the
                // failed operation as an absent or newly stored credential.
                recordSavedConfiguration(
                    config,
                    plan: plan,
                    preserveCredentialState: true
                )
            }
            saveButton.isEnabled = true
            showError(message)
        case .succeeded(let outcome, let warning):
            recordSavedConfiguration(config, plan: plan)
            finishSavedConfiguration(
                outcome: outcome,
                settingsChanged: settingsChanged,
                serviceChoicesChanged: serviceChoicesChanged,
                wantsOpenCodeServerAgent: wantsOpenCodeServerAgent,
                plan: plan,
                warning: warning
            )
        }
    }

    private func recordSavedConfiguration(
        _ config: AppConfig,
        plan: CredentialSavePlan,
        preserveCredentialState: Bool = false
    ) {
        loadedAccount = KeychainStore.account(forUsername: config.username)
        loadedConfig = config
        if preserveCredentialState {
            // Keep the explicit Edit/removal state and any plaintext already
            // held in memory available for a retry. A failed Keychain action
            // must not clear the editor and silently turn into success.
            renderCredentialEditor()
            return
        }
        clearPasswordFields()
        if plan.passwordPresenceKnown {
            credentialEditorState = plan.passwordStored ? .stored : .absent
            renderCredentialEditor()
        } else if !plan.passwordPresenceKnown, let loadedAccount {
            beginCredentialProbe(account: loadedAccount)
        }
    }

    private func finishSavedConfiguration(
        outcome: KeychainStore.SaveOutcome,
        settingsChanged: Bool,
        serviceChoicesChanged: Bool,
        wantsOpenCodeServerAgent: Bool,
        plan: CredentialSavePlan,
        warning: String?
    ) {
        saveButton.isEnabled = true
        applyServiceChoices(
            wantsOpenCodeServerAgent: wantsOpenCodeServerAgent
        )
        if !settingsChanged, outcome == .unchanged, !plan.accountChanged,
           !serviceChoicesChanged
        {
            // A fully unchanged save must SAY so — "restart to apply"
            // advice when nothing changed is a lie (NN/g heuristic #1:
            // feedback has to reflect what actually happened).
            feedbackLabel.textColor = .secondaryLabelColor
            updateFeedbackText("No changes to save.")
            saveFeedbackContext = nil
            saveFeedbackSawConfigPending = false
            return
        }

        if let warning {
            feedbackLabel.textColor = .systemOrange
            updateFeedbackText(warning)
            saveFeedbackContext = nil
        } else {
            feedbackLabel.textColor = .systemGreen
            saveFeedbackContext = SaveFeedbackContext(
                passwordStored: plan.passwordStored,
                accountChanged: plan.accountChanged
            )
            renderSaveFeedback()
        }
        refreshAccessRow()
        didSave()
        let needsAuthorization = Self.needsCredentialAuthorization(
            passwordIsEmpty: !plan.passwordStored,
            outcome: outcome,
            passwordState: statusProvider()?.passwordState
        )
        offerRestartIfNeeded(
            settingsChanged: settingsChanged,
            credentialChanged: plan.credentialChanged,
            needsAuthorization: needsAuthorization
        )
    }

    /// Offers to apply a saved change immediately. OpenCode keeps running
    /// with the configuration it was started with until it restarts, and a
    /// deferred restart is exactly what strands an outdated process when the
    /// agent next re-registers (configuration fingerprint mismatch), so the
    /// product asks right away instead of hoping the user remembers. The
    /// choice stays with the user: a restart briefly interrupts sessions.
    ///
    /// When the new credential still needs Keychain authorization the
    /// primary button becomes “Allow & Restart”: one click raises the
    /// system consent prompt, and AppDelegate performs the restart
    /// automatically once the agent reports the grant — no second dialog,
    /// no orphaned “restart later” state.
    private func offerRestartIfNeeded(
        settingsChanged: Bool,
        credentialChanged: Bool,
        needsAuthorization: Bool
    ) {
        guard Self.shouldOfferRestart(
            settingsChanged: settingsChanged,
            credentialChanged: credentialChanged,
            openCodeIsRunning: statusProvider()?.pid != nil
        ) else { return }
        let alert = NSAlert()
        alert.alertStyle = .informational
        if needsAuthorization {
            alert.messageText = "Allow Keychain access and restart OpenCode?"
            alert.informativeText =
                "OpenCodeServerAgent needs permission to read the saved password. macOS may ask you to choose “Always Allow”. After access is granted, OpenCode restarts automatically."
            alert.addButton(withTitle: "Allow & Restart")
            alert.addButton(withTitle: "Later")
            if alert.runModal() == .alertFirstButtonReturn {
                authorizeAndRestartPerformer()
            }
        } else {
            alert.messageText = "Restart OpenCode to apply the changes?"
            alert.informativeText =
                "OpenCode is still running with the previous configuration. Restarting briefly interrupts active sessions; you can also choose “Restart OpenCode…” from the menu later."
            alert.addButton(withTitle: "Restart OpenCode")
            alert.addButton(withTitle: "Later")
            if alert.runModal() == .alertFirstButtonReturn {
                restartPerformer()
            }
        }
    }

    /// Decides whether the contextual restart offer must first obtain
    /// OpenCodeServerAgent Keychain access. Creating the first nonempty
    /// credential needs the same explicit grant as changing one; Save itself
    /// remains non-interactive, and `offerRestartIfNeeded` suppresses the
    /// dialog entirely when OpenCode is not running.
    static func needsCredentialAuthorization(
        passwordIsEmpty: Bool,
        outcome: KeychainStore.SaveOutcome,
        passwordState: PasswordState?
    ) -> Bool {
        guard !passwordIsEmpty else { return false }
        switch outcome {
        case .created, .updatedExisting:
            return true
        case .unchanged, .deleted:
            return passwordState == .accessPending
        }
    }

    static func shouldOfferRestart(
        settingsChanged: Bool,
        credentialChanged: Bool,
        openCodeIsRunning: Bool
    ) -> Bool {
        openCodeIsRunning && (settingsChanged || credentialChanged)
    }

    static func credentialNotice(for outcome: KeychainStore.SaveOutcome) -> AgentCommand? {
        CredentialMutationCoordinator.notice(for: outcome)?.command
    }

    private func applyServiceChoices(wantsOpenCodeServerAgent: Bool) {
        if wantsOpenCodeServerAgent {
            services.registerOpenCodeServerAgent()
        } else if services.openCodeServerAgentStatus == .enabled {
            services.unregisterOpenCodeServerAgent { [weak self] error in
                if let error {
                    self?.showError(error.localizedDescription)
                }
            }
        }
        let wantsOpenCodeServer = openCodeServerLoginButton.state == .on
        services.setOpenCodeServerLoginEnabled(wantsOpenCodeServer) { [weak self] error in
            if let error {
                self?.showError(error.localizedDescription)
            }
        }
    }
}
