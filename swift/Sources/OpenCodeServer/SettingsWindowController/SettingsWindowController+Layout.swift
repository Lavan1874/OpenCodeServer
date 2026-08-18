import AppKit

extension SettingsWindowController {
    private enum LayoutMetrics {
        static let horizontalMargin: CGFloat = 24
        static let topMargin: CGFloat = 24
        static let bottomMargin: CGFloat = 20
        static let rowSpacing: CGFloat = 12
        static let columnSpacing: CGFloat = 18
        static let controlSpacing: CGFloat = 8
    }

    func buildInterface() {
        hostnameField.placeholderString = "127.0.0.1"
        hostnameField.setAccessibilityLabel("Listening address")
        portField.placeholderString = "4096"
        portField.setAccessibilityLabel("Port")
        portField.formatter = {
            let formatter = NumberFormatter()
            formatter.allowsFloats = false
            formatter.minimum = 1
            formatter.maximum = 65_535
            return formatter
        }()
        usernameField.placeholderString = "opencode"
        usernameField.setAccessibilityLabel("Username")
        securePasswordField.setAccessibilityLabel("Password")
        plainPasswordField.setAccessibilityLabel("Visible password")
        showPasswordButton.setAccessibilityLabel("Show password")
        passwordStatusLabel.setAccessibilityLabel("Password status")
        passwordStatusLabel.textColor = .secondaryLabelColor
        credentialProgressIndicator.style = .spinning
        credentialProgressIndicator.controlSize = .small
        credentialProgressIndicator.isIndeterminate = true
        credentialProgressIndicator.isDisplayedWhenStopped = false
        credentialProgressIndicator.setAccessibilityLabel("Reading password from Keychain")
        editPasswordButton.setAccessibilityLabel("Edit saved password")
        copyPasswordButton.setAccessibilityLabel("Copy saved password")
        // Help tags carry the non-obvious authorization model at the exact
        // control it concerns, replacing the deleted footer paragraphs
        // (HIG: context-sensitive help, not manual pages embedded in UI).
        copyPasswordButton.toolTip = "Copy the saved password. macOS may ask you to allow Keychain access."
        removePasswordButton.setAccessibilityLabel("Remove saved password")
        grantAccessButton.toolTip = "Authorizes the background OpenCodeServerAgent to read the saved password — a separate grant from editing or copying it here."
        executableField.placeholderString = "Automatic discovery"
        executableField.setAccessibilityLabel("OpenCode executable")
        candidatePopup.setAccessibilityLabel("Detected OpenCode executable")
        plainPasswordField.isHidden = true

        showPasswordButton.target = self
        showPasswordButton.action = #selector(togglePasswordVisibility)
        editPasswordButton.target = self
        editPasswordButton.action = #selector(editPassword)
        copyPasswordButton.target = self
        copyPasswordButton.action = #selector(copyPassword)
        removePasswordButton.target = self
        removePasswordButton.action = #selector(removePassword)
        candidatePopup.target = self
        candidatePopup.action = #selector(candidateChanged)
        grantAccessButton.target = self
        grantAccessButton.action = #selector(grantAccess)

        let chooseButton = NSButton(title: "Choose…", target: self, action: #selector(chooseExecutable))
        chooseButton.setAccessibilityLabel("Choose OpenCode executable")
        // HIG: a field is sized for its anticipated content, and a password
        // the user must verify (with Show on) must not scroll horizontally.
        // The row is therefore two lines: the content line takes the full
        // value-column width (state label, spinner, or the password field),
        // and the action controls live on a second line instead of dividing
        // the field's width. Progressive disclosure is unchanged: which
        // controls the second line shows still depends on the state.
        let passwordContentStack = horizontalStack([
            credentialProgressIndicator,
            passwordStatusLabel,
            securePasswordField,
            plainPasswordField
        ])
        // The trailing spacer absorbs the column's slack width (hugging 1
        // loses every tie), so the buttons keep their native fitting widths
        // and the stack itself has a fully determined width.
        let passwordControlsTail = NSView()
        passwordControlsTail.setContentHuggingPriority(
            NSLayoutConstraint.Priority(1),
            for: .horizontal
        )
        let passwordControlsStack = horizontalStack([
            showPasswordButton,
            editPasswordButton,
            copyPasswordButton,
            removePasswordButton,
            passwordControlsTail
        ])
        self.passwordControlsStack = passwordControlsStack
        let passwordStack = NSStackView(views: [passwordContentStack, passwordControlsStack])
        passwordStack.orientation = .vertical
        passwordStack.alignment = .leading
        passwordStack.spacing = LayoutMetrics.controlSpacing
        passwordContentStack.widthAnchor.constraint(equalTo: passwordStack.widthAnchor).isActive = true
        passwordControlsStack.widthAnchor.constraint(equalTo: passwordStack.widthAnchor).isActive = true
        plainPasswordField.widthAnchor.constraint(equalTo: securePasswordField.widthAnchor).isActive = true
        let accessStack = horizontalStack([agentAccessValueLabel, grantAccessButton])
        let executableStack = horizontalStack([executableField, chooseButton])

        // A port has at most five decimal digits. Derive a compact native
        // field width from AppKit's own fitting size for the largest valid
        // value instead of assigning an arbitrary visual constant.
        let portWidth = textFieldWidth(
            matching: portField,
            anticipatedText: "65535"
        )
        portField.widthAnchor.constraint(
            equalToConstant: portWidth
        ).isActive = true
        portField.setContentHuggingPriority(.required, for: .horizontal)
        let portStack = horizontalStack([portField, NSView()])

        let startupStack = NSStackView(views: [
            openCodeServerAgentLoginButton,
            openCodeServerLoginButton
        ])
        startupStack.orientation = .vertical
        startupStack.alignment = .leading
        startupStack.spacing = LayoutMetrics.controlSpacing

        let listeningAddressLabel = label("Listening address")
        let portLabel = label("Port")
        let usernameLabel = label("Username")
        let passwordLabel = label("Password")
        let agentAccessLabel = label("Agent access")
        let startupLabel = label("Startup")
        let discoveryLabel = label("Discovery")
        let detectedOpenCodeLabel = label("Detected OpenCode")
        let executableLabel = label("OpenCode executable")
        let formLabels = [
            listeningAddressLabel,
            portLabel,
            usernameLabel,
            passwordLabel,
            agentAccessLabel,
            startupLabel,
            discoveryLabel,
            detectedOpenCodeLabel,
            executableLabel
        ]
        let sharedLabelColumnWidth = ceil(
            formLabels.map(\.fittingSize.width).max() ?? 0
        )
        // Password swaps between native controls with different heights: the
        // content line is tallest while editing (a bordered text field), the
        // controls line is tallest with push buttons shown. Reserve the
        // tallest native semantic state — two lines measured from the
        // controls themselves, independent of the state visible right now —
        // so NSGridView never shrinks this row when the visible arranged
        // subviews change.
        let passwordContentLineHeight = [
            credentialProgressIndicator,
            passwordStatusLabel,
            securePasswordField,
            plainPasswordField
        ].map(\.fittingSize.height).max() ?? 0
        let passwordControlsLineHeight = [
            showPasswordButton,
            editPasswordButton,
            copyPasswordButton,
            removePasswordButton
        ].map(\.fittingSize.height).max() ?? 0
        let passwordRowHeight = ceil([
            passwordLabel.fittingSize.height,
            passwordContentLineHeight
                + LayoutMetrics.controlSpacing
                + passwordControlsLineHeight
        ].max() ?? 0)

        // HIG: size a text field for the anticipated quantity of input. A
        // listening address may be a full textual IPv6 address, so use that
        // real input shape with the current AppKit font/control metrics.
        let addressWidth = textFieldWidth(
            matching: hostnameField,
            anticipatedText: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"
        )
        let executableFieldWidth = textFieldWidth(
            matching: executableField,
            anticipatedText: "Automatic discovery"
        )

        // Accommodate the widest Password and Agent access states without
        // resizing. The Password row is two lines now, so its width demand is
        // the stored state's controls line; the editing state's field simply
        // fills the column. These widths come from native fitting sizes, not
        // guessed field widths or a screenshot-specific window size.
        let storedPasswordWidth = fittingWidth(of: [
            editPasswordButton,
            copyPasswordButton,
            removePasswordButton
        ])
        let authorizationWidth = fittingWidth(of: [
            NSTextField(labelWithString: "Authorization requested…"),
            grantAccessButton
        ])
        let valueColumnWidth = ceil([
            addressWidth,
            storedPasswordWidth,
            authorizationWidth,
            startupStack.fittingSize.width,
            mdnsButton.fittingSize.width,
            executableFieldWidth
                + LayoutMetrics.controlSpacing
                + chooseButton.fittingSize.width
        ].max() ?? 0)
        hostnameField.widthAnchor.constraint(
            greaterThanOrEqualToConstant: valueColumnWidth
        ).isActive = true
        preferredContentWidth = ceil(
            LayoutMetrics.horizontalMargin * 2
                + sharedLabelColumnWidth
                + LayoutMetrics.columnSpacing
                + valueColumnWidth
        )

        // The one fact worth keeping permanently visible: what an empty
        // password means. It sits directly beneath the field it describes,
        // in the small secondary style System Settings uses for control
        // captions, and it is worded as a state-neutral rule — with a
        // password stored it reads as documentation (and gives Remove… its
        // consequence), never as an instruction presupposing a blank field.
        // Everything else the old footer paragraphs covered is delivered at
        // its moment of need instead — save feedback, the consent dialog
        // itself, and control help tags.
        let passwordCaption = NSTextField(
            wrappingLabelWithString: "Without a password, OpenCode is unauthenticated."
        )
        passwordCaption.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        passwordCaption.textColor = .secondaryLabelColor

        let grid = NSGridView(views: [
            [listeningAddressLabel, hostnameField],
            [portLabel, portStack],
            [usernameLabel, usernameField],
            [passwordLabel, passwordStack],
            [NSGridCell.emptyContentView, passwordCaption],
            [agentAccessLabel, accessStack],
            [startupLabel, startupStack]
        ])
        configureFormGrid(grid, labelColumnWidth: sharedLabelColumnWidth)
        let passwordRow = grid.row(at: 3)
        // A spinner has no text baseline. Baseline-aligning a spinner-only
        // stack makes AppKit move it upward and also changes the apparent
        // spacing above and below the row. Center both cells within the stable
        // semantic row height instead.
        passwordRow.rowAlignment = .none
        passwordRow.yPlacement = .center
        passwordRow.height = passwordRowHeight

        let advancedGrid = NSGridView(views: [
            [discoveryLabel, mdnsButton],
            [detectedOpenCodeLabel, candidatePopup],
            [executableLabel, executableStack]
        ])
        configureFormGrid(advancedGrid, labelColumnWidth: sharedLabelColumnWidth)
        self.advancedGrid = advancedGrid

        // Every value cell shares one width across both grids. Hiding the
        // Password row's buttons while a Keychain read is pending therefore
        // changes only that row's contents; it can never move the form's
        // label/control boundary or any unrelated field.
        let valueColumnViews: [NSView] = [
            portStack,
            usernameField,
            passwordStack,
            accessStack,
            startupStack,
            mdnsButton,
            candidatePopup,
            executableStack
        ]
        let valueColumnConstraints = valueColumnViews.map {
            $0.widthAnchor.constraint(equalTo: hostnameField.widthAnchor)
        }

        // Progressive disclosure in the presentation Finder's Get Info
        // window uses for collapsible sections (HIG disclosure-controls:
        // a labeled disclosure triangle; the label names what is hidden):
        // a full-width section header row — chevron plus a semibold title,
        // preceded by a hairline — instead of the dated triangle-plus-label
        // look of the stock disclosure bezel. Only the presentation is
        // custom; the control remains a plain pushOnPushOff NSButton with
        // the expanded state exposed to VoiceOver.
        let advancedSeparator = NSBox()
        advancedSeparator.boxType = .separator
        advancedDisclosureButton.setButtonType(.pushOnPushOff)
        advancedDisclosureButton.isBordered = false
        advancedDisclosureButton.title = "Advanced"
        advancedDisclosureButton.font = NSFont.systemFont(
            ofSize: NSFont.systemFontSize,
            weight: .semibold
        )
        advancedDisclosureButton.imagePosition = .imageLeading
        advancedDisclosureButton.imageHugsTitle = true
        advancedDisclosureButton.alignment = .left
        advancedDisclosureButton.target = self
        advancedDisclosureButton.action = #selector(toggleAdvanced)

        feedbackLabel.textColor = .systemRed
        feedbackLabel.setAccessibilityLabel("Settings feedback")
        // The root stack supplies the available width. Lower horizontal
        // compression resistance lets Auto Layout wrap this transient copy
        // inside that width instead of treating the unwrapped sentence as a
        // new window-width requirement.
        feedbackLabel.lineBreakMode = .byWordWrapping
        feedbackLabel.maximumNumberOfLines = 3
        feedbackLabel.setContentCompressionResistancePriority(
            .defaultLow,
            for: .horizontal
        )

        let cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancel))
        cancelButton.keyEquivalent = "\u{1b}"
        saveButton.target = self
        saveButton.action = #selector(save)
        saveButton.keyEquivalent = "\r"
        let buttons = horizontalStack([NSView(), cancelButton, saveButton])

        let root = NSStackView(views: [
            grid,
            advancedSeparator,
            advancedDisclosureButton,
            advancedGrid,
            feedbackLabel,
            buttons
        ])
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = LayoutMetrics.columnSpacing
        root.translatesAutoresizingMaskIntoConstraints = false
        rootStack = root
        guard let content = window?.contentView else { return }
        content.addSubview(root)
        NSLayoutConstraint.activate(valueColumnConstraints + [
            root.leadingAnchor.constraint(
                equalTo: content.leadingAnchor,
                constant: LayoutMetrics.horizontalMargin
            ),
            root.trailingAnchor.constraint(
                equalTo: content.trailingAnchor,
                constant: -LayoutMetrics.horizontalMargin
            ),
            root.topAnchor.constraint(
                equalTo: content.topAnchor,
                constant: LayoutMetrics.topMargin
            ),
            root.bottomAnchor.constraint(
                lessThanOrEqualTo: content.bottomAnchor,
                constant: -LayoutMetrics.bottomMargin
            ),
            grid.widthAnchor.constraint(equalTo: root.widthAnchor),
            advancedSeparator.widthAnchor.constraint(equalTo: root.widthAnchor),
            advancedDisclosureButton.widthAnchor.constraint(equalTo: root.widthAnchor),
            advancedGrid.widthAnchor.constraint(equalTo: root.widthAnchor),
            feedbackLabel.widthAnchor.constraint(equalTo: root.widthAnchor),
            buttons.widthAnchor.constraint(equalTo: root.widthAnchor)
        ])
        setAdvancedExpanded(false, resize: false)
        renderCredentialEditor()
    }

    @objc private func toggleAdvanced() {
        setAdvancedExpanded(advancedDisclosureButton.state == .on, resize: true)
    }

    /// Shows or hides the Advanced area. A hidden arranged subview drops out
    /// of the stack layout entirely, so the window is resized to fit the new
    /// content height — a fixed-height window would leave dead space or clip
    /// the feedback and buttons.
    func setAdvancedExpanded(_ expanded: Bool, resize: Bool) {
        advancedDisclosureButton.state = expanded ? .on : .off
        // The chevron stands in for the disclosure triangle: right while
        // collapsed, down while expanded (HIG disclosure-controls).
        advancedDisclosureButton.image = NSImage(
            systemSymbolName: expanded ? "chevron.down" : "chevron.right",
            accessibilityDescription: nil
        )?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: NSFont.systemFontSize, weight: .semibold)
        )
        advancedDisclosureButton.setAccessibilityExpanded(expanded)
        advancedGrid?.isHidden = !expanded
        guard resize else { return }
        resizeWindowForContent()
    }

    func resizeWindowForContent() {
        guard let window, let rootStack else { return }
        rootStack.layoutSubtreeIfNeeded()
        window.layoutIfNeeded()
        let height = ceil(
            rootStack.fittingSize.height
                + LayoutMetrics.topMargin
                + LayoutMetrics.bottomMargin
        )
        window.setContentSize(NSSize(width: preferredContentWidth, height: height))
    }

    private func label(_ title: String) -> NSTextField {
        let field = NSTextField(labelWithString: title)
        // Align text toward the control column. Use the interface direction
        // rather than a permanently physical edge so the form mirrors if it
        // is localized for a right-to-left language.
        field.alignment = NSApp.userInterfaceLayoutDirection == .rightToLeft ? .left : .right
        return field
    }

    private func configureFormGrid(_ grid: NSGridView, labelColumnWidth: CGFloat) {
        grid.rowSpacing = LayoutMetrics.rowSpacing
        grid.columnSpacing = LayoutMetrics.columnSpacing
        grid.rowAlignment = .firstBaseline
        // Apple form guidance uses equal-width labels with trailing-aligned
        // text. Fixing the column to the largest native fitting width keeps
        // the control leading edge stable without hard-coding a locale-
        // specific label width.
        grid.column(at: 0).width = labelColumnWidth
        grid.column(at: 0).xPlacement = .fill
        grid.column(at: 1).xPlacement = .fill
    }

    private func horizontalStack(_ views: [NSView]) -> NSStackView {
        let stack = NSStackView(views: views)
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.spacing = LayoutMetrics.controlSpacing
        return stack
    }

    private func fittingWidth(of views: [NSView]) -> CGFloat {
        guard !views.isEmpty else { return 0 }
        return views.reduce(0) { $0 + $1.fittingSize.width }
            + LayoutMetrics.controlSpacing * CGFloat(views.count - 1)
    }

    /// Editable NSTextField deliberately has no intrinsic horizontal width:
    /// it expands to the space its form gives it. Consequently its
    /// `fittingSize.width` is zero even after `sizeToFit()` changes the frame.
    /// Ask NSControl for the cell-backed size that fits the anticipated input
    /// instead; this follows AppKit metrics for the active font, bezel, and
    /// control size without baking screenshot-derived pixels into the form.
    private func textFieldWidth(
        matching field: NSTextField,
        anticipatedText: String
    ) -> CGFloat {
        let sizer = NSTextField()
        sizer.font = field.font
        sizer.controlSize = field.controlSize
        sizer.bezelStyle = field.bezelStyle
        sizer.stringValue = anticipatedText
        return ceil(
            sizer.sizeThatFits(
                NSSize(
                    width: CGFloat.greatestFiniteMagnitude,
                    height: CGFloat.greatestFiniteMagnitude
                )
            ).width
        )
    }
}
