import AppKit

/// Offline tests for shell/Updater.swift: `make test-updater`. The fixture apps are real,
/// ad-hoc-signed bundles zipped exactly as `make package` zips a release, so the checksum, archive,
/// signature and swap steps run against the same kind of input a GitHub release provides.
@main
@MainActor
enum UpdaterTests {
    static let repository = "SeifBoukerdenna/blindspot"
    static let bundleID = "com.seifboukerdenna.blindspot"

    static func main() async {
        versions()
        releases()
        checksums()
        await pipeline()
        print("Updater: versions, release parsing and 4 refusals, checksums, verified install, and refusal of a bad checksum, bundle ID, version, signature and path: passed")
    }

    static func versions() {
        precondition(Updater.Version("v0.2.10")! > Updater.Version("0.2.9")!)
        precondition(Updater.Version("1.0.0")! > Updater.Version("0.99.99")!)
        precondition(Updater.Version("v1.2.3")?.description == "1.2.3")
        for invalid in ["1.2", "1.2.3-beta", "+1.2.3", "1.2.3.4", "v1..3", "１.2.3", ""] {
            precondition(Updater.Version(invalid) == nil, "\(invalid) must not parse")
        }
        precondition(Updater.validRepository(repository))
        precondition(!Updater.validRepository("owner/repo/../x") && !Updater.validRepository("owner"))
    }

    static func releaseJSON(tag: String = "v0.2.9", draft: Bool = false, base: String? = nil, archive: String = "Blindspot-0.2.9.zip") -> Data {
        let base = base ?? "https://github.com/\(repository)/releases/download/\(tag)/"
        let object: [String: Any] = [
            "tag_name": tag, "draft": draft, "prerelease": false,
            "html_url": "https://github.com/\(repository)/releases/tag/\(tag)",
            "body": "## Blindspot 0.2.9\n\n- **In-app updates.**\n\n### Install\n\nUnzip it.",
            "assets": [
                ["name": archive, "size": 5_624_517, "browser_download_url": base + archive],
                ["name": "Blindspot-0.2.9-SHA256SUMS.txt", "size": 182, "browser_download_url": base + "Blindspot-0.2.9-SHA256SUMS.txt"],
            ],
        ]
        return try! JSONSerialization.data(withJSONObject: object)
    }

    static func releases() {
        let release = try! Updater.release(from: releaseJSON(), repository: repository)
        precondition(release.version.description == "0.2.9" && release.archive.name == "Blindspot-0.2.9.zip")
        precondition(release.notesExcerpt == "- **In-app updates.**", release.notesExcerpt)
        for (reason, data) in [
            ("draft", releaseJSON(draft: true)),
            ("another host", releaseJSON(base: "https://evil.example/download/")),
            ("missing archive", releaseJSON(archive: "Other.zip")),
            ("non-version tag", releaseJSON(tag: "latest")),
        ] {
            precondition((try? Updater.release(from: data, repository: repository)) == nil, "a \(reason) release must be refused")
        }
    }

    static func checksums() {
        let hash = String(repeating: "ab", count: 32)
        let sums = "\(hash)  Blindspot-0.2.9.zip\n\(String(repeating: "cd", count: 32))  Blindspot-0.2.9-Cheatsheet.md\n"
        precondition(Updater.digest(for: "Blindspot-0.2.9.zip", in: sums) == hash)
        precondition(Updater.digest(for: "Blindspot-0.2.9-Cheatsheet.md", in: "\(hash.uppercased()) *Blindspot-0.2.9-Cheatsheet.md") == hash)
        precondition(Updater.digest(for: "missing.zip", in: sums) == nil)
        precondition(Updater.digest(for: "Blindspot-0.2.9.zip", in: "xyz  Blindspot-0.2.9.zip") == nil)
    }

    static func pipeline() async {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("blindspot-updater-tests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let applications = root.appendingPathComponent("Applications")
        try! FileManager.default.createDirectory(at: applications, withIntermediateDirectories: true)
        let release = try! Updater.release(from: releaseJSON(), repository: repository)

        // The installed copy: version 0.2.8.
        let installed = applications.appendingPathComponent("Blindspot.app")
        makeApp(at: installed, version: "0.2.8")
        precondition(Updater.locationProblem(installed) == nil)
        precondition(Updater.locationProblem(URL(fileURLWithPath: "/private/var/folders/x/AppTranslocation/y/Blindspot.app")) != nil)

        // A valid release verifies, and swapping it in leaves one app at the installed path.
        let (archive, sums) = makeRelease(in: root, name: "good")
        let staging = applications.appendingPathComponent(".blindspot-update-test", isDirectory: true)
        try! FileManager.default.createDirectory(at: staging, withIntermediateDirectories: false)
        let prepared = try! await Updater.prepare(
            archive: archive, sums: sums, release: release, bundleID: bundleID, installedTeam: nil, staging: staging)
        precondition(prepared.trust == .unverified(installedTeam: nil))
        try! Updater.replace(installed, with: prepared.app)
        precondition(version(of: installed) == "0.2.9")
        precondition((try? Updater.signingTeam(of: installed)) == .some(nil), "the swapped-in ad hoc app must still validate")
        precondition(!FileManager.default.fileExists(atPath: prepared.app.path))
        try! FileManager.default.removeItem(at: staging)

        // An ad hoc release over a Developer ID copy is allowed, but flagged for a warning.
        let (flaggedArchive, flaggedSums) = makeRelease(in: root, name: "flagged")
        let flagged = try! await Updater.prepare(
            archive: flaggedArchive, sums: flaggedSums, release: release, bundleID: bundleID,
            installedTeam: "ABCDE12345", staging: freshStaging(in: root))
        precondition(flagged.trust == .unverified(installedTeam: "ABCDE12345"))

        // Refusals.
        let wrongSum = sums.replacingOccurrences(of: String(sums.prefix(8)), with: "00000000")
        await refuses("a checksum mismatch") {
            _ = try await Updater.prepare(archive: archive, sums: wrongSum, release: release, bundleID: bundleID,
                                          installedTeam: nil, staging: freshStaging(in: root))
        }
        let (otherID, otherIDSums) = makeRelease(in: root, name: "other-id", bundleID: "com.example.other")
        await refuses("another bundle identifier") {
            _ = try await Updater.prepare(archive: otherID, sums: otherIDSums, release: release, bundleID: bundleID,
                                          installedTeam: nil, staging: freshStaging(in: root))
        }
        let (otherVersion, otherVersionSums) = makeRelease(in: root, name: "other-version", version: "0.3.0")
        await refuses("a version that differs from the tag") {
            _ = try await Updater.prepare(archive: otherVersion, sums: otherVersionSums, release: release, bundleID: bundleID,
                                          installedTeam: nil, staging: freshStaging(in: root))
        }
        let (tampered, tamperedSums) = makeRelease(in: root, name: "tampered", tamper: true)
        await refuses("a modified signed app") {
            _ = try await Updater.prepare(archive: tampered, sums: tamperedSums, release: release, bundleID: bundleID,
                                          installedTeam: nil, staging: freshStaging(in: root))
        }
        let traversal = root.appendingPathComponent("traversal.zip")
        run("/usr/bin/python3", ["-c", "import zipfile,sys; z=zipfile.ZipFile(sys.argv[1],'w'); z.writestr('Blindspot-0.2.9/../../escaped.txt','x'); z.close()", traversal.path])
        let traversalSums = "\(try! Updater.sha256(of: traversal))  Blindspot-0.2.9.zip\n"
        await refuses("an archive path that escapes its folder") {
            _ = try await Updater.prepare(archive: traversal, sums: traversalSums, release: release, bundleID: bundleID,
                                          installedTeam: nil, staging: freshStaging(in: root))
        }
        precondition(!FileManager.default.fileExists(atPath: root.appendingPathComponent("escaped.txt").path))
    }

    // MARK: - Fixtures

    static func makeApp(at url: URL, version: String, bundleID: String = bundleID) {
        let executables = url.appendingPathComponent("Contents/MacOS")
        try! FileManager.default.createDirectory(at: executables, withIntermediateDirectories: true)
        try! FileManager.default.copyItem(atPath: "/usr/bin/true", toPath: executables.appendingPathComponent("blindspot").path)
        let info: NSDictionary = [
            "CFBundleIdentifier": bundleID, "CFBundleShortVersionString": version, "CFBundleVersion": version,
            "CFBundleExecutable": "blindspot", "CFBundlePackageType": "APPL",
        ]
        precondition(info.write(to: url.appendingPathComponent("Contents/Info.plist"), atomically: true))
        run("/usr/bin/codesign", ["--force", "--sign", "-", url.path])
    }

    /// A release zip laid out like `make package`, with its checksum line.
    static func makeRelease(in root: URL, name: String, version: String = "0.2.9", bundleID: String = bundleID, tamper: Bool = false) -> (URL, String) {
        let folder = root.appendingPathComponent("release-\(name)/Blindspot-0.2.9")
        let app = folder.appendingPathComponent("Blindspot.app")
        makeApp(at: app, version: version, bundleID: bundleID)
        if tamper {
            let plist = app.appendingPathComponent("Contents/Info.plist")
            let info = NSMutableDictionary(contentsOf: plist)!
            info["Injected"] = true
            precondition(info.write(to: plist, atomically: true))
        }
        let archive = root.appendingPathComponent("Blindspot-\(name).zip")
        run("/usr/bin/ditto", ["-c", "-k", "--sequesterRsrc", "--keepParent", folder.path, archive.path])
        return (archive, "\(try! Updater.sha256(of: archive))  Blindspot-0.2.9.zip\n")
    }

    static func freshStaging(in root: URL) -> URL {
        let staging = root.appendingPathComponent("staging-\(UUID().uuidString)", isDirectory: true)
        try! FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        return staging
    }

    static func version(of app: URL) -> String? {
        NSDictionary(contentsOf: app.appendingPathComponent("Contents/Info.plist"))?["CFBundleShortVersionString"] as? String
    }

    static func refuses(_ reason: String, _ body: () async throws -> Void) async {
        do {
            try await body()
            preconditionFailure("\(reason) must be refused")
        } catch {}
    }

    static func run(_ tool: String, _ arguments: [String]) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try! process.run()
        process.waitUntilExit()
        precondition(process.terminationStatus == 0, "\(tool) failed")
    }
}
