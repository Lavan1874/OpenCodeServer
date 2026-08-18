import AppKit

extension SettingsWindowController {
    /// Re-renders the rows that mirror live agent status. AppDelegate calls
    /// this on every pushed/polled status update so the Agent access row
    /// (and its Allow Keychain Access button) never goes stale while the
    /// window is open — a Save flips the agent to `accessPending` within a
    /// fraction of a second, and the row must follow without a window reopen.
    func refreshLiveStatus() {
        guard window?.isVisible == true else { return }
        refreshAccessRow()
        renderSaveFeedback()
    }

    /// Mirrors OpenCodeServerAgent's credential state. The Allow Keychain
    /// Access button is only actionable when an item exists but the agent
    /// has not been authorized yet; this is distinct from the GUI's explicit
    /// Edit and Copy reads above.
    func refreshAccessRow() {
        if let detail = credentialMutations.availability.detail {
            agentAccessValueLabel.stringValue = "Unavailable"
            agentAccessValueLabel.toolTip = detail
            grantAccessButton.title = "Retry"
            grantAccessButton.setAccessibilityLabel("Retry credential journal read")
            grantAccessButton.toolTip =
                "Retry reading the pending credential mutation state. No state is deleted."
            grantAccessButton.isEnabled = true
            if saveFeedbackContext == nil {
                feedbackLabel.textColor = .systemRed
                updateFeedbackText(detail)
            }
            return
        }
        if feedbackLabel.stringValue.hasPrefix("Credential journal at ") {
            updateFeedbackText("")
        }
        agentAccessValueLabel.toolTip = nil
        grantAccessButton.title = "Allow Keychain Access…"
        grantAccessButton.setAccessibilityLabel("Allow OpenCodeServerAgent Keychain access")
        switch statusProvider()?.passwordState {
        case .configured:
            agentAccessValueLabel.stringValue = "Granted"
            grantAccessButton.isEnabled = false
        case .accessPending:
            agentAccessValueLabel.stringValue = "Not granted"
            grantAccessButton.isEnabled = true
        case .notConfigured:
            agentAccessValueLabel.stringValue = "—"
            grantAccessButton.isEnabled = false
        case nil:
            agentAccessValueLabel.stringValue = "Unknown"
            grantAccessButton.isEnabled = false
        }
    }

    /// Re-renders the green Save feedback from live credential state. Runs
    /// at Save time and on every status push, so guidance appears exactly
    /// while the Allow Keychain Access button is actionable and vanishes the
    /// moment re-authorization completes — no stale text, no reconciliation
    /// patch (replaces the v49/v53 write-then-trim design and its two flags).
    ///
    /// The "restart to apply" advice also retires on its own: once the agent
    /// has been SEEN carrying the pending change and a later status reports
    /// the restart landed, the text converges to "Saved. Changes are in
    /// effect." instead of advising a restart that already happened.
    func renderSaveFeedback() {
        guard let context = saveFeedbackContext else { return }
        let status = statusProvider()
        if status?.configPending == true {
            saveFeedbackSawConfigPending = true
        } else if saveFeedbackSawConfigPending, status?.configPending == false {
            // The agent registered the saved change earlier and a restart
            // (Allow & Restart, menu restart, or any other convergence)
            // has now applied it. A nil status never clears: unreachable is
            // unknown, not "applied".
            saveFeedbackContext = nil
            saveFeedbackSawConfigPending = false
            updateFeedbackText("Saved. Changes are in effect.")
            return
        }
        updateFeedbackText(
            Self.saveFeedbackText(
                context: context,
                passwordState: status?.passwordState
            )
        )
    }

    /// Pure feedback wording, kept static for tests. The authorization
    /// guidance is shown only in `accessPending` — precisely the state in
    /// which the button it mentions is enabled; in every other state the
    /// save stands on its own.
    static func saveFeedbackText(
        context: SaveFeedbackContext,
        passwordState: PasswordState?
    ) -> String {
        var text =
            "Saved. Restart OpenCode when you want these changes to take effect."
        guard context.passwordStored, passwordState == .accessPending else {
            return text
        }
        text +=
            " Click “Allow Keychain Access…” and choose “Always Allow” so OpenCodeServerAgent can read the new password from Keychain."
        if context.accountChanged {
            text +=
                " The username change created a new Keychain item, so access must be granted again."
        }
        return text
    }
}
