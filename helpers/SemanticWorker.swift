import Foundation
import NaturalLanguage
import Darwin

private enum Operation: String, Decodable { case probe, embed }
private struct Request: Decodable {
    let version: Int
    let id: UInt64
    let operation: Operation
    let texts: [String]
}
private struct Model: Encodable {
    let identifier: String
    let revision: Int
    let dimensions: Int
}
private enum Failure: String, Encodable {
    case invalidRequest, unsupportedProtocol, inputTooLarge, truncatedFrame
    case modelUnavailable, embeddingUnavailable
}
private struct Response: Encodable {
    let version = 1
    let id: UInt64
    var model: Model?
    var vectors: [[Float]]?
    var error: Failure?
}

/// Mean-pooled `NLContextualEmbedding` vectors. The sentence embedding this replaced ranked
/// relevant documents 32nd–123rd on a real home-folder index; the contextual model ranked them
/// 7th–11th for the same queries, at a similar per-document cost. Assets are never requested:
/// a Mac without them degrades to lexical search instead of starting a download.
private final class Embeddings {
    private var attempted = false
    private var model: NLContextualEmbedding?

    func respond(to data: Data) -> Response {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(object.keys) == ["version", "id", "operation", "texts"],
              let request = try? JSONDecoder().decode(Request.self, from: data) else {
            return Response(id: 0, error: .invalidRequest)
        }
        guard request.version == 1 else { return Response(id: request.id, error: .unsupportedProtocol) }
        guard request.texts.count <= 8,
              request.texts.allSatisfy({ !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && $0.utf8.count <= 4096 }),
              request.texts.reduce(0, { $0 + $1.utf8.count }) <= 16_384 else {
            return Response(id: request.id, error: .inputTooLarge)
        }
        guard (request.operation == .probe && request.texts.isEmpty) ||
              (request.operation == .embed && !request.texts.isEmpty) else {
            return Response(id: request.id, error: .invalidRequest)
        }
        if !attempted {
            attempted = true
            if let candidate = NLContextualEmbedding(language: .english), candidate.hasAvailableAssets,
               (try? candidate.load()) != nil {
                model = candidate
            }
        }
        guard let model, (1...2048).contains(model.dimension), model.revision > 0 else {
            return Response(id: request.id, error: .modelUnavailable)
        }
        let metadata = Model(identifier: "apple-contextual-en", revision: model.revision, dimensions: model.dimension)
        if request.operation == .probe { return Response(id: request.id, model: metadata) }
        var vectors: [[Float]] = []
        for text in request.texts {
            guard let vector = pooled(model, text) else { return Response(id: request.id, error: .embeddingUnavailable) }
            vectors.append(vector)
        }
        return Response(id: request.id, model: metadata, vectors: vectors)
    }

    private func pooled(_ model: NLContextualEmbedding, _ text: String) -> [Float]? {
        guard let result = try? model.embeddingResult(for: text, language: .english) else { return nil }
        var sum = [Double](repeating: 0, count: model.dimension)
        var tokens = 0
        var malformed = false
        result.enumerateTokenVectors(in: text.startIndex..<text.endIndex) { vector, _ in
            guard vector.count == sum.count else { malformed = true; return false }
            for index in sum.indices { sum[index] += vector[index] }
            tokens += 1
            return true
        }
        guard !malformed, tokens > 0 else { return nil }
        let norm = sqrt(sum.reduce(0) { $0 + $1 * $1 })
        guard norm.isFinite, norm > 0 else { return nil }
        let unit = sum.map { Float($0 / norm) }
        return unit.allSatisfy(\.isFinite) ? unit : nil
    }
}

@main
private enum SemanticWorker {
    private static let maximumFrame = 65_536

    static func main() {
        let embeddings = Embeddings()
        var pending = Data()
        var input = [UInt8](repeating: 0, count: 4096)
        do {
            while true {
                // FileHandle.read(upToCount:) can wait for the requested count on pipes.
                let count = input.withUnsafeMutableBytes { Darwin.read(STDIN_FILENO, $0.baseAddress, $0.count) }
                if count < 0 {
                    if errno == EINTR { continue }
                    throw CocoaError(.fileReadUnknown)
                }
                if count == 0 { break }
                pending.append(contentsOf: input.prefix(count))
                while let newline = pending.firstIndex(of: 10) {
                    guard pending.distance(from: pending.startIndex, to: newline) <= maximumFrame else {
                        try send(Response(id: 0, error: .inputTooLarge)); exit(1)
                    }
                    let frame = Data(pending[..<newline])
                    pending.removeSubrange(...newline)
                    let response = autoreleasepool { embeddings.respond(to: frame) }
                    try send(response)
                }
                guard pending.count <= maximumFrame else {
                    try send(Response(id: 0, error: .inputTooLarge)); exit(1)
                }
            }
            if !pending.isEmpty { try send(Response(id: 0, error: .truncatedFrame)); exit(1) }
        } catch {
            FileHandle.standardError.write(Data("Semantic worker I/O unavailable\n".utf8))
            exit(1)
        }
    }

    private static func send(_ response: Response) throws {
        var data = try JSONEncoder().encode(response)
        guard data.count <= 262_144 else { throw CocoaError(.fileWriteUnknown) }
        data.append(10)
        try FileHandle.standardOutput.write(contentsOf: data)
    }
}
