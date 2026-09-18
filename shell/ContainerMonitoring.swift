import Foundation
import Darwin

struct ContainerStreamChunk: Sendable {
    let text: String
    let stderr: Bool
}

extension ContainerCommand {
    static func stream(_ endpoint: ContainerEndpoint, arguments: [String]) -> AsyncThrowingStream<ContainerStreamChunk, Error> {
        AsyncThrowingStream(bufferingPolicy: .bufferingNewest(128)) { continuation in
            let worker = Task.detached(priority: .utility) {
                do {
                    guard ContainerEndpoint.localSocket(endpoint.socket) != nil else { throw ContainerFailure("Only local engines are supported.") }
                    let process = Process(), output = Pipe(), errors = Pipe()
                    process.executableURL = endpoint.executable; process.arguments = endpoint.arguments + arguments
                    process.environment = environment(); process.standardInput = FileHandle.nullDevice
                    process.standardOutput = output; process.standardError = errors
                    defer {
                        if process.isRunning {
                            process.terminate()
                            let deadline = ProcessInfo.processInfo.systemUptime + 0.3
                            while process.isRunning && ProcessInfo.processInfo.systemUptime < deadline { usleep(10_000) }
                            if process.isRunning { Darwin.kill(process.processIdentifier, SIGKILL) }
                            process.waitUntilExit()
                        }
                        for pipe in [output, errors] { try? pipe.fileHandleForReading.close(); try? pipe.fileHandleForWriting.close() }
                    }
                    for pipe in [output, errors] {
                        let fd = pipe.fileHandleForReading.fileDescriptor
                        guard fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK) != -1 else { throw ContainerFailure("Could not prepare monitoring.") }
                    }
                    try Task.checkCancellation()
                    try process.run()
                    try? output.fileHandleForWriting.close(); try? errors.fileHandleForWriting.close()
                    var pending = [Data(), Data()]
                    var bytes = [UInt8](repeating: 0, count: 8192)
                    func emit(_ data: Data, _ index: Int) throws {
                        let value = ContainerStreamChunk(text: String(decoding: data, as: UTF8.self), stderr: index == 1)
                        if case .dropped = continuation.yield(value) { throw ContainerFailure("Output arrived faster than it could be displayed. Reconnect for recent history.") }
                    }
                    while true {
                        try Task.checkCancellation()
                        var received = false
                        for (index, pipe) in [output, errors].enumerated() {
                            let count = Darwin.read(pipe.fileHandleForReading.fileDescriptor, &bytes, bytes.count)
                            if count > 0 {
                                received = true; pending[index].append(contentsOf: bytes.prefix(count))
                                while let newline = pending[index].firstIndex(of: 10) {
                                    guard pending[index].distance(from: pending[index].startIndex, to: newline) <= 65_536 else { throw ContainerFailure("A log line exceeded 64 KB. Use runtime tools for this output.") }
                                    try emit(Data(pending[index][...newline]), index)
                                    pending[index].removeSubrange(...newline)
                                }
                                guard pending[index].count <= 65_536 else { throw ContainerFailure("A log line exceeded 64 KB. Use runtime tools for this output.") }
                            } else if count < 0 && errno != EAGAIN && errno != EINTR { throw ContainerFailure("Monitoring disconnected.") }
                        }
                        if !process.isRunning && !received {
                            for index in 0...1 where !pending[index].isEmpty { try emit(pending[index], index) }
                            guard process.terminationStatus == 0 else { throw ContainerFailure("Monitoring disconnected. Check the engine and reconnect.") }
                            break
                        }
                        if !received { usleep(25_000) }
                    }
                    continuation.finish()
                } catch { continuation.finish(throwing: error) }
            }
            continuation.onTermination = { @Sendable _ in worker.cancel() }
        }
    }
}

struct ContainerSample: Identifiable, Sendable {
    let id = UUID()
    let date = Date()
    let cpu: Double
    let memory: Double
    let summary: String
    static func parse(_ data: Data) throws -> Self {
        guard let object = try ContainerService.objects(data).first,
              let cpuText = object["cpu"] as? String,
              let cpu = Double(cpuText.replacingOccurrences(of: "%", with: "").trimmingCharacters(in: .whitespaces)), cpu.isFinite, cpu >= 0,
              let memory = object["memory"] as? String, let bytes = byteCount(String(memory.split(separator: "/").first ?? "")) else {
            throw ContainerFailure("Resource metrics are unavailable from this runtime.")
        }
        let net = ContainerService.clean(object["network"] as? String ?? "Unavailable")
        let disk = ContainerService.clean(object["disk"] as? String ?? "Unavailable")
        return Self(cpu: cpu, memory: bytes / 1_048_576,
            summary: "CPU \(cpuText) · Memory \(ContainerService.clean(memory))\nNetwork RX / TX: \(net) · Disk read / write: \(disk)")
    }
    static func byteCount(_ value: String) -> Double? {
        let value = value.trimmingCharacters(in: .whitespaces)
        let number = value.prefix { $0.isNumber || $0 == "." }
        guard let amount = Double(number), amount.isFinite, amount >= 0 else { return nil }
        let unit = value.dropFirst(number.count).trimmingCharacters(in: .whitespaces).lowercased()
        let factors: [String: Double] = ["b":1,"kb":1e3,"mb":1e6,"gb":1e9,"tb":1e12,"kib":1024,"mib":1_048_576,"gib":1_073_741_824,"tib":1_099_511_627_776]
        guard let factor = factors[unit] else { return nil }; return amount * factor
    }
}

extension ContainerService {
    static func sample(_ container: LocalContainer, endpoint: ContainerEndpoint) async throws -> ContainerSample {
        guard LocalContainer.validID(container.id), ContainerEndpoint.localSocket(endpoint.socket) != nil else { throw ContainerFailure("Invalid container target.") }
        let cpu = ".CPUPerc"
        let format = "{\"cpu\":{{json \(cpu)}},\"memory\":{{json .MemUsage}},\"network\":{{json .NetIO}},\"disk\":{{json .BlockIO}}}"
        let output = try await ContainerCommand.run(endpoint.executable, endpoint.arguments + ["stats", "--no-stream", "--format", format, container.id], timeout: 8)
        return try ContainerSample.parse(output.stdout)
    }
    static func event(_ text: String, id: String) -> String? {
        guard let data = text.data(using: .utf8), let item = try? objects(data).first else { return nil }
        let actor = item["Actor"] as? [String: Any] ?? [:]
        guard (actor["ID"] as? String ?? item["ID"] as? String ?? item["id"] as? String) == id else { return nil }
        let action = item["Action"] as? String ?? item["Status"] as? String ?? item["status"] as? String ?? ""
        guard ["start", "stop", "restart", "die", "died", "exit", "exited", "kill", "pause", "unpause", "remove", "destroy", "health_status"].contains(where: { action == $0 || action.hasPrefix($0 + ":") }) else { return nil }
        let time = item["time"] as? Double ?? (item["TimeNano"] as? Double).map { $0 / 1e9 } ?? (item["timeNano"] as? Double).map { $0 / 1e9 }
        let stamp = time.map { Date(timeIntervalSince1970: $0).formatted(date: .omitted, time: .standard) } ?? Date().formatted(date: .omitted, time: .standard)
        let health = action == "health_status" ? " · " + clean(item["HealthStatus"] as? String ?? "Unknown", limit: 64) : ""
        return "\(stamp) · \(clean(action, limit: 128))" + health
    }
    static func cleanLog(_ text: String) -> String {
        let filtered = text.replacingOccurrences(of: #"\x1B\[[0-?]*[ -/]*[@-~]"#, with: "", options: .regularExpression)
        return String(String.UnicodeScalarView(filtered.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) || $0 == "\n" || $0 == "\t" }))
    }
}
