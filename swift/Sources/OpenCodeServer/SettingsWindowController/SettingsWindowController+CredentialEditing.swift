import AppKit
import OSLog

extension SettingsWindowController {
    private struct CredentialOperationToken {
        let presentationGeneration: Int
        let operationGeneration: Int
        let account: String
    }

    /// Starts a new visible Settings presentation. The controller itself is
    /// intentionally retained by AppDelegate, but credentials must not cross
    /// the boundary between two presentations.
    func beginCredentialPresentationSession() {
        credentialPresentationGeneration &+= 1
        credentialPresentationIsActive = true
    }

    /// Invalidates every in-flight probe/edit/copy result before clearing the
    /// controls. The callback guard below then makes any late result a no-op,
    /// including the Copy path before it can touch the pasteboard.
    func endCredentialPresentationSession() {
        // Do not cancel or release credentialSaveTransaction here. It owns a
        // durable Keychain/configuration transaction that must finish even
        // when the Settings window is closed; only presentation state is
        // invalidated below.
        let safeState: CredentialEditorState
        switch credentialEditorState {
        case .loading, .editingExisting, .removalPending:
            // All three states can only be entered for an item that the
            // attribute-only probe established as present. Preserve that
            // non-secret fact while discarding the decrypted value and the
            // editor's original-password comparison value.
            safeState = .stored
        case .checking:
            safeState = .checking
        case .absent:
            safeState = .absent
        case .stored:
            safeState = .stored
        case .unavailable:
            safeState = .unavailable
        }
        credentialPresentationIsActive = false
        credentialPresentationGeneration &+= 1
        credentialOperationGeneration &+= 1
        window?.makeFirstResponder(nil)
        clearPasswordFields()
        credentialEditorState = safeState
        loadedAccount = nil
        renderCredentialEditor()
    }

    private func beginCredentialOperation(account: String) -> CredentialOperationToken? {
        guard credentialPresentationIsActive else { return nil }
        credentialOperationGeneration &+= 1
        return CredentialOperationToken(
            presentationGeneration: credentialPresentationGeneration,
            operationGeneration: credentialOperationGeneration,
            account: account
        )
    }

    private func isCurrentCredentialOperation(_ token: CredentialOperationToken) -> Bool {
        credentialPresentationIsActive
            && token.presentationGeneration == credentialPresentationGeneration
            && token.operationGeneration == credentialOperationGeneration
            && token.account == loadedAccount
    }

    /// Checks only whether an item exists. `kSecReturnAttributes` does not
    /// decrypt the value or raise the legacy Keychain authorization prompt,
    /// but Security.framework can still block on securityd, so even this
    /// non-interactive operation stays off the main thread.
    func beginCredentialProbe(account: String) {
        guard let token = beginCredentialOperation(account: account) else { return }
        credentialEditorState = .checking
        clearPasswordFields()
        renderCredentialEditor()
        let contains = keychainContains
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = Result { try contains(account) }
            DispatchQueue.main.async { [weak self] in
                guard let self,
                      self.isCurrentCredentialOperation(token)
                else { return }
                switch result {
                case .success(true):
                    self.credentialEditorState = .stored
                case .success(false):
                    self.credentialEditorState = .absent
                case .failure(let error):
                    self.credentialEditorState = .unavailable
                    self.showError(
                        "Password status could not be checked (\(error.localizedDescription)). You can still save other settings; retry before changing the username or password."
                    )
                }
                self.renderCredentialEditor()
            }
        }
    }

    /// The second disclosure level for the GUI's own credential access.
    /// Only an explicit Edit or Copy click reaches this decrypt-class read;
    /// it runs on a worker queue because the system consent dialog can block
    /// until the user answers it.
    private enum ExplicitCredentialRead {
        case edit
        case copy
    }

    private func beginExplicitCredentialRead(_ action: ExplicitCredentialRead) {
        guard let account = loadedAccount else { return }
        guard let token = beginCredentialOperation(account: account) else { return }
        credentialEditorState = .loading
        renderCredentialEditor()
        let load = keychainLoad
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let result = Result { try load(account) }
            DispatchQueue.main.async { [weak self] in
                guard let self,
                      self.isCurrentCredentialOperation(token)
                else { return }
                switch result {
                case .success(.some(let password)):
                    switch action {
                    case .edit:
                        self.securePasswordField.stringValue = password
                        self.plainPasswordField.stringValue = password
                        self.credentialEditorState = .editingExisting(original: password)
                    case .copy:
                        self.copyPasswordToPasteboard(password)
                        self.credentialEditorState = .stored
                        self.feedbackLabel.textColor = .secondaryLabelColor
                        self.updateFeedbackText("Password copied.")
                    }
                case .success(nil):
                    self.credentialEditorState = .absent
                    self.clearPasswordFields()
                    self.showError("The saved password is no longer present in Keychain.")
                case .failure(let error):
                    self.credentialEditorState = .stored
                    if KeychainStore.isUserCancellation(error) {
                        // Cancel and Escape are ordinary ways to decline the
                        // system dialog. Return to the stable stored state;
                        // don't turn an intentionally abandoned action into
                        // an alarming application error.
                        self.logger.debug("Saved password read was canceled by the user")
                    } else {
                        self.logger.error(
                            "Saved password read failed: \(String(describing: error), privacy: .public)"
                        )
                        self.showError(KeychainStore.userFacingReadFailure(error))
                    }
                }
                self.renderCredentialEditor()
            }
        }
    }

    func renderCredentialEditor() {
        saveButton.isEnabled = true
        credentialProgressIndicator.stopAnimation(nil)
        credentialProgressIndicator.isHidden = true
        passwordStatusLabel.isHidden = true
        securePasswordField.isHidden = true
        plainPasswordField.isHidden = true
        showPasswordButton.isHidden = true
        editPasswordButton.isHidden = true
        copyPasswordButton.isHidden = true
        removePasswordButton.isHidden = true

        switch credentialEditorState {
        case .checking:
            passwordStatusLabel.stringValue = "Checking Keychain…"
            passwordStatusLabel.isHidden = false
        case .absent:
            securePasswordField.isHidden = showPasswordButton.state == .on
            plainPasswordField.isHidden = showPasswordButton.state != .on
            showPasswordButton.isHidden = false
        case .stored:
            passwordStatusLabel.stringValue = "Stored in Keychain"
            passwordStatusLabel.isHidden = false
            editPasswordButton.title = "Edit…"
            editPasswordButton.setAccessibilityLabel("Edit saved password")
            editPasswordButton.toolTip = "Load the saved password for editing. macOS may ask you to allow Keychain access."
            editPasswordButton.isHidden = false
            copyPasswordButton.isHidden = false
            removePasswordButton.isHidden = false
        case .loading:
            saveButton.isEnabled = false
            // The Password row itself supplies the context. Apple recommends
            // a small, unlabeled spinner for a user-initiated background task
            // in constrained space; VoiceOver receives the explicit label.
            credentialProgressIndicator.isHidden = false
            credentialProgressIndicator.startAnimation(nil)
        case .editingExisting:
            securePasswordField.isHidden = showPasswordButton.state == .on
            plainPasswordField.isHidden = showPasswordButton.state != .on
            showPasswordButton.isHidden = false
            editPasswordButton.title = "Cancel Edit"
            editPasswordButton.setAccessibilityLabel("Cancel editing, keep the saved password")
            editPasswordButton.toolTip = "Discard your edits and keep the saved password."
            editPasswordButton.isHidden = false
            removePasswordButton.isHidden = false
        case .removalPending:
            passwordStatusLabel.stringValue = "Will be removed when you save"
            passwordStatusLabel.isHidden = false
            editPasswordButton.title = "Undo"
            editPasswordButton.setAccessibilityLabel("Undo password removal")
            editPasswordButton.toolTip = "Keep the saved password after all."
            editPasswordButton.isHidden = false
        case .unavailable:
            passwordStatusLabel.stringValue = "Unable to determine"
            passwordStatusLabel.isHidden = false
            editPasswordButton.title = "Retry"
            editPasswordButton.setAccessibilityLabel("Retry password status check")
            editPasswordButton.toolTip = nil
            editPasswordButton.isHidden = false
        }
        // A state with no visible controls (checking/loading) hides the
        // whole second line: a hidden arranged subview drops out of the
        // vertical stack entirely, so its spacing collapses and the single
        // content line centers in the reserved row height.
        passwordControlsStack?.isHidden = [
            showPasswordButton,
            editPasswordButton,
            copyPasswordButton,
            removePasswordButton
        ].allSatisfy(\.isHidden)
    }

    func clearPasswordFields() {
        securePasswordField.stringValue = ""
        plainPasswordField.stringValue = ""
        showPasswordButton.state = .off
    }

    @objc func togglePasswordVisibility() {
        let show = showPasswordButton.state == .on
        if show {
            plainPasswordField.stringValue = securePasswordField.stringValue
        } else {
            securePasswordField.stringValue = plainPasswordField.stringValue
        }
        securePasswordField.isHidden = show
        plainPasswordField.isHidden = !show
    }

    @objc func editPassword() {
        switch credentialEditorState {
        case .stored:
            beginExplicitCredentialRead(.edit)
        case .editingExisting:
            clearPasswordFields()
            credentialEditorState = .stored
            renderCredentialEditor()
        case .removalPending:
            credentialEditorState = .stored
            renderCredentialEditor()
        case .unavailable:
            if let loadedAccount {
                beginCredentialProbe(account: loadedAccount)
            }
        case .checking, .absent, .loading:
            break
        }
    }

    @objc func copyPassword() {
        guard case .stored = credentialEditorState else { return }
        beginExplicitCredentialRead(.copy)
    }

    @objc func removePassword() {
        switch credentialEditorState {
        case .stored, .editingExisting:
            clearPasswordFields()
            credentialEditorState = .removalPending
            renderCredentialEditor()
        case .checking, .absent, .loading, .removalPending, .unavailable:
            break
        }
    }

    /// The agent's one deliberate Keychain authorization path: it performs
    /// an interactive read on a worker thread and macOS presents the
    /// "Always Allow" dialog in the context of this click.
    @objc func grantAccess() {
        guard credentialMutations.availability.isAvailable else {
            credentialMutations.retryAvailability()
            refreshAccessRow()
            return
        }
        grantAccessButton.isEnabled = false
        agentAccessValueLabel.stringValue = "Authorization requested…"
        credentialAuthorizationPerformer()
        // The agent answers asynchronously once the user resolves the system
        // dialog; refresh the row shortly after, and always on re-present.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            self?.refreshAccessRow()
        }
    }

    func currentPassword() -> String {
        showPasswordButton.state == .on
            ? plainPasswordField.stringValue
            : securePasswordField.stringValue
    }
}
