import AppKit
import Foundation
import OSLog
import UserNotifications

final class NotificationController: NSObject, UNUserNotificationCenterDelegate {
    private let logger = Logger(subsystem: "ai.opencode.server", category: "notifications")
    private let center = UNUserNotificationCenter.current()
    private let defaults = UserDefaults.standard
    private let deliveryLedger = NotificationDeliveryLedger(defaults: .standard)

    override init() {
        super.init()
        center.delegate = self
    }

    @MainActor
    func explainAndRequestIfNeeded() {
        let key = "NotificationPurposeExplained"
        guard !defaults.bool(forKey: key) else { return }
        defaults.set(true, forKey: key)

        let alert = NSAlert()
        alert.messageText = "OpenCode problem notifications"
        alert.informativeText = "OpenCodeServer uses notifications only for unexpected OpenCode failures, recovery, or exhausted recovery attempts. Normal OpenCode starts and stops are not announced."
        alert.addButton(withTitle: "Continue")
        alert.runModal()
        center.requestAuthorization(options: [.alert, .sound]) { [logger] _, error in
            if let error {
                logger.error("Notification authorization failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    func deliver(_ event: AgentNotification) {
        guard deliveryLedger.begin(eventID: event.eventID) else { return }
        let content = UNMutableNotificationContent()
        content.title = event.title
        content.body = event.message
        content.sound = event.kind == .recovered ? nil : .default
        let request = UNNotificationRequest(
            identifier: event.eventID,
            content: content,
            trigger: nil
        )
        center.add(request) { [deliveryLedger, logger] error in
            deliveryLedger.finish(eventID: event.eventID, accepted: error == nil)
            if let error {
                logger.error("Unable to deliver notification: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// A quiet pointer to Settings when OpenCodeServerAgent still needs the
    /// user to grant Keychain access. AppDelegate rate-limits this to once
    /// per access-pending episode; the menu and Settings rows are the
    /// primary indicators.
    func deliverKeychainAccessReminder() {
        let content = UNMutableNotificationContent()
        content.title = "Keychain access needed"
        content.body = "OpenCodeServerAgent is not authorized to read the OpenCode password. Open OpenCodeServer Settings and choose “Allow Keychain Access…”."
        let request = UNNotificationRequest(
            identifier: "keychain-access-reminder",
            content: content,
            trigger: nil
        )
        center.add(request) { [logger] error in
            if let error {
                logger.error("Unable to deliver notification: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent _: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }
}

/// De-duplicates the latest event repeated by status pushes without assuming
/// any ordering relationship between OpenCodeServer and OpenCodeServerAgent
/// lifetimes. IDs are recorded only after macOS accepts the request; failures
/// remain retryable. The bounded history prevents unbounded UserDefaults
/// growth while covering normal reconnect and relaunch repetition.
final class NotificationDeliveryLedger: @unchecked Sendable {
    private let defaults: UserDefaults
    private let key: String
    private let capacity: Int
    private let lock = NSLock()
    private var inFlight: Set<String> = []

    init(
        defaults: UserDefaults,
        key: String = "DeliveredAgentNotificationEventIDs",
        capacity: Int = 64
    ) {
        precondition(capacity > 0)
        self.defaults = defaults
        self.key = key
        self.capacity = capacity
    }

    func begin(eventID: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let delivered = defaults.stringArray(forKey: key) ?? []
        guard !delivered.contains(eventID), !inFlight.contains(eventID) else {
            return false
        }
        inFlight.insert(eventID)
        return true
    }

    func finish(eventID: String, accepted: Bool) {
        lock.lock()
        defer { lock.unlock() }
        inFlight.remove(eventID)
        guard accepted else { return }
        var delivered = defaults.stringArray(forKey: key) ?? []
        guard !delivered.contains(eventID) else { return }
        delivered.append(eventID)
        if delivered.count > capacity {
            delivered.removeFirst(delivered.count - capacity)
        }
        defaults.set(delivered, forKey: key)
    }
}
