// FigCraft Dock launcher (fork). scripts/fork/install-dock-app.sh compiles it into
// ~/Applications/FigCraft.app. Click: opens the app, building it first only when the code changed
// (scripts/fork/app.sh). While it builds, a small window shows progress; Stop, closing the window,
// Cmd-Q or Quit from the Dock stop the build. A running FigCraft is never quit or replaced.
//
// The launcher starts FigCraft itself and stays running (hidden) until it quits: macOS privacy
// permissions (Full Disk Access, Files and Folders) belong to the app that started a process, so
// granting them to FigCraft.app once covers every rebuilt binary. A launcher that quit right
// away left the binary on its own, without them.
//
// Documents: the launcher is the app macOS hands .figcraft files to (Finder double-click, Open
// With, drops on the Dock icon). The built binary isn't an app bundle, so it never gets those
// events itself: a FigCraft that already runs opens the file through its control channel
// (`file.open`, port FIGCRAFT_CONTROL_PORT, 7979 by default); otherwise the file goes on the
// command line of the FigCraft the launcher starts.

import AppKit

let root = Bundle.main.object(forInfoDictionaryKey: "FigCraftRoot") as? String ?? ""
let script = root + "/scripts/fork/app.sh"
let log = root + "/target/local/build.log"
let appBinary = root + "/target/local/local/figcraft"
let controlPort = Int(ProcessInfo.processInfo.environment["FIGCRAFT_CONTROL_PORT"] ?? "") ?? 7979

/// Ask the running FigCraft to open `path` (control channel `engine.execute` `file.open`).
/// Returns nil when it did, else why not.
func openInRunningApp(_ path: String) -> String? {
    var input: InputStream?
    var output: OutputStream?
    Stream.getStreamsToHost(withName: "127.0.0.1", port: controlPort, inputStream: &input, outputStream: &output)
    guard let i = input, let o = output else { return "FigCraft's control channel isn't reachable" }
    i.open()
    o.open()
    defer {
        i.close()
        o.close()
    }
    let request: [String: Any] = ["id": 1, "method": "engine.execute", "params": ["command": "file.open", "params": ["path": path]]]
    guard var data = try? JSONSerialization.data(withJSONObject: request) else { return "couldn't encode the request" }
    data.append(10)
    let bytes = [UInt8](data)
    let deadline = Date().addingTimeInterval(15)
    var sent = 0
    while sent < bytes.count {
        if Date() > deadline { return "FigCraft didn't answer" }
        if o.streamStatus == .error { return "FigCraft's control channel isn't reachable" }
        guard o.hasSpaceAvailable else {
            usleep(10_000)
            continue
        }
        let n = bytes[sent...].withUnsafeBufferPointer { o.write($0.baseAddress!, maxLength: $0.count) }
        if n < 0 { return "FigCraft's control channel isn't reachable" }
        sent += n
    }
    var reply = Data()
    var buf = [UInt8](repeating: 0, count: 4096)
    while !reply.contains(10) {
        if Date() > deadline { return "FigCraft didn't answer" }
        if i.streamStatus == .error || i.streamStatus == .atEnd { break }
        guard i.hasBytesAvailable else {
            usleep(10_000)
            continue
        }
        let n = i.read(&buf, maxLength: buf.count)
        if n <= 0 { break }
        reply.append(contentsOf: buf[0..<n])
    }
    let line = reply.split(separator: 10).first.map { Data($0) } ?? Data()
    guard let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { return "FigCraft didn't answer" }
    if obj["ok"] as? Bool == false || obj["error"] != nil {
        if let text = obj["error"] as? String { return text }
        if let err = obj["error"] as? [String: Any], let text = err["message"] as? String { return text }
        return "FigCraft couldn't open it"
    }
    return nil
}

/// Runs app.sh with `args` and returns its trimmed output (empty on failure).
func run(_ args: [String]) -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/bin/sh")
    p.arguments = [script] + args
    let out = Pipe()
    p.standardOutput = out
    p.standardError = FileHandle.nullDevice
    guard (try? p.run()) != nil else { return "" }
    p.waitUntilExit()
    let data = out.fileHandleForReading.readDataToEndOfFile()
    return String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
}

/// Brings the running FigCraft (a plain executable, found by its path) to the front.
func bringForward() {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
    p.arguments = ["-f", "^\(appBinary)( |$)"]
    let out = Pipe()
    p.standardOutput = out
    guard (try? p.run()) != nil else { return }
    p.waitUntilExit()
    let pids = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    for line in pids.split(separator: "\n") {
        if let pid = pid_t(line), let app = NSRunningApplication(processIdentifier: pid) {
            app.activate()
            return
        }
    }
}

/// The last "Compiling …" / "Finished …" / "error…" line of the build log.
func lastLogLine() -> String {
    guard let text = try? String(contentsOfFile: log, encoding: .utf8) else { return "" }
    for line in text.split(separator: "\n").reversed() {
        let t = line.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("Compiling") || t.hasPrefix("Finished") || t.hasPrefix("error") {
            return String(t.prefix(80))
        }
    }
    return ""
}

final class Launcher: NSObject, NSApplicationDelegate, NSWindowDelegate {
    var window: NSWindow?
    var detail = NSTextField(labelWithString: "Only what changed since the last build.")
    var build: Process?
    var appProcess: Process?
    var timer: Timer?
    var stopping = false
    /// Documents to open once FigCraft is up (they arrived while launching or building).
    var pending: [String] = []
    var launched = false

    /// Finder double-click, Open With, a drop on the Dock icon: before launch finishes (the
    /// documents that launched the launcher) or any time after.
    func application(_ application: NSApplication, open urls: [URL]) {
        let paths = urls.filter { $0.isFileURL }.map { $0.path }
        guard launched else {
            pending += paths
            return
        }
        if appProcess?.isRunning == true || run(["--status"]) == "running" {
            handOver(paths)
            bringForward()
        } else {
            pending += paths
        }
    }

    /// Open `paths` in the FigCraft that runs; say which ones it couldn't take.
    func handOver(_ paths: [String]) {
        var failed: [String] = []
        for p in paths {
            if let why = openInRunningApp(p) { failed.append("\((p as NSString).lastPathComponent): \(why)") }
        }
        guard !failed.isEmpty else { return }
        let alert = NSAlert()
        alert.messageText = "FigCraft couldn't open " + (failed.count == 1 ? "the file" : "\(failed.count) files")
        alert.informativeText = failed.joined(separator: "\n")
        alert.alertStyle = .warning
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    func applicationDidFinishLaunching(_ note: Notification) {
        launched = true
        switch run(["--status"]) {
        case "running":
            handOver(pending)
            pending = []
            bringForward()
            NSApp.terminate(nil)
        case "ready":
            launchApp()
        default:
            startBuild()
        }
    }

    func startBuild() {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 420, height: 130), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        w.title = "FigCraft"
        w.delegate = self
        let title = NSTextField(labelWithString: "Building FigCraft…")
        title.font = .boldSystemFont(ofSize: 14)
        detail.textColor = .secondaryLabelColor
        detail.lineBreakMode = .byTruncatingTail
        let spinner = NSProgressIndicator()
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.startAnimation(nil)
        let stop = NSButton(title: "Stop", target: self, action: #selector(stopPressed))
        stop.keyEquivalent = "\u{1b}"
        let top = NSStackView(views: [spinner, title])
        let stack = NSStackView(views: [top, detail, stop])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 18, left: 20, bottom: 16, right: 20)
        stack.translatesAutoresizingMaskIntoConstraints = false
        w.contentView?.addSubview(stack)
        if let c = w.contentView {
            NSLayoutConstraint.activate([
                stack.leadingAnchor.constraint(equalTo: c.leadingAnchor),
                stack.trailingAnchor.constraint(equalTo: c.trailingAnchor),
                stack.topAnchor.constraint(equalTo: c.topAnchor),
                detail.widthAnchor.constraint(equalToConstant: 380),
            ])
        }
        w.center()
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        window = w

        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        // Build only: launchApp() starts FigCraft itself once the build ends.
        p.arguments = [script, "--build-only"]
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        p.terminationHandler = { [weak self] p in
            let status = p.terminationStatus
            DispatchQueue.main.async { self?.buildEnded(status) }
        }
        do {
            try p.run()
        } catch {
            fail("Couldn't start the build: \(error.localizedDescription)")
            return
        }
        build = p
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            let line = lastLogLine()
            if !line.isEmpty { self?.detail.stringValue = line }
        }
    }

    /// The build's exit status decides, not `--status`: sources edited while it ran (another
    /// agent at work) leave it `stale` even though the build that just ended succeeded.
    func buildEnded(_ status: Int32) {
        timer?.invalidate()
        build = nil
        if stopping {
            NSApp.terminate(nil)
            return
        }
        if status != 0 {
            fail("The FigCraft build failed.")
            return
        }
        launchApp()
    }

    /// Start FigCraft as this launcher's child (so it runs with the launcher's privacy
    /// permissions), hide the launcher, and quit when FigCraft quits.
    func launchApp() {
        window?.orderOut(nil)
        window = nil
        let p = Process()
        p.executableURL = URL(fileURLWithPath: appBinary)
        p.currentDirectoryURL = URL(fileURLWithPath: root)
        // Documents that arrived while launching or building open with it.
        p.arguments = pending
        pending = []
        // Remote control always on, like app.sh (9877 is EffectCraft's).
        var env = ProcessInfo.processInfo.environment
        env["FIGCRAFT_CONTROL_PORT"] = env["FIGCRAFT_CONTROL_PORT"] ?? "7979"
        p.environment = env
        let logURL = URL(fileURLWithPath: root + "/target/local/app.log")
        FileManager.default.createFile(atPath: logURL.path, contents: nil)
        if let out = try? FileHandle(forWritingTo: logURL) {
            p.standardOutput = out
            p.standardError = out
        }
        p.terminationHandler = { _ in DispatchQueue.main.async { NSApp.terminate(nil) } }
        do {
            try p.run()
        } catch {
            fail("Couldn't start FigCraft: \(error.localizedDescription)")
            return
        }
        appProcess = p
        NSApp.setActivationPolicy(.accessory)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { bringForward() }
    }

    /// A Dock click while FigCraft runs: bring it to the front.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if appProcess?.isRunning == true { bringForward() }
        return false
    }

    func fail(_ message: String) {
        window?.orderOut(nil)
        let alert = NSAlert()
        alert.messageText = message
        alert.informativeText = "The build log has the details."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Show Log")
        if alert.runModal() == .alertSecondButtonReturn {
            NSWorkspace.shared.open(URL(fileURLWithPath: log))
        }
        NSApp.terminate(nil)
    }

    /// Stop the build (app.sh stops cargo and every rustc it started), then quit.
    func stopBuild() {
        guard let p = build, p.isRunning else { return }
        stopping = true
        detail.stringValue = "Stopping…"
        p.terminate()
    }

    @objc func stopPressed() {
        if build?.isRunning == true { stopBuild() } else { NSApp.terminate(nil) }
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if build?.isRunning == true {
            stopBuild()
            return false
        }
        return true
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if build?.isRunning == true {
            // Cmd-Q or Quit from the Dock: stop the build first; buildEnded() quits.
            stopBuild()
            return .terminateCancel
        }
        return .terminateNow
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { appProcess == nil }
}

let app = NSApplication.shared
let delegate = Launcher()
app.delegate = delegate
app.setActivationPolicy(.regular)
// A menu with Quit, so Cmd-Q stops the build too.
let menu = NSMenu()
let appItem = NSMenuItem()
appItem.submenu = NSMenu()
appItem.submenu?.addItem(withTitle: "Quit FigCraft Launcher", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
menu.addItem(appItem)
app.mainMenu = menu
app.run()
