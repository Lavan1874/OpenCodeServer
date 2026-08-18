import AppKit

extension SettingsWindowController {
    func rebuildCandidates(selected: String) {
        candidatePopup.removeAllItems()
        candidatePopup.addItem(withTitle: "Automatic (recommended)")
        candidatePopup.lastItem?.representedObject = ""
        for path in ConfigStore.discoverExecutableCandidates() {
            candidatePopup.addItem(withTitle: path)
            candidatePopup.lastItem?.representedObject = path
        }
        if selected.isEmpty {
            candidatePopup.selectItem(at: 0)
        } else if let item = candidatePopup.itemArray.first(
            where: { ($0.representedObject as? String) == selected }
        ) {
            candidatePopup.select(item)
        }
    }

    @objc func candidateChanged() {
        if let path = candidatePopup.selectedItem?.representedObject as? String {
            executableField.stringValue = path
        }
    }

    @objc func chooseExecutable() {
        let panel = NSOpenPanel()
        panel.title = "Choose the native OpenCode executable"
        panel.prompt = "Choose"
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        panel.allowsMultipleSelection = false
        panel.resolvesAliases = false
        panel.beginSheetModal(for: window!) { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            self?.executableField.stringValue = url.path
            self?.rebuildCandidates(selected: url.path)
        }
    }
}
