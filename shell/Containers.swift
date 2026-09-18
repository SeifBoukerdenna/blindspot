import Foundation
import Darwin

enum ContainerEngine: String, CaseIterable, Sendable, Identifiable {
    case docker, podman
    var id: String { rawValue }
    var title: String { self == .docker ? "Docker" : "Podman" }
    var candidates: [String] {
        let common = ["/opt/homebrew/bin/", "/usr/local/bin/", "/opt/podman/bin/"]
        return common.map { $0 + rawValue } + (self == .docker ? ["/Applications/Docker.app/Contents/Resources/bin/docker"] : [])
    }
}

struct ContainerFailure: LocalizedError, Sendable {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

struct ContainerEndpoint: Identifiable, Equatable, Sendable {
    let engine: ContainerEngine
    let name: String
    let socket: String
    let executable: URL
    var id: String { engine.rawValue + ":" + socket }
    var title: String { engine.title + " · " + name }
    var arguments: [String] {
        engine == .docker ? ["--host", socket] : ["--remote", "--url", socket]
    }
    static func localSocket(_ value: String) -> String? {
        guard value.utf8.count <= 1024, !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
              let parts = URLComponents(string: value), parts.scheme == "unix",
              parts.host == nil || parts.host == "", parts.user == nil, parts.password == nil,
              parts.port == nil, parts.query == nil, parts.fragment == nil,
              parts.path.hasPrefix("/"), parts.path.count > 1,
              !parts.path.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { return nil }
        return parts.string
    }
}

struct LocalContainer: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let image: String
    let state: String
    let status: String
    let ports: String
    var running: Bool { state == "running" }
    static func validID(_ value: String) -> Bool {
        value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    func allows(_ operation: ContainerOperation) -> Bool {
        switch operation {
        case .start: ["exited", "stopped", "created", "configured", "initialized"].contains(state)
        case .stop, .restart: running
        }
    }
}

struct LocalContainerImage: Identifiable, Equatable, Sendable {
    let id: String
    var references: [String]
    let size: String
    let created: String
    var name: String { references.first ?? "Untagged image" }
}

struct ContainerDetails: Sendable {
    let fields: [String]
    let localPorts: [Int]
}

struct ContainerRunDraft: Sendable {
    let name: String
    let hostPort: Int?
    let containerPort: Int?

    var environment: [String: String] = [:]
    var mounts: [ContainerMount] = []
    var restart = "no"
    var cpus: Double?
    var memoryMiB: Int?

    func arguments(imageID: String) throws -> [String] {
        guard LocalContainer.validID(imageID), !name.isEmpty, name.utf8.count <= 63,
              name.first?.isASCII == true, name.first?.isLetter == true || name.first?.isNumber == true,
              name.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 46, 95].contains($0) }),
              (hostPort == nil) == (containerPort == nil) else { throw ContainerFailure("Use a container name with letters, numbers, dots, underscores or hyphens, starting with a letter or number. Supply both ports or leave both empty.") }
        var arguments = ["run", "--detach", "--pull", "never", "--name", name]
        if let hostPort, let containerPort {
            guard (1...65535).contains(hostPort), (1...65535).contains(containerPort) else { throw ContainerFailure("Ports must be between 1 and 65535.") }
            arguments += ["--publish", "127.0.0.1:\(hostPort):\(containerPort)"]
        }
        guard ["no", "always", "unless-stopped", "on-failure"].contains(restart), mounts.count <= 16 else { throw ContainerFailure("Unsupported restart policy or too many mounts.") }
        _ = try ContainerEnvironment.merged(environment, overrides: [])
        arguments += ["--restart", restart]
        if let cpus {
            guard cpus.isFinite, (0.1...1024).contains(cpus) else { throw ContainerFailure("CPU limit must be between 0.1 and 1,024 cores.") }
            arguments += ["--cpus", String(cpus)]
        }
        if let memoryMiB {
            guard (16...1_048_576).contains(memoryMiB) else { throw ContainerFailure("Memory limit must be between 16 and 1,048,576 MiB.") }
            arguments += ["--memory", "\(memoryMiB)m"]
        }
        var targets = Set<String>()
        for mount in mounts {
            guard targets.insert(mount.destination).inserted else { throw ContainerFailure("Each mount needs a unique container destination.") }
            arguments += ["--mount", try mount.argument()]
        }
        return arguments + [imageID]
    }
}

struct ContainerPreferences: Codable {
    var logLines = 200
    var keepOnTop = true
    static let lineChoices = [50, 100, 200, 500, 1000, 5000]
    static var file: URL? {
        ProcessInfo.processInfo.environment["HOME"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0)
            .appendingPathComponent(".local/share/blindspot/container-preferences.json") }
    }
    static func read() -> Self {
        guard let file, let data = try? Data(contentsOf: file), data.count < 4096,
              var value = try? JSONDecoder().decode(Self.self, from: data) else { return Self() }
        if !lineChoices.contains(value.logLines) { value.logLines = 200 }
        return value
    }
    func save() {
        guard let file = Self.file, let data = try? JSONEncoder().encode(self) else { return }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: file, options: .atomic)
    }
}

enum ContainerOperation: String, Sendable, CaseIterable {
    case start, stop, restart
    var title: String { rawValue.capitalized }
}

struct ContainerCommandOutput: Sendable {
    let stdout: Data
    let stderr: Data
}

enum ContainerCommand {
    static func environment(_ source: [String: String] = ProcessInfo.processInfo.environment) -> [String: String] {
        var result = source.filter { ["HOME", "USER", "LOGNAME", "TMPDIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR"].contains($0.key) }
        result["PATH"] = "/opt/homebrew/bin:/opt/podman/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        result["LANG"] = "en_US.UTF-8"
        result["NO_COLOR"] = "1"
        return result
    }

    static func run(_ executable: URL, _ arguments: [String], timeout: Double = 8, cap: Int = 524_288) async throws -> ContainerCommandOutput {
        let worker = Task.detached(priority: .userInitiated) {
            try capture(executable, arguments, timeout: timeout, cap: cap)
        }
        return try await withTaskCancellationHandler { try await worker.value } onCancel: { worker.cancel() }
    }

    static func capture(_ executable: URL, _ arguments: [String], timeout: Double, cap: Int) throws -> ContainerCommandOutput {
        try Task.checkCancellation()
        let process = Process()
        let output = Pipe(), errors = Pipe()
        process.executableURL = executable
        process.arguments = arguments
        process.environment = environment()
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = output
        process.standardError = errors
        for pipe in [output, errors] {
            let fd = pipe.fileHandleForReading.fileDescriptor
            guard fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK) != -1 else {
                throw ContainerFailure("Could not prepare the container command.")
            }
        }
        defer {
            for pipe in [output, errors] {
                try? pipe.fileHandleForReading.close()
                try? pipe.fileHandleForWriting.close()
            }
        }
        do { try process.run() }
        catch { throw ContainerFailure("The container CLI could not be launched. Install or update the runtime, then refresh.") }
        try? output.fileHandleForWriting.close()
        try? errors.fileHandleForWriting.close()
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        var data = [Data(), Data()]
        var buffer = [UInt8](repeating: 0, count: 8192)
        do {
            while true {
                try Task.checkCancellation()
                guard ProcessInfo.processInfo.systemUptime < deadline else {
                    throw ContainerFailure("The runtime took too long. Check that it is running, then refresh. A requested change may already have reached the engine.")
                }
                var received = false
                for (index, pipe) in [output, errors].enumerated() {
                    let count = Darwin.read(pipe.fileHandleForReading.fileDescriptor, &buffer, buffer.count)
                    if count > 0 {
                        received = true
                        guard data[0].count + data[1].count + count <= cap else {
                            throw ContainerFailure("The runtime response exceeded the display limit. Use its own tools for larger output.")
                        }
                        data[index].append(contentsOf: buffer.prefix(count))
                    } else if count < 0 && errno != EAGAIN && errno != EINTR {
                        throw ContainerFailure("Could not read the runtime response. Refresh to check its current state.")
                    }
                }
                if !process.isRunning && !received { break }
                if !received { Thread.sleep(forTimeInterval: 0.015) }
            }
            guard process.terminationStatus == 0 else {
                throw ContainerFailure("The runtime could not complete the request. Check that this local engine is running and accessible. Refresh to check its current state before retrying.")
            }
            return ContainerCommandOutput(stdout: data[0], stderr: data[1])
        } catch {
            if process.isRunning { process.terminate() }
            let stop = ProcessInfo.processInfo.systemUptime + 0.3
            while process.isRunning && ProcessInfo.processInfo.systemUptime < stop { Thread.sleep(forTimeInterval: 0.01) }
            if process.isRunning { Darwin.kill(process.processIdentifier, SIGKILL) }
            process.waitUntilExit()
            throw error
        }
    }
}

struct ContainerDiscovery: Sendable {
    let endpoints: [ContainerEndpoint]
    let notices: [String]
}

enum ContainerService {
    static let format = #"{"id":{{json .ID}},"name":{{json .Names}},"image":{{json .Image}},"state":{{json .State}},"status":{{json .Status}},"ports":{{json .Ports}}}"#

    static func objects(_ data: Data) throws -> [[String: Any]] {
        guard data.count <= 524_288 else { throw ContainerFailure("Container metadata is too large.") }
        if data.isEmpty { return [] }
        if let objects = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] { return objects }
        return try data.split(separator: 10).filter { !$0.allSatisfy { $0 == 32 || $0 == 13 || $0 == 9 } }.map {
            guard let object = try JSONSerialization.jsonObject(with: Data($0)) as? [String: Any] else {
                throw ContainerFailure("The runtime returned an unsupported response. Update its CLI and retry.")
            }
            return object
        }
    }

    static func clean(_ value: String, limit: Int = 512) -> String {
        String(String.UnicodeScalarView(value.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) })).prefixString(limit)
    }

    static func dockerEndpoints(_ data: Data, executable: URL) throws -> [ContainerEndpoint] {
        try objects(data).prefix(64).sorted {
            ($0["Current"] as? Bool == true) && ($1["Current"] as? Bool != true)
        }.compactMap { object in
            guard let host = object["DockerEndpoint"] as? String, let socket = ContainerEndpoint.localSocket(host),
                  let name = object["Name"] as? String else { return nil }
            return ContainerEndpoint(engine: .docker, name: clean(name), socket: socket, executable: executable)
        }
    }

    static func podmanEndpoints(_ data: Data, executable: URL) throws -> [ContainerEndpoint] {
        try objects(data).prefix(32).compactMap { object in
            guard let connection = object["ConnectionInfo"] as? [String: Any],
                  let podmanSocket = connection["PodmanSocket"] as? [String: Any],
                  let path = podmanSocket["Path"] as? String, path.hasPrefix("/"),
                  let name = object["Name"] as? String else { return nil }
            var parts = URLComponents()
            parts.scheme = "unix"
            parts.path = path
            guard let value = parts.string, let socket = ContainerEndpoint.localSocket(value) else { return nil }
            return ContainerEndpoint(engine: .podman, name: clean(name), socket: socket, executable: executable)
        }
    }

    static func discover() async -> ContainerDiscovery {
        var endpoints: [ContainerEndpoint] = [], notices: [String] = []
        for engine in ContainerEngine.allCases {
            guard !Task.isCancelled else { break }
            guard let path = engine.candidates.first(where: FileManager.default.isExecutableFile(atPath:)) else {
                notices.append("\(engine.title) CLI not found in standard installation locations.")
                continue
            }
            let executable = URL(fileURLWithPath: path)
            do {
                if engine == .docker {
                    let output = try await ContainerCommand.run(executable, ["context", "ls", "--format", "{{json .}}"])
                    let found = try dockerEndpoints(output.stdout, executable: executable)
                    endpoints += found
                    if found.isEmpty { notices.append("Docker has no local Unix-socket context. Remote contexts are not connected.") }
                } else {
                    let output = try await ContainerCommand.run(executable, ["machine", "list", "--format", "json"])
                    let names = try objects(output.stdout).prefix(32).compactMap { $0["Name"] as? String }.filter {
                        !$0.isEmpty && $0.utf8.count <= 128 && $0.utf8.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 95, 46].contains($0) } && !$0.hasPrefix("-")
                    }
                    if names.isEmpty { notices.append("No Podman machine found. Create and start one in Podman, then refresh.") }
                    else {
                        let inspected = try await ContainerCommand.run(executable, ["machine", "inspect"] + names)
                        endpoints += try podmanEndpoints(inspected.stdout, executable: executable)
                    }
                }
            } catch is CancellationError { break }
            catch { notices.append("\(engine.title) discovery failed. Check its installation and refresh.") }
        }
        var seen = Set<String>()
        return ContainerDiscovery(endpoints: endpoints.filter { seen.insert($0.id).inserted }, notices: notices)
    }

    static func list(_ endpoint: ContainerEndpoint, id: String? = nil) async throws -> [LocalContainer] {
        guard ContainerEndpoint.localSocket(endpoint.socket) != nil else { throw ContainerFailure("Only local Unix-socket engines are supported.") }
        var arguments = endpoint.arguments + ["ps", "--all", "--no-trunc", "--format", format]
        if let id {
            guard LocalContainer.validID(id) else { throw ContainerFailure("Invalid container identity.") }
            arguments += ["--filter", "id=" + id]
        }
        let output = try await ContainerCommand.run(endpoint.executable, arguments)
        return try containers(output.stdout)
    }

    static func containers(_ data: Data) throws -> [LocalContainer] {
        let values = try objects(data)
        guard values.count <= 2000 else { throw ContainerFailure("This engine has too many containers for this view.") }
        var seen = Set<String>()
        return try values.map { object in
            guard let id = object["id"] as? String, LocalContainer.validID(id), seen.insert(id).inserted,
                  let image = object["image"] as? String, let state = object["state"] as? String,
                  let status = object["status"] as? String, let ports = object["ports"] as? String else {
                throw ContainerFailure("The container list has unsupported or ambiguous metadata. Update the runtime and refresh.")
            }
            let name = (object["name"] as? String) ?? (object["name"] as? [String])?.joined(separator: ", ") ?? String(id.prefix(12))
            return LocalContainer(id: id, name: clean(name), image: clean(image), state: clean(state).lowercased(), status: clean(status), ports: clean(ports, limit: 4096))
        }.sorted { $0.running != $1.running ? $0.running : $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    static func mutate(_ operation: ContainerOperation, container: LocalContainer, endpoint: ContainerEndpoint) async throws {
        let current = try await list(endpoint, id: container.id)
        guard let latest = current.first(where: { $0.id == container.id }), latest.allows(operation) else {
            throw ContainerFailure("The container disappeared or changed state. Refresh before trying again.")
        }
        try Task.checkCancellation()
        var arguments = endpoint.arguments + [operation.rawValue]
        if operation != .start { arguments += ["--time", "10"] }
        arguments.append(container.id)
        _ = try await ContainerCommand.run(endpoint.executable, arguments, timeout: 25)
    }

    static func logs(_ container: LocalContainer, endpoint: ContainerEndpoint, lines: Int = 200) async throws -> String {
        guard LocalContainer.validID(container.id), ContainerEndpoint.localSocket(endpoint.socket) != nil else { throw ContainerFailure("Invalid container target.") }
        guard ContainerPreferences.lineChoices.contains(lines) else { throw ContainerFailure("Choose between 50 and 5,000 log lines.") }
        let output = try await ContainerCommand.run(endpoint.executable,
            endpoint.arguments + ["logs", "--tail", String(lines), "--timestamps", container.id], cap: 1_048_576)
        let text = String(decoding: output.stdout + output.stderr, as: UTF8.self)
        let filtered = text.replacingOccurrences(of: #"\x1B\[[0-?]*[ -/]*[@-~]"#, with: "", options: .regularExpression)
        return String(String.UnicodeScalarView(filtered.unicodeScalars.filter {
            !CharacterSet.controlCharacters.contains($0) || $0 == "\n" || $0 == "\t"
        }))
    }

    static func images(_ endpoint: ContainerEndpoint) async throws -> [LocalContainerImage] {
        guard ContainerEndpoint.localSocket(endpoint.socket) != nil else { throw ContainerFailure("Only local engines are supported.") }
        let format = #"{"id":{{json .ID}},"repository":{{json .Repository}},"tag":{{json .Tag}},"size":{{json .Size}},"created":{{json .CreatedAt}}}"#
        let output = try await ContainerCommand.run(endpoint.executable,
            endpoint.arguments + ["image", "ls", "--all", "--no-trunc", "--format", format])
        return try imageRows(output.stdout)
    }

    static func imageRows(_ data: Data) throws -> [LocalContainerImage] {
        let values = try objects(data)
        guard values.count <= 2000 else { throw ContainerFailure("This engine has too many images for this view.") }
        var rows: [String: LocalContainerImage] = [:]
        for object in values {
            guard let rawID = object["id"] as? String, let repo = object["repository"] as? String,
                  let tag = object["tag"] as? String, let size = object["size"] as? String,
                  let created = object["created"] as? String else { throw ContainerFailure("Unsupported image metadata.") }
            let id = rawID.hasPrefix("sha256:") ? String(rawID.dropFirst(7)) : rawID
            guard LocalContainer.validID(id) else { throw ContainerFailure("Invalid image identity.") }
            let reference = repo == "<none>" ? nil : clean(repo + (tag == "<none>" ? "" : ":" + tag))
            if var existing = rows[id] {
                if let reference, !existing.references.contains(reference) { existing.references.append(reference) }
                rows[id] = existing
            } else {
                rows[id] = LocalContainerImage(id: id, references: reference.map { [$0] } ?? [], size: clean(size), created: clean(created))
            }
        }
        return rows.values.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    static func inspect(_ container: LocalContainer, endpoint: ContainerEndpoint) async throws -> ContainerDetails {
        guard LocalContainer.validID(container.id), ContainerEndpoint.localSocket(endpoint.socket) != nil else { throw ContainerFailure("Invalid container target.") }
        let output = try await ContainerCommand.run(endpoint.executable,
            endpoint.arguments + ["container", "inspect", container.id])
        return try details(output.stdout, id: container.id)
    }

    static func details(_ data: Data, id: String) throws -> ContainerDetails {
        guard let item = try objects(data).first, (item["Id"] as? String ?? item["ID"] as? String) == id else {
            throw ContainerFailure("The container changed or is no longer available.")
        }
        let state = item["State"] as? [String: Any] ?? [:]
        let host = item["HostConfig"] as? [String: Any] ?? [:]
        let restart = host["RestartPolicy"] as? [String: Any] ?? [:]
        let network = item["NetworkSettings"] as? [String: Any] ?? [:]
        let networks = (network["Networks"] as? [String: Any] ?? [:]).keys.sorted()
        let health = state["Health"] as? [String: Any] ?? [:]
        var fields = ["Created: \(clean(item["Created"] as? String ?? "Unknown"))",
                      "Health: \(clean(health["Status"] as? String ?? "No health check"))",
                      "Restart policy: \(clean(restart["Name"] as? String ?? "Unknown"))",
                      "Networks: \(networks.map { clean($0) }.joined(separator: ", "))"]
        for key in ["Status", "StartedAt", "FinishedAt", "ExitCode", "OOMKilled"] {
            if let value = state[key] { fields.append("\(key): \(clean(String(describing: value)))") }
        }
        if let count = item["RestartCount"] { fields.append("Restarts: \(count)") }
        let config = item["Config"] as? [String: Any] ?? [:]
        for key in ["Entrypoint", "Cmd"] {
            if let value = config[key] as? [String] { fields.append("\(key): \(clean(value.joined(separator: " "), limit: 4096))") }
        }
        for key in ["Memory", "NanoCpus"] {
            if let value = host[key] { fields.append("\(key): \(value)") }
        }
        for mount in (item["Mounts"] as? [[String: Any]] ?? []).prefix(64) {
            fields.append("Mount: \(clean(mount["Source"] as? String ?? "")) → \(clean(mount["Destination"] as? String ?? ""))")
        }
        var ports = Set<Int>()
        for (key, bindings) in network["Ports"] as? [String: Any] ?? [:] where key.hasSuffix("/tcp") {
            for binding in bindings as? [[String: Any]] ?? [] {
                let host = binding["HostIp"] as? String ?? ""
                if ["", "0.0.0.0", "127.0.0.1", "::", "::1"].contains(host),
                   let value = binding["HostPort"] as? String, let port = Int(value), (1...65535).contains(port) { ports.insert(port) }
            }
        }
        return ContainerDetails(fields: fields, localPorts: ports.sorted())
    }

    static func stats(_ container: LocalContainer, endpoint: ContainerEndpoint) async throws -> String {
        guard container.running, LocalContainer.validID(container.id), ContainerEndpoint.localSocket(endpoint.socket) != nil else {
            throw ContainerFailure("Resource usage is available for running local containers.")
        }
        let cpu = endpoint.engine == .docker ? ".CPUPerc" : ".CPU"
        let format = "CPU: {{\(cpu)}} · Memory: {{.MemUsage}} · Network: {{.NetIO}} · Disk I/O: {{.BlockIO}}"
        let output = try await ContainerCommand.run(endpoint.executable,
            endpoint.arguments + ["stats", "--no-stream", "--format", format, container.id], timeout: 12)
        return clean(String(decoding: output.stdout, as: UTF8.self), limit: 4096)
    }

    static func create(_ draft: ContainerRunDraft, image: LocalContainerImage, endpoint: ContainerEndpoint) async throws {
        var arguments = try draft.arguments(imageID: image.id)
        let available = try await images(endpoint)
        guard available.contains(where: { $0.id == image.id }) else { throw ContainerFailure("The local image disappeared. Refresh before creating a container.") }
        try Task.checkCancellation()
        for mount in draft.mounts {
            if mount.volume {
                let output = try await ContainerCommand.run(endpoint.executable, endpoint.arguments + ["volume", "inspect", mount.source])
                guard try objects(output.stdout).contains(where: { $0["Name"] as? String == mount.source }) else { throw ContainerFailure("An existing volume is no longer available.") }
            } else {
                var directory: ObjCBool = false
                guard FileManager.default.fileExists(atPath: mount.source, isDirectory: &directory), directory.boolValue else { throw ContainerFailure("A selected bind folder is no longer available.") }
            }
        }
        // A private short-lived file avoids exposing values in argv or the CLI's own environment.
        // Always removed, including timeout and cancellation; never used as a saved profile.
        var secretDirectory: URL?
        defer { if let secretDirectory { try? FileManager.default.removeItem(at: secretDirectory) } }
        if !draft.environment.isEmpty {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-env-" + UUID().uuidString)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            secretDirectory = directory
            let file = directory.appendingPathComponent("environment")
            let data = Data(draft.environment.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: "\n").utf8)
            guard FileManager.default.createFile(atPath: file.path, contents: data, attributes: [.posixPermissions: 0o600]) else { throw ContainerFailure("Could not prepare the environment for this container.") }
            arguments.insert(contentsOf: ["--env-file", file.path], at: arguments.count - 1)
        }
        try Task.checkCancellation()
        _ = try await ContainerCommand.run(endpoint.executable, endpoint.arguments + arguments, timeout: 25)
    }
}

private extension String {
    func prefixString(_ count: Int) -> String { String(prefix(count)) }
}

enum ContainerRequest {
    static func command(_ text: String) -> String? {
        switch text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
        case ":containers", "containers": ":containers"
        case ":docker", "docker containers": ":docker"
        case ":podman", "podman containers": ":podman"
        default: nil
        }
    }
    static func engine(_ command: String) -> ContainerEngine? {
        command == ":docker" ? .docker : command == ":podman" ? .podman : nil
    }
}
