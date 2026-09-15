import Foundation
import NaturalLanguage

let documents = [
    "Receipt for a MacBook purchased from Apple in August.",
    "Slides introducing neural networks and supervised machine learning.",
    "A Python server that uses websockets to stream live chat messages.",
    "Notes describing database tables, primary keys, and schema migrations.",
    "Lecture notes on solving ordinary differential equations.",
    "Meeting notes about hiring designers and planning interviews.",
    "A recipe for sourdough bread with a three day fermentation.",
    "Travel itinerary with flights and hotels for a vacation in Japan.",
    "A quarterly revenue report and budget forecast.",
    "Instructions for compiling a Swift application in Xcode.",
    "Photographs of a beach sunset taken on a summer holiday.",
    "A project proposal for replacing the office network equipment."
]
let queries: [(String, Int)] = [
    ("invoice for my Apple laptop", 0),
    ("presentation about artificial intelligence", 1),
    ("Python project with real time socket communication", 2),
    ("how my database is structured", 3),
    ("PDF about differential equations", 4),
    ("candidates for the design team", 5),
    ("how to bake fermented bread", 6),
    ("my Japanese holiday bookings", 7)
]
let started = ContinuousClock.now
let exportJSON = CommandLine.arguments.contains("--json")
guard let model = NLEmbedding.sentenceEmbedding(for: .english) else {
    print("embedding_unavailable: conventional search must remain enabled")
    exit(0)
}
if !exportJSON { print("revision=\(model.revision) dimensions=\(model.dimension) initialization=\(started.duration(to: .now))") }
func unit(_ text: String) -> [Double]? {
    guard let vector = model.vector(for: text), vector.allSatisfy(\.isFinite) else { return nil }
    let norm = sqrt(vector.reduce(0) { $0 + $1 * $1 })
    guard norm > 0 else { return nil }
    return vector.map { $0 / norm }
}
let vectors = documents.map(unit)
if exportJSON {
    let queryVectors = queries.map { unit($0.0) }
    guard vectors.allSatisfy({ $0 != nil }), queryVectors.allSatisfy({ $0 != nil }) else { exit(1) }
    let data = try JSONSerialization.data(withJSONObject: [
        "revision": model.revision,
        "dimensions": model.dimension,
        "documents": vectors.compactMap { $0 },
        "queries": queryVectors.compactMap { $0 },
        "expected": queries.map(\.1)
    ], options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    exit(0)
}
var topOne = 0
var topThree = 0
let inferenceStart = ContinuousClock.now
for (index, pair) in queries.enumerated() {
    guard let vector = unit(pair.0) else { print("query=\(index) unavailable"); continue }
    let ranked = vectors.enumerated().compactMap { item -> (Int, Double)? in
        guard let candidate = item.element, candidate.count == vector.count else { return nil }
        return (item.offset, zip(vector, candidate).reduce(0) { $0 + $1.0 * $1.1 })
    }.sorted { $0.1 > $1.1 }
    if ranked.first?.0 == pair.1 { topOne += 1 }
    if ranked.prefix(3).contains(where: { $0.0 == pair.1 }) { topThree += 1 }
    print("query=\(index) expected=\(pair.1) ranked=\(ranked.prefix(3).map(\.0))")
}
print("top1=\(topOne)/\(queries.count) top3=\(topThree)/\(queries.count) query_embedding_and_ranking=\(inferenceStart.duration(to: .now))")
