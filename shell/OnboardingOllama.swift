import Foundation

struct SetupDownloadProgress: Sendable, Equatable {
    var status: String
    var completed: Int64 = 0
    var total: Int64 = 0
    var fraction: Double? { total > 0 ? min(1, max(0, Double(completed) / Double(total))) : nil }
}

private final class SetupRedirectGuard: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping @Sendable (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

actor SetupOllama {
    let endpoint: URL

    init(host: String) throws {
        guard let endpoint = Self.endpoint(host) else { throw SetupFailure("Choose a local Ollama address in Settings → AI. Setup never connects to a remote server.") }
        self.endpoint = endpoint
    }

    static func endpoint(_ host: String) -> URL? {
        guard let url = URL(string: "http://" + host), let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty, let port = parts.port, (1...65535).contains(port),
              let hostname = parts.host, ["localhost", "127.0.0.1", "[::1]", "::1"].contains(hostname) else { return nil }
        return URL(string: "http://\(hostname == "localhost" ? "127.0.0.1" : hostname):\(port)")
    }

    static func isLocal(_ object: [String: Any], capability: String) -> Bool {
        guard object["remote_host"] == nil || (object["remote_host"] as? String) == "",
              object["remote_model"] == nil || (object["remote_model"] as? String) == "",
              let details = object["details"] as? [String: Any],
              let format = details["format"] as? String, ["gguf", "safetensors"].contains(format.lowercased()),
              let info = object["model_info"] as? [String: Any], !info.isEmpty,
              let capabilities = object["capabilities"] as? [String], capabilities.contains(capability) else { return false }
        return true
    }

    func models() async throws -> [String] {
        let object = try await request("tags", timeout: 5)
        guard let models = object["models"] as? [[String: Any]], models.count <= 1024 else {
            throw SetupFailure("Ollama returned an unreadable model list. Update Ollama and try again.")
        }
        return models.compactMap {
            guard ($0["remote_host"] as? String ?? "").isEmpty,
                  ($0["remote_model"] as? String ?? "").isEmpty,
                  let name = $0["name"] as? String, name.utf8.count <= 128,
                  !name.lowercased().contains("cloud") else { return nil }
            return name
        }.sorted()
    }

    func verify(_ name: String, embedding: Bool = false) async throws {
        let object = try await request("show", body: ["model": name], timeout: 10)
        guard Self.isLocal(object, capability: embedding ? "embedding" : "completion") else {
            throw SetupFailure("This model could not be verified as a local \(embedding ? "embedding" : "answer") model. Choose another installed model or download the recommended one.")
        }
    }

    func test(_ name: String, embedding: Bool = false) async throws {
        try await verify(name, embedding: embedding)
        let object: [String: Any]
        if embedding {
            object = try await request("embed", body: ["model": name, "input": "A quiet walk beside the river.", "keep_alive": "1m"], timeout: 90)
            guard let embeddings = object["embeddings"] as? [[Double]], let vector = embeddings.first,
                  !vector.isEmpty, vector.count <= 16384, vector.allSatisfy(\.isFinite) else {
                throw SetupFailure("The search model did not return a usable embedding. Try another model.")
            }
        } else {
            object = try await request("generate", body: ["model": name, "prompt": "Reply with a short greeting in one sentence.",
                "stream": false, "think": false, "keep_alive": "1m", "options": ["num_ctx": 4096, "num_predict": 64]], timeout: 90)
            guard object["done"] as? Bool == true, let response = object["response"] as? String,
                  !response.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                throw SetupFailure("The model loaded but did not finish a response. Try again or choose a smaller model.")
            }
        }
        guard (object["remote_host"] as? String ?? "").isEmpty, (object["remote_model"] as? String ?? "").isEmpty else {
            throw SetupFailure("Ollama reported a remote response. This model cannot be used in local setup.")
        }
    }

    func pull(_ model: SetupModel, progress: @escaping @Sendable (SetupDownloadProgress) async -> Void) async throws {
        guard (SetupModel.choices + [SetupModel.embedding]).contains(model) else {
            throw SetupFailure("Only the displayed local model catalog can be downloaded through setup.")
        }
        var request = URLRequest(url: endpoint.appendingPathComponent("api/pull"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["model": model.id, "stream": true])
        request.timeoutInterval = 90
        let session = makeSession(resourceTimeout: 7200)
        defer { session.invalidateAndCancel() }
        let (bytes, response) = try await session.bytes(for: request)
        try check(response)
        var line = Data()
        var count = 0
        var succeeded = false
        var lastUpdate = Date.distantPast
        for try await byte in bytes {
            try Task.checkCancellation()
            count += 1
            guard count <= 64 * 1024 * 1024, line.count < 64 * 1024 else { throw SetupFailure("Download progress exceeded its safety limit. Check Ollama and retry.") }
            if byte != 10 { line.append(byte); continue }
            if line.isEmpty { continue }
            let object = try decode(line)
            line.removeAll(keepingCapacity: true)
            let raw = object["status"] as? String ?? "Downloading"
            let total = max(0, (object["total"] as? NSNumber)?.int64Value ?? 0)
            let completed = max(0, (object["completed"] as? NSNumber)?.int64Value ?? 0)
            if raw == "success" { succeeded = true }
            if Date().timeIntervalSince(lastUpdate) >= 0.15 || succeeded {
                lastUpdate = Date()
                let status = raw == "success" ? "Download complete" : raw.hasPrefix("pulling") ? "Downloading model layer" : raw.contains("verif") ? "Verifying download" : "Preparing model"
                await progress(SetupDownloadProgress(status: status, completed: completed, total: total))
            }
        }
        if !line.isEmpty {
            let object = try decode(line)
            succeeded = succeeded || object["status"] as? String == "success"
        }
        guard succeeded else { throw SetupFailure("The download ended before completion. Check your connection and retry; Ollama can reuse downloaded layers.") }
    }

    private func request(_ path: String, body: [String: Any]? = nil, timeout: Double) async throws -> [String: Any] {
        var request = URLRequest(url: endpoint.appendingPathComponent("api/" + path))
        request.timeoutInterval = timeout
        if let body {
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let session = makeSession(resourceTimeout: timeout)
        defer { session.invalidateAndCancel() }
        let (bytes, response) = try await session.bytes(for: request)
        try check(response)
        var data = Data()
        for try await byte in bytes {
            try Task.checkCancellation()
            guard data.count < 2 * 1024 * 1024 else { throw SetupFailure("Ollama's response exceeded the setup limit.") }
            data.append(byte)
        }
        return try decode(data)
    }

    private func makeSession(resourceTimeout: Double) -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.connectionProxyDictionary = [:]
        config.timeoutIntervalForRequest = 90
        config.timeoutIntervalForResource = resourceTimeout
        return URLSession(configuration: config, delegate: SetupRedirectGuard(), delegateQueue: nil)
    }

    private func check(_ response: URLResponse) throws {
        guard let response = response as? HTTPURLResponse, response.statusCode == 200 else {
            throw SetupFailure("Ollama could not complete this request. Check that it is running and up to date, and that the model is available. Then retry.")
        }
    }

    private func decode(_ data: Data) throws -> [String: Any] {
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw SetupFailure("Ollama returned an unreadable response. Update Ollama and retry.")
        }
        if object["error"] != nil { throw SetupFailure("Ollama reported a problem. Check free disk space and your connection, or try a smaller model. Existing models were kept.") }
        return object
    }
}
