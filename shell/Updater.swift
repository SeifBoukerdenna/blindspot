import AppKit
import CryptoKit
import Security

/// Installs the latest GitHub release of the repository named in Info.plist
/// (`BlindspotReleaseRepository`), only when asked from Settings → Status. Nothing checks in
/// the background.
///
/// What an update has to pass, in order:
/// - it comes over TLS from api.github.com and that repository's own release download URLs;
/// - the zip matches the SHA-256 in the release's checksum file, which catches truncated or
///   corrupted downloads (not a compromised account, which could replace both);
/// - the archive holds only `Blindspot-X.Y.Z/…` paths, checked before anything is extracted;
/// - the app inside has a valid code signature, the same bundle identifier, the release's version,
///   and the same Developer ID team as the installed copy.
///
/// A release signed ad hoc over a Developer ID copy is installed only after a warning. That is
/// what downloading the zip by hand does, but macOS then treats it as a new app for permissions.
/// A release signed by a different team is refused outright.
///
/// Self-contained, so bench/UpdaterTests.swift can compile it without the rest of the shell.
@MainActor
final class Updater {
    static let shared = Updater(bundle: .main)

    struct Version: Comparable, Sendable, CustomStringConvertible {
        let major, minor, patch: Int

        /// `X.Y.Z` or `vX.Y.Z` with plain ASCII digits; prereleases and build metadata are not updates.
        init?(_ text: String) {
            let trimmed = text.hasPrefix("v") ? text.dropFirst() : Substring(text)
            let parts = trimmed.split(separator: ".", omittingEmptySubsequences: false)
            guard parts.count == 3,
                  parts.allSatisfy({ !$0.isEmpty && $0.count <= 9 && $0.allSatisfy { $0.isASCII && $0.isNumber } }),
                  let major = Int(parts[0]), let minor = Int(parts[1]), let patch = Int(parts[2]) else { return nil }
            (self.major, self.minor, self.patch) = (major, minor, patch)
        }

        static func < (left: Version, right: Version) -> Bool {
            (left.major, left.minor, left.patch) < (right.major, right.minor, right.patch)
        }

        var description: String { "\(major).\(minor).\(patch)" }
    }

    struct Asset: Equatable, Sendable {
        let name: String
        let url: URL
        let size: Int
    }

    struct Release: Equatable, Sendable {
        let version: Version
        let tag: String
        let page: URL?
        let notes: String
        let archive: Asset
        let checksums: Asset

        /// The change list without the release's own title and install footer, short enough for an alert.
        var notesExcerpt: String {
            let body = notes.components(separatedBy: "### Install").first ?? notes
            let lines = body.split(separator: "\n", omittingEmptySubsequences: false)
                .filter { !$0.hasPrefix("## ") }
                .joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return lines.count > 700 ? String(lines.prefix(700)) + "…" : lines
        }
    }

    enum Trust: Equatable, Sendable {
        case sameDeveloper(team: String)
        /// The download is ad hoc, or the installed copy is; `installedTeam` is the copy's team, if any.
        case unverified(installedTeam: String?)
    }

    struct Prepared: Equatable, Sendable {
        let release: Release
        let staging: URL
        let app: URL
        let trust: Trust
    }

    enum State: Equatable {
        case idle
        case checking
        case current(Version)
        case available(Release)
        case downloading(Release)
        case ready(Prepared)
        case installing
        case failed(String)
    }

    struct Failure: LocalizedError, Equatable {
        let message: String
        init(_ message: String) { self.message = message }
        var errorDescription: String? { message }
    }

    nonisolated static let maximumArchiveBytes = 300 * 1_048_576

    let repository: String?
    let installed: Version?
    let bundle: URL
    let bundleID: String
    var onChange: (() -> Void)?
    private(set) var state: State = .idle {
        didSet { if state != oldValue { onChange?() } }
    }
    private let session: URLSession
    private var task: Task<Void, Never>?

    convenience init(bundle: Bundle) {
        self.init(
            repository: bundle.object(forInfoDictionaryKey: "BlindspotReleaseRepository") as? String,
            installed: (bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String).flatMap(Version.init),
            bundle: bundle.bundleURL,
            bundleID: bundle.bundleIdentifier ?? "")
    }

    init(repository: String?, installed: Version?, bundle: URL, bundleID: String) {
        self.repository = repository.flatMap { Self.validRepository($0) ? $0 : nil }
        self.installed = installed
        self.bundle = bundle
        self.bundleID = bundleID
        // Ephemeral: no cookies, cache or credentials are kept between checks.
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 900
        configuration.httpAdditionalHeaders = [
            "User-Agent": "Blindspot/\(installed?.description ?? "unknown")",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        ]
        session = URLSession(configuration: configuration)
    }

    // MARK: - What Settings shows

    var summary: String {
        let version = installed?.description ?? "unknown"
        switch state {
        case .idle:
            guard let repository else { return "Version \(version). This build has no release repository to update from." }
            return "Version \(version). Checks github.com/\(repository) for a newer release only when you press the button."
        case .checking: return "Asking GitHub for the latest release…"
        case .current(let latest):
            return latest == installed ? "Version \(version) is the latest release." : "Version \(version) is newer than the latest release (\(latest))."
        case .available(let release):
            return "Version \(release.version) is available; you have \(version). Installing downloads it, checks its checksum and signature, then asks before relaunching."
        case .downloading(let release): return "Downloading and verifying \(release.version)…"
        case .ready(let prepared): return "Version \(prepared.release.version) is downloaded and verified."
        case .installing: return "Installing. Blindspot reopens in a moment."
        case .failed(let message): return message
        }
    }

    var buttonTitle: String {
        switch state {
        case .idle: "Check for updates"
        case .checking: "Checking…"
        case .current: "Check again"
        case .available(let release): "Install \(release.version)"
        case .downloading: "Cancel"
        case .ready: "Install and relaunch"
        case .installing: "Installing…"
        case .failed: "Try again"
        }
    }

    // MARK: - Steps

    func check() {
        switch state {
        case .checking, .downloading, .installing: return
        default: break
        }
        guard let repository, let installed else {
            state = .failed("This build has no release repository or version, so it cannot update itself.")
            return
        }
        state = .checking
        task = Task { [weak self] in
            guard let self else { return }
            do {
                guard let url = URL(string: "https://api.github.com/repos/\(repository)/releases/latest") else {
                    throw Failure("The release repository name is not valid.")
                }
                let (data, response) = try await session.data(from: url)
                switch (response as? HTTPURLResponse)?.statusCode ?? 0 {
                case 200 where data.count <= 1_048_576: break
                case 404: throw Failure("\(repository) has no published release yet.")
                case 403, 429: throw Failure("GitHub is limiting requests from this network. Try again in a while.")
                case let status: throw Failure("GitHub answered with status \(status).")
                }
                let release = try Self.release(from: data, repository: repository)
                state = release.version > installed ? .available(release) : .current(release.version)
            } catch {
                state = Self.cancelled(error) ? .idle : .failed(Self.describe(error))
            }
        }
    }

    func download(_ release: Release) {
        guard case .available = state, let installed, release.version > installed else { return }
        if let problem = Self.locationProblem(bundle) {
            state = .failed(problem)
            return
        }
        state = .downloading(release)
        let installedTeam = (try? Self.signingTeam(of: bundle)) ?? nil
        let bundleID = bundleID
        // Beside the installed app, so the final swap is a rename on one volume.
        let staging = bundle.deletingLastPathComponent()
            .appendingPathComponent(".blindspot-update-\(UUID().uuidString)", isDirectory: true)
        task = Task { [weak self] in
            guard let self else { return }
            do {
                try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: false)
                let (sumsData, sumsResponse) = try await session.data(from: release.checksums.url)
                guard (sumsResponse as? HTTPURLResponse)?.statusCode == 200, sumsData.count <= 65_536,
                      let sums = String(data: sumsData, encoding: .utf8) else {
                    throw Failure("The release's checksum file could not be downloaded.")
                }
                let (temporary, response) = try await session.download(from: release.archive.url)
                let archive = staging.appendingPathComponent(release.archive.name)
                try FileManager.default.moveItem(at: temporary, to: archive)
                guard (response as? HTTPURLResponse)?.statusCode == 200,
                      try archive.resourceValues(forKeys: [.fileSizeKey]).fileSize == release.archive.size else {
                    throw Failure("The download was incomplete. Try again.")
                }
                try Task.checkCancellation()
                let prepared = try await Self.prepare(
                    archive: archive, sums: sums, release: release, bundleID: bundleID,
                    installedTeam: installedTeam, staging: staging)
                try Task.checkCancellation()
                state = .ready(prepared)
            } catch {
                try? FileManager.default.removeItem(at: staging)
                state = Self.cancelled(error) ? .available(release) : .failed(Self.describe(error))
            }
        }
    }

    func cancel() {
        task?.cancel()
    }

    func discard(_ prepared: Prepared) {
        try? FileManager.default.removeItem(at: prepared.staging)
        if case .ready = state { state = .available(prepared.release) }
    }

    /// Replaces the running app's bundle and relaunches. The running process keeps its already
    /// opened files, so it quits cleanly after the swap.
    func install(_ prepared: Prepared) {
        guard case .ready = state else { return }
        state = .installing
        do {
            try Self.replace(bundle, with: prepared.app)
            try? FileManager.default.removeItem(at: prepared.staging)
            LSRegisterURL(bundle as CFURL, true)
            try Self.relaunch(bundle)
            NSApp.terminate(nil)
        } catch {
            try? FileManager.default.removeItem(at: prepared.staging)
            state = .failed("Could not replace \(bundle.path): \(error.localizedDescription)")
        }
    }

    // MARK: - Pure steps (tested offline)

    nonisolated static func validRepository(_ name: String) -> Bool {
        name.range(of: #"^[A-Za-z0-9-]{1,39}/[A-Za-z0-9._-]{1,100}$"#, options: .regularExpression) != nil
    }

    nonisolated static func release(from data: Data, repository: String) throws -> Release {
        struct Payload: Decodable {
            struct Item: Decodable {
                let name: String
                let size: Int
                let browser_download_url: String
            }
            let tag_name: String
            let draft: Bool
            let prerelease: Bool
            let html_url: String?
            let body: String?
            let assets: [Item]
        }
        let payload: Payload
        do {
            payload = try JSONDecoder().decode(Payload.self, from: data)
        } catch {
            throw Failure("GitHub's description of the latest release could not be read.")
        }
        guard !payload.draft, !payload.prerelease else {
            throw Failure("The latest release is a draft or prerelease, which is not installed automatically.")
        }
        guard let version = Version(payload.tag_name), payload.tag_name == "v\(version)" else {
            throw Failure("The latest release tag \(payload.tag_name) is not a vX.Y.Z version.")
        }
        let prefix = "https://github.com/\(repository)/releases/download/\(payload.tag_name)/"
        func asset(_ name: String) throws -> Asset {
            guard let item = payload.assets.first(where: { $0.name == name }) else {
                throw Failure("Release \(payload.tag_name) has no \(name).")
            }
            guard item.browser_download_url == prefix + name, let url = URL(string: item.browser_download_url) else {
                throw Failure("\(name) is not hosted in \(repository)'s own releases, so it was refused.")
            }
            guard item.size > 0, item.size <= maximumArchiveBytes else {
                throw Failure("\(name) has an unexpected size.")
            }
            return Asset(name: name, url: url, size: item.size)
        }
        return Release(
            version: version, tag: payload.tag_name, page: payload.html_url.flatMap(URL.init(string:)),
            notes: payload.body ?? "", archive: try asset("Blindspot-\(version).zip"),
            checksums: try asset("Blindspot-\(version)-SHA256SUMS.txt"))
    }

    /// The SHA-256 for `name` in `shasum -a 256` output.
    nonisolated static func digest(for name: String, in sums: String) -> String? {
        for line in sums.split(whereSeparator: \.isNewline) {
            let fields = line.split(separator: " ", maxSplits: 1)
            guard fields.count == 2 else { continue }
            var file = fields[1].drop { $0 == " " }
            if file.first == "*" { file = file.dropFirst() }
            let hash = fields[0].lowercased()
            if file == name, hash.count == 64, hash.allSatisfy(\.isHexDigit) { return hash }
        }
        return nil
    }

    nonisolated static func sha256(of file: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: file)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// Verifies a downloaded release and extracts it into `staging`, returning the app to install.
    nonisolated static func prepare(
        archive: URL, sums: String, release: Release, bundleID: String, installedTeam: String?, staging: URL
    ) async throws -> Prepared {
        guard let expected = digest(for: release.archive.name, in: sums) else {
            throw Failure("The release's checksum file does not list \(release.archive.name).")
        }
        guard try sha256(of: archive) == expected else {
            throw Failure("The download does not match its published checksum, so it was discarded.")
        }

        let listing = staging.appendingPathComponent("entries.txt")
        try await run("/usr/bin/zipinfo", ["-1", archive.path], output: listing)
        let root = "Blindspot-\(release.version)/"
        let entries = try String(contentsOf: listing, encoding: .utf8).split(whereSeparator: \.isNewline)
        guard !entries.isEmpty, entries.allSatisfy({ entry in
            (entry.hasPrefix(root) || entry.hasPrefix("__MACOSX/"))
                && !entry.split(separator: "/").contains("..") && !entry.contains("\\")
        }) else {
            throw Failure("The download contains unexpected paths, so it was refused.")
        }

        let extracted = staging.appendingPathComponent("extracted", isDirectory: true)
        try FileManager.default.createDirectory(at: extracted, withIntermediateDirectories: true)
        try await run("/usr/bin/ditto", ["-x", "-k", archive.path, extracted.path])
        let app = extracted.appendingPathComponent(root + "Blindspot.app", isDirectory: true)
        let kind = try? app.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
        guard kind?.isDirectory == true, kind?.isSymbolicLink == false else {
            throw Failure("The download does not contain \(root)Blindspot.app.")
        }
        guard let info = NSDictionary(contentsOf: app.appendingPathComponent("Contents/Info.plist")),
              info["CFBundleIdentifier"] as? String == bundleID else {
            throw Failure("The downloaded app is not Blindspot (its bundle identifier differs), so it was refused.")
        }
        guard (info["CFBundleShortVersionString"] as? String).flatMap(Version.init) == release.version else {
            throw Failure("The downloaded app's version does not match release \(release.tag).")
        }

        let team = try signingTeam(of: app)
        let trust: Trust
        switch (installedTeam, team) {
        case let (installed?, downloaded?) where installed == downloaded:
            trust = .sameDeveloper(team: downloaded)
        case let (installed?, downloaded?):
            throw Failure("The download is signed by a different developer (team \(downloaded), not \(installed)), so it was refused.")
        default:
            trust = .unverified(installedTeam: installedTeam)
        }
        return Prepared(release: release, staging: staging, app: app, trust: trust)
    }

    /// Validates the whole bundle's signature, nested helpers included, and returns its Developer
    /// ID team, or nil when it is signed ad hoc.
    nonisolated static func signingTeam(of app: URL) throws -> String? {
        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(app as CFURL, SecCSFlags(), &code) == errSecSuccess, let code else {
            throw Failure("The app's code signature could not be read.")
        }
        let strict = SecCSFlags(rawValue: UInt32(kSecCSCheckAllArchitectures | kSecCSStrictValidate | kSecCSCheckNestedCode))
        let status = SecStaticCodeCheckValidity(code, strict, nil)
        guard status == errSecSuccess else {
            throw Failure("The app's code signature is not valid (\(status)), so it was refused.")
        }
        var information: CFDictionary?
        guard SecCodeCopySigningInformation(code, SecCSFlags(rawValue: UInt32(kSecCSSigningInformation)), &information) == errSecSuccess,
              let details = information as? [String: Any] else {
            throw Failure("The app's signing information could not be read.")
        }
        return details[kSecCodeInfoTeamIdentifier as String] as? String
    }

    /// Why installing in place cannot work from here, if it cannot.
    nonisolated static func locationProblem(_ bundle: URL) -> String? {
        if bundle.path.contains("/AppTranslocation/") {
            return "macOS is running Blindspot from a temporary read-only copy. Move Blindspot.app to your Applications folder, open it from there, then update."
        }
        guard bundle.pathExtension == "app" else {
            return "Only an installed Blindspot.app can update itself."
        }
        let folder = bundle.deletingLastPathComponent()
        guard FileManager.default.isWritableFile(atPath: folder.path) else {
            return "Blindspot cannot write to \(folder.path). Move it to a folder you own, such as Applications in your home folder."
        }
        return nil
    }

    nonisolated static func replace(_ installed: URL, with staged: URL) throws {
        _ = try FileManager.default.replaceItemAt(installed, withItemAt: staged, backupItemName: nil, options: [])
    }

    /// Opens the new copy once this process has exited, so its single-instance check never sees the old one.
    nonisolated static func relaunch(_ app: URL) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = [
            "-c", #"while /bin/kill -0 "$1" 2>/dev/null; do /bin/sleep 0.2; done; exec /usr/bin/open "$2""#,
            "blindspot-relaunch", String(getpid()), app.path,
        ]
        try process.run()
    }

    /// Output goes to a file rather than a pipe: a listing larger than the pipe buffer would
    /// block the child before it could exit.
    nonisolated static func run(_ tool: String, _ arguments: [String], output: URL? = nil) async throws {
        let sink: FileHandle
        if let output {
            FileManager.default.createFile(atPath: output.path, contents: nil)
            sink = try FileHandle(forWritingTo: output)
        } else {
            sink = FileHandle.nullDevice
        }
        defer { if output != nil { try? sink.close() } }
        let status: Int32 = try await withCheckedThrowingContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: tool)
            process.arguments = arguments
            process.standardOutput = sink
            process.standardError = FileHandle.nullDevice
            process.terminationHandler = { continuation.resume(returning: $0.terminationStatus) }
            do {
                try process.run()
            } catch {
                continuation.resume(throwing: error)
            }
        }
        guard status == 0 else {
            throw Failure("\((tool as NSString).lastPathComponent) could not read the download (status \(status)).")
        }
    }

    nonisolated static func cancelled(_ error: Error) -> Bool {
        error is CancellationError || (error as? URLError)?.code == .cancelled
    }

    nonisolated static func describe(_ error: Error) -> String {
        if let failure = error as? Failure { return failure.message }
        if let url = error as? URLError {
            switch url.code {
            case .notConnectedToInternet, .networkConnectionLost, .cannotFindHost, .cannotConnectToHost, .timedOut:
                return "Could not reach GitHub. Check your connection and try again."
            default: break
            }
        }
        return error.localizedDescription
    }
}
