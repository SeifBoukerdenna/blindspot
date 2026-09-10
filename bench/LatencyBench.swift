import AppKit

/// End-to-end latency harness for the keystroke path.
///
/// The criterion bench in `core/benches/query.rs` covers `Ranker::rank`, which measured
/// 0.005ms out of a 3.261ms path — 0.15% of it. Everything else is AppKit, and criterion
/// cannot reach AppKit. This links the real `libblindspot_core.a` and imports the
/// shipping `Bridge.swift` and `ResultsView.swift`, so it measures the code that runs
/// rather than a copy that drifts.
///
/// Run with `make bench-shell`. Exits non-zero if p99 breaches the frame budget, so it is
/// a gate and not just a printout.
@main
@MainActor
enum LatencyBench {
    /// One frame at 60Hz. p99 is the number that matters here, not the mean — a launcher
    /// that is usually fast and occasionally drops a frame feels broken, and an average
    /// hides exactly that.
    static let budget = 0.016

    /// Prefixes of things actually installed, plus one that matches nothing, so the
    /// figures cover both a full result list and an empty one.
    static let queries = ["", "s", "sa", "saf", "te", "term", "termi", "x", "co", "zzzz"]
    static let iterations = 300

    static func main() {
        NSApplication.shared.setActivationPolicy(.accessory)

        let startedInit = DispatchTime.now().uptimeNanoseconds
        guard let core = Core() else {
            FileHandle.standardError.write(Data("bench: bs_init returned NULL\n".utf8))
            exit(1)
        }
        let initSeconds = elapsed(since: startedInit)

        // Warmed outside the timed loop deliberately: that is the M2 change under test.
        // Synchronously, unlike the app, because the harness needs a known-warm cache
        // before it can measure steady state. The figure printed is the total work the
        // app spreads across main-loop turns.
        let startedWarm = DispatchTime.now().uptimeNanoseconds
        IconCache.warmNow(core.allPaths())
        let warmSeconds = elapsed(since: startedWarm)

        let results = ResultsView(capacity: max(core.maxResults, 1))
        let container = NSView()
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 400),
            styleMask: [.nonactivatingPanel, .borderless],
            backing: .buffered, defer: false)
        container.addSubview(results)
        panel.contentView = container
        NSLayoutConstraint.activate([
            results.topAnchor.constraint(equalTo: container.topAnchor),
            results.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            results.trailingAnchor.constraint(equalTo: container.trailingAnchor),
        ])
        panel.orderFrontRegardless()

        // Steady state is what a typing user experiences; first-touch view setup is not.
        for query in queries { results.update(core.query(query, limit: core.maxResults).matches) }

        var ffi: [Double] = [], update: [Double] = [], reframe: [Double] = [], total: [Double] = []
        var lastHeight: CGFloat = -1

        for _ in 0..<iterations {
            for query in queries {
                let t0 = DispatchTime.now().uptimeNanoseconds
                let matches = core.query(query, limit: core.maxResults).matches
                let t1 = DispatchTime.now().uptimeNanoseconds
                results.update(matches)
                let t2 = DispatchTime.now().uptimeNanoseconds
                // Mirrors Panel.layoutForResults, unchanged-height guard included, so the
                // harness cannot flatter the app by skipping work the app does.
                let height = 58 + results.fittingHeight
                if height != lastHeight {
                    lastHeight = height
                    panel.setFrame(
                        NSRect(x: 0, y: 0, width: 640, height: height), display: true)
                }
                let t3 = DispatchTime.now().uptimeNanoseconds

                ffi.append(seconds(t0, t1))
                update.append(seconds(t1, t2))
                reframe.append(seconds(t2, t3))
                total.append(seconds(t0, t3))
            }
        }

        print("blindspot shell latency — \(total.count) renders over \(core.maxResults) rows\n")
        print(String(format: "  bs_init (config + sync scan)     %7.2f ms", initSeconds * 1000))
        print(String(format: "  icon warm (whole index, sync)    %7.2f ms", warmSeconds * 1000))
        print("")
        row("1 core.query (FFI + decode)", ffi)
        row("2 results.update", update)
        row("3 panel.setFrame", reframe)
        row("TOTAL", total)

        let over = total.filter { $0 > budget }.count
        print("")
        print("  budget 16.00 ms/frame — \(over) of \(total.count) renders over")

        guard percentile(total, 0.99) <= budget else {
            FileHandle.standardError.write(
                Data("bench: p99 breached the 16ms frame budget\n".utf8))
            exit(1)
        }
    }

    private static func row(_ label: String, _ samples: [Double]) {
        let name = label.padding(toLength: 32, withPad: " ", startingAt: 0)
        print(
            String(
                format: "  %@ median %7.3f ms   p99 %7.3f ms   max %7.3f ms", name,
                percentile(samples, 0.5) * 1000,
                percentile(samples, 0.99) * 1000,
                (samples.max() ?? 0) * 1000))
    }

    private static func percentile(_ samples: [Double], _ p: Double) -> Double {
        guard !samples.isEmpty else { return 0 }
        let sorted = samples.sorted()
        let index = Int((Double(sorted.count - 1) * p).rounded())
        return sorted[min(sorted.count - 1, max(0, index))]
    }

    private static func seconds(_ from: UInt64, _ to: UInt64) -> Double {
        Double(to &- from) / 1e9
    }

    private static func elapsed(since start: UInt64) -> Double {
        seconds(start, DispatchTime.now().uptimeNanoseconds)
    }
}
