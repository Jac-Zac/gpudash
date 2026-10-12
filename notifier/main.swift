// gpudash-notify: posts one macOS notification, shown with the gpudash icon.
// Lives inside gpudash.app so macOS knows which icon to use.
// Usage: gpudash-notify TITLE [BODY] [sound]
// Exit codes: 0 posted, 2 notifications not allowed, 3 failed.
import Foundation
import UserNotifications

let args = CommandLine.arguments
let title = args.count > 1 ? args[1] : "gpudash"
let body = args.count > 2 ? args[2] : ""
let sound = args.count > 3 && args[3] == "sound"

let center = UNUserNotificationCenter.current()
let done = DispatchSemaphore(value: 0)
var code: Int32 = 3

center.requestAuthorization(options: [.alert, .sound]) { granted, error in
    if let error { FileHandle.standardError.write("gpudash-notify: \(error.localizedDescription)\n".data(using: .utf8)!) }
    guard granted else {
        code = 2
        done.signal()
        return
    }
    let content = UNMutableNotificationContent()
    content.title = title
    content.body = body
    if sound { content.sound = .default }
    let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
    center.add(request) { error in
        code = error == nil ? 0 : 3
        done.signal()
    }
}

// the first run waits for the user to answer the permission prompt
_ = done.wait(timeout: .now() + 60)
exit(code)
