import AppKit

@main
@MainActor
enum ContainerTests {
    static var checks = 0
    static func check(_ value: Bool, _ message: String) {
        precondition(value, message)
        checks += 1
    }
    static func main() {
        precondition(ProcessInfo.processInfo.environment["HOME"] == nil)
        NSApplication.shared.setActivationPolicy(.accessory)
        Task { @MainActor in
            do {
                try policy()
                try contrast()
                try await runtime()
                print("Containers: \(checks) assertions passed; local endpoint policy, bounded CLI/cancellation, exact-ID lifecycle guards, logs, native states and palette contrast. No live engines used.")
                exit(0)
            } catch { fatalError("Container fixture failed: \(error)") }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 60) { fatalError("Container fixture timed out") }
        NSApp.run()
    }
    static func policy() throws {
        let cli = URL(fileURLWithPath: "/fixture/docker")
        for value in ["tcp://localhost:2375", "ssh://host", "unix://remote/tmp/socket", "unix:///tmp/s?x=1", "unix:///tmp/s#x", "unix:///tmp/%00socket", "unix:", "http://127.0.0.1"] {
            check(ContainerEndpoint.localSocket(value) == nil, "Remote or ambiguous endpoint accepted")
        }
        check(ContainerEndpoint.localSocket("unix:///tmp/container.sock") != nil, "Local socket")
        let contexts = Data(#"""
{"Name":"desktop-local","DockerEndpoint":"unix:///tmp/docker.sock"}
{"Name":"production","DockerEndpoint":"ssh://example.invalid"}
"""#.utf8)
        let docker = try ContainerService.dockerEndpoints(contexts, executable: cli)
        check(docker.count == 1 && docker[0].name == "desktop-local", "Exclude remote contexts before connecting")
        let ordered = try ContainerService.dockerEndpoints(Data(#"[{"Name":"other","DockerEndpoint":"unix:///tmp/other.sock"},{"Name":"active","DockerEndpoint":"unix:///tmp/active.sock","Current":true}]"#.utf8), executable: cli)
        check(ordered.first?.name == "active", "Prefer the active local Docker context")
        let machines = Data(#"[{"Name":"fixture","ConnectionInfo":{"PodmanSocket":{"Path":"/tmp/podman machine/api.sock"}}}]"#.utf8)
        let podman = try ContainerService.podmanEndpoints(machines, executable: cli)
        check(podman.count == 1 && podman[0].socket.contains("%20"), "Podman local forwarded socket")
        let env = ContainerCommand.environment(["HOME":"/fixture", "DOCKER_HOST":"ssh://prod", "DOCKER_CONTEXT":"production", "DOCKER_CONFIG":"/secret", "CONTAINER_HOST":"ssh://prod", "CONTAINER_CONNECTION":"prod", "DYLD_INSERT_LIBRARIES":"/bad", "PATH":"/bad"])
        check(env["HOME"] == "/fixture" && env["DOCKER_HOST"] == nil && env["CONTAINER_HOST"] == nil && env["DOCKER_CONFIG"] == nil && env["DYLD_INSERT_LIBRARIES"] == nil, "Sanitize subprocess environment")
        for id in ["abc", String(repeating: "a", count: 63), "--all", String(repeating: "x", count: 64)] {
            check(!LocalContainer.validID(id), "Only full immutable IDs allowed")
        }
        check(ContainerRequest.command(":docker") == ":docker" && ContainerRequest.command("podman containers") == ":podman", "Discoverable aliases")
        check(ContainerRequest.command("docker") == nil && ContainerRequest.command(":docker rm") == nil, "Do not interpret shell input")
        for name in ["--privileged", "bad;name", "has space", "", "éclair"] {
            do { _ = try ContainerRunDraft(name: name, hostPort: nil, containerPort: nil).arguments(imageID: String(repeating: "b", count: 64)); fatalError("Invalid creation name accepted") }
            catch { checks += 1 }
        }
        for ports in [(0, 80), (65536, 80), (8080, -1)] {
            do { _ = try ContainerRunDraft(name: "harbor", hostPort: ports.0, containerPort: ports.1).arguments(imageID: String(repeating: "b", count: 64)); fatalError("Invalid port accepted") }
            catch { checks += 1 }
        }
        let fileEnv = try ContainerEnvironment.parse(Data("# comment\nMODE=file\nEMPTY=\nTOKEN=literal$VALUE\n".utf8))
        check(fileEnv["EMPTY"] == "" && fileEnv["TOKEN"] == "literal$VALUE", "Environment values stay literal")
        let merged = try ContainerEnvironment.merged(fileEnv, overrides: [ContainerVariable(name: "MODE", value: "inline")])
        check(merged["MODE"] == "inline", "Inline values override file values")
        for value in ["export TOKEN=value", "TOKEN", "BAD NAME=value", "A=bad\0value"] {
            do { _ = try ContainerEnvironment.parse(Data(value.utf8)); fatalError("Invalid environment accepted") } catch { checks += 1 }
        }
        var advanced = ContainerRunDraft(name: "valid", hostPort: nil, containerPort: nil)
        advanced.cpus = .infinity
        do { _ = try advanced.arguments(imageID: String(repeating: "b", count: 64)); fatalError("Invalid CPU accepted") } catch { checks += 1 }
        check(ContainerSample.byteCount("20MiB") == 20 * 1_048_576 && ContainerSample.byteCount("1.5 GB") == 1.5e9, "Metric units normalized")
        do { _ = try ContainerService.containers(Data(#"{"id":"--all"}"#.utf8)); fatalError("Unsafe ID accepted") }
        catch { checks += 1 }
    }
    static func contrast() throws {
        func rgb(_ c: NSColor) -> [Double] {
            let c = c.usingColorSpace(.sRGB)!
            return [c.redComponent, c.greenComponent, c.blueComponent].map(Double.init)
        }
        func mix(_ a: [Double], _ b: [Double], _ alpha: Double) -> [Double] { zip(a,b).map { $0 * alpha + $1 * (1-alpha) } }
        func luminance(_ rgb: [Double]) -> Double {
            zip(rgb.map { $0 <= 0.04045 ? $0/12.92 : pow(($0+0.055)/1.055,2.4) }, [0.2126,0.7152,0.0722]).map(*).reduce(0,+)
        }
        for palette in Palette.all {
            Theme.current = palette
            let view = SurfaceView(radius: 18)
            view.applyAccessibility(reduceTransparency: false, increaseContrast: false)
            let glass = NSColor(cgColor: view.subviews.last!.layer!.backgroundColor!)!
            check(!view.usesOpaqueFallback && glass.alphaComponent < 0.5, "Default surface retains transparent glass")
            for reduceTransparency in [false, true] {
                view.applyAccessibility(reduceTransparency: reduceTransparency, increaseContrast: !reduceTransparency)
                let tint = NSColor(cgColor: view.subviews.last!.layer!.backgroundColor!)!
                check(view.usesOpaqueFallback && tint.alphaComponent == 1, "Accessibility surface stays opaque")
                let surface = rgb(tint)
                for selected in [false, true] {
                    let background = selected ? mix(rgb(palette.selection), surface, Double(palette.selection.alphaComponent)) : surface
                    for color in [palette.ink, palette.inkSoft, palette.muted, palette.faint] {
                        let l1 = luminance(rgb(color)), l2 = luminance(background)
                        let ratio = (max(l1,l2)+0.05)/(min(l1,l2)+0.05)
                        check(ratio >= 4.5, "\(palette.name) accessibility text contrast \(ratio) must survive selection")
                    }
                }
            }
            view.applyAccessibility(reduceTransparency: false, increaseContrast: false)
            check(!view.usesOpaqueFallback && !view.subviews.first!.isHidden, "Disabling accessibility fallback restores native glass")
        }
    }
    static func runtime() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-containers-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let cli = root.appendingPathComponent("fixture")
        try FileManager.default.copyItem(at: URL(fileURLWithPath: "bench/ContainerFixture.py"), to: cli)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: cli.path)
        let endpoint = ContainerEndpoint(engine: .docker, name: "Fixture", socket: "unix:///tmp/fixture.sock", executable: cli)
        let rows = try await ContainerService.list(endpoint)
        check(rows.count == 1 && rows[0].running && rows[0].ports.contains("8080"), "Parse list and ports")
        let row = rows[0]
        let images = try await ContainerService.images(endpoint)
        check(images.count == 2 && images.first(where: { $0.id == String(repeating: "b", count: 64) })?.references.count == 2,
              "Group multiple tags by full image ID and retain dangling images")
        let image = images.first { $0.id == String(repeating: "b", count: 64) }!
        let draft = ContainerRunDraft(name: "fixture-new", hostPort: 8081, containerPort: 80)
        try await ContainerService.create(draft, image: image, endpoint: endpoint)
        let creationCalls = try String(contentsOf: root.appendingPathComponent("calls.jsonl"), encoding: .utf8)
        check(creationCalls.contains("127.0.0.1:8081:80") && creationCalls.contains("\"--pull\", \"never\"") && creationCalls.contains(image.id), "Creation uses only installed immutable image and localhost port")
        var advanced = draft
        advanced.environment = ["TOKEN": "fixture-secret", "MODE": "override"]
        advanced.cpus = 1.5; advanced.memoryMiB = 256; advanced.restart = "on-failure"
        advanced.mounts = [ContainerMount(source: "fixture_data", destination: "/data", volume: true)]
        try await ContainerService.create(advanced, image: image, endpoint: endpoint)
        check(try String(contentsOf: root.appendingPathComponent("env-result"), encoding: .utf8) == "validated", "Environment file private and consumed by CLI")
        func checkSecretCleanup() throws {
            let calls = try String(contentsOf: root.appendingPathComponent("calls.jsonl"), encoding: .utf8)
            check(!calls.contains("fixture-secret"), "Secret values never enter command arguments")
            for line in calls.split(separator: "\n") {
                let args = try JSONDecoder().decode([String].self, from: Data(line.utf8))
                if let index = args.firstIndex(of: "--env-file") { check(!FileManager.default.fileExists(atPath: args[index + 1]), "Temporary environment removed") }
            }
        }
        try checkSecretCleanup()
        try Data().write(to: root.appendingPathComponent("fail-create"))
        do { try await ContainerService.create(advanced, image: image, endpoint: endpoint); fatalError("Fixture creation should fail") } catch { checks += 1 }
        try checkSecretCleanup()
        try FileManager.default.removeItem(at: root.appendingPathComponent("fail-create"))
        try Data().write(to: root.appendingPathComponent("no-image"))
        do { try await ContainerService.create(draft, image: image, endpoint: endpoint); fatalError("Missing image accepted") }
        catch { checks += 1 }
        try FileManager.default.removeItem(at: root.appendingPathComponent("no-image"))
        let details = try await ContainerService.inspect(row, endpoint: endpoint)
        check(details.localPorts == [8080] && !details.fields.joined().contains("SECRET"), "Inspect exposes only selected metadata and local TCP links")
        let stats = try await ContainerService.stats(row, endpoint: endpoint)
        check(stats.contains("Memory"), "Bounded one-shot resource usage")
        _ = try await ContainerService.logs(row, endpoint: endpoint, lines: 1000)
        let logCalls = try String(contentsOf: root.appendingPathComponent("calls.jsonl"), encoding: .utf8)
        check(logCalls.contains("\"--tail\", \"1000\""), "Log limit reaches CLI")
        do { _ = try await ContainerService.logs(row, endpoint: endpoint, lines: -1); fatalError("Unbounded logs accepted") }
        catch { checks += 1 }
        let logs = try await ContainerService.logs(row, endpoint: endpoint)
        check(logs.contains("Ready on port 80") && logs.contains("sample error stream") && !logs.contains("\u{1b}"), "Both log streams, no ANSI escapes")
        try await ContainerService.mutate(.stop, container: row, endpoint: endpoint)
        let stopped = try await ContainerService.list(endpoint)
        check(stopped[0].state == "exited", "Stop exact ID")
        do { try await ContainerService.mutate(.restart, container: row, endpoint: endpoint); fatalError("Stale running state accepted") }
        catch { checks += 1 }
        try await ContainerService.mutate(.start, container: stopped[0], endpoint: endpoint)
        try await ContainerService.mutate(.restart, container: row, endpoint: endpoint)
        let calls = try String(contentsOf: root.appendingPathComponent("calls.jsonl"), encoding: .utf8)
        check(calls.contains("--host") && calls.contains(row.id) && !calls.contains("harbor-api"), "Pinned endpoint and identity; no display names in argv")
        let podman = ContainerEndpoint(engine: .podman, name: "Fixture", socket: endpoint.socket, executable: cli)
        let podRows = try await ContainerService.list(podman)
        check(podRows == rows, "Podman remote local-socket argv")
        check(try await ContainerService.images(podman) == images, "Podman images use local socket")
        try await ContainerService.create(advanced, image: image, endpoint: podman)
        try checkSecretCleanup()
        check(try await ContainerService.sample(row, endpoint: podman).memory == 20, "Podman metric response")
        try "missing".write(to: root.appendingPathComponent("state"), atomically: true, encoding: .utf8)
        do { try await ContainerService.mutate(.stop, container: row, endpoint: endpoint); fatalError("Missing target accepted") }
        catch { checks += 1 }
        for args in [["failure"], ["huge"], ["slow"]] {
            do { _ = try await ContainerCommand.run(cli, args, timeout: 0.2, cap: 32768); fatalError("Failure, overflow or timeout accepted") }
            catch { checks += 1 }
        }
        let cancel = Task { try await ContainerCommand.run(cli, ["slow"]) }
        try await Task.sleep(for: .milliseconds(50))
        cancel.cancel()
        do { _ = try await cancel.value; fatalError("Cancellation ignored") }
        catch is CancellationError { checks += 1 }
        try "running".write(to: root.appendingPathComponent("state"), atomically: true, encoding: .utf8)
        let ui = ContainerWindow()
        ui.endpoints = [endpoint, podman]; ui.endpointID = endpoint.id; ui.rows = rows; ui.selectedID = row.id
        ui.status = "1 container · 1 running"; ui.notice = ""; ui.logs = logs
        ui.show(discover: false)
        ui.filter = "8080 api"
        check(ui.filtered.count == 1, "Filter combines name and port")
        ui.selectContainer(row.id)
        check(ui.logs == nil, "Selection clears logs")
        ui.logs = logs
        ui.inspection = details; ui.resourceUsage = stats
        guard let window = ui.window else { fatalError("No container window") }
        NSApp.deactivate()
        check(window.isVisible && window.level == .floating && !window.hidesOnDeactivate, "Container workspace persists when focus changes")
        ui.setKeepOnTop(false)
        check(window.level == .normal && window.isVisible, "Unpin keeps the window open")
        ui.setKeepOnTop(true)
        ui.setLogLines(1000)
        check(ui.preferences.logLines == 1000 && ui.logs == nil, "Log line preference invalidates old output")
        ui.logs = logs
        for (name, palette) in [("light", Palette.parchment), ("dark", .nocturne)] {
            Theme.current = palette
            ui.show(discover: false)
            try await Task.sleep(for: .milliseconds(250))
            check(window.isVisible && !window.contentView!.hasAmbiguousLayout, "Native container layout")
            check(window.contentView is SurfaceView && !window.isOpaque, "Containers shares Blindspot glass")
            try snapshot(window, name: "containers-" + name)
        }
        ui.section = .images; ui.images = images; ui.selectedImageID = image.id; ui.filter = ""
        try await Task.sleep(for: .milliseconds(250))
        try snapshot(window, name: "container-images")
        let beforeCancel = try String(contentsOf: root.appendingPathComponent("calls.jsonl"), encoding: .utf8)
        ui.createContainer()
        try await Task.sleep(for: .milliseconds(150))
        check(window.attachedSheet != nil && ui.creation != nil, "Native creation sheet presented")
        ui.creation?.finish(false)
        try await Task.sleep(for: .milliseconds(150))
        check(try String(contentsOf: root.appendingPathComponent("calls.jsonl"), encoding: .utf8) == beforeCancel, "Cancelling creation never calls the CLI")
        ui.createContainer()
        try await Task.sleep(for: .milliseconds(150))
        guard let creationSheet = window.attachedSheet, let form = ui.creation else { fatalError("Creation sheet missing") }
        form.reviewConfiguration()
        check(!form.review && form.error != nil, "Invalid creation stays editable")
        form.name = "fixture-ui"; form.hostPort = "8082"; form.containerPort = "80"
        form.cpu = "1.5"; form.memory = "256"; form.error = nil
        form.fileValues = ["MODE":"file", "TOKEN":"fixture-secret"]
        form.variables = [ContainerVariable(name: "MODE", value: "override")]
        try await Task.sleep(for: .milliseconds(150))
        try snapshot(creationSheet, name: "container-create")
        form.reviewConfiguration()
        check(form.review && form.error == nil, "Review validates all advanced fields")
        try await Task.sleep(for: .milliseconds(150))
        try snapshot(creationSheet, name: "container-review")
        form.finish(true)
        for _ in 0..<100 {
            if !ui.busy && ui.section == .containers { break }
            try await Task.sleep(for: .milliseconds(25))
        }
        check(ui.error == nil && !ui.busy && ui.section == .containers && !ui.rows.isEmpty, "Confirmed creation refreshes containers using fixture CLI")
        ui.startMonitoring(); ui.followLogs()
        for _ in 0..<80 {
            if !ui.samples.isEmpty && !ui.events.isEmpty && (ui.logs?.contains("Ready") ?? false) { break }
            try await Task.sleep(for: .milliseconds(50))
        }
        check(!ui.samples.isEmpty && !ui.events.isEmpty, "Live metrics and events load")
        check(ui.followingLogs && ui.logs?.contains("Ready") == true, "Follow loads streaming output")
        ui.logQuery = "Ready"
        check(ui.filteredLogText.contains("Ready") && !ui.filteredLogText.contains("sample error"), "Search filters loaded output")
        ui.pauseLogs()
        check(ui.logExportText(filtered: true).contains("Paused") && ui.logExportText(filtered: true).contains("filtered view"), "Export identifies paused filtered output")
        ui.preferences.logLines = 50
        ui.appendLog(String(repeating: "entry\n", count: 500))
        check(ui.logTruncated && ui.logs!.components(separatedBy: "\n").count <= 101, "Live history is bounded")
        ui.logQuery = ""; ui.stopMonitoring()
        let count = ui.samples.count
        try await Task.sleep(for: .milliseconds(200))
        check(!ui.monitoring && !ui.followingLogs && ui.samples.count == count, "Pause rejects stale monitoring updates")
        if let pid = Int32(try String(contentsOf: root.appendingPathComponent("stream.pid"), encoding: .utf8)) {
            check(kill(pid, 0) == -1, "Cancelled stream process terminated")
        }
        ui.detailTab = "Resources"
        try await Task.sleep(for: .milliseconds(200))
        try snapshot(window, name: "container-monitoring")
        ui.detailTab = "Logs"
        try await Task.sleep(for: .milliseconds(200))
        try snapshot(window, name: "container-logs")
        ui.detailTab = "Overview"
        ui.cancel()
        check(ui.rows.isEmpty && ui.logs == nil && !ui.busy, "Cancellation invalidates visible results")
        ui.error = "The local engine is unavailable. Open the runtime and refresh."
        try await Task.sleep(for: .milliseconds(250))
        try snapshot(window, name: "containers-unavailable")
        let close = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: .command,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, characters: "w", charactersIgnoringModifiers: "w", isARepeat: false, keyCode: 13)!
        check(window.performKeyEquivalent(with: close), "Command-W handled explicitly")
        check(!window.isVisible, "Close releases displayed data")
    }
    static func button(in view: NSView, title: String) -> NSButton? {
        if let button = view as? NSButton, button.title == title { return button }
        return view.subviews.lazy.compactMap { button(in: $0, title: title) }.first
    }
    static func editableFields(in view: NSView) -> [NSTextField] {
        if let field = view as? NSTextField, field.isEditable { return [field] }
        return view.subviews.flatMap(editableFields)
    }
    static func snapshot(_ window: NSWindow, name: String) throws {
        guard ProcessInfo.processInfo.environment["BLINDSPOT_CAPTURE_CONTAINERS"] == "1" else { return }
        let directory = URL(fileURLWithPath: "build/container-screens")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), directory.appendingPathComponent(name + ".png").path]
        try process.run(); process.waitUntilExit()
        check(process.terminationStatus == 0, "Native window capture")
    }
}
