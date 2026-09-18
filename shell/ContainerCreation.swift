import AppKit
import SwiftUI

struct ContainerMount: Identifiable, Sendable {
    var id = UUID()
    var source = ""
    var destination = ""
    var volume = false
    var readOnly = true
    func argument() throws -> String {
        guard !source.isEmpty, destination.hasPrefix("/"), destination != "/",
              ![source, destination].contains(where: { $0.contains(",") || $0.contains("\"") || $0.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) }),
              volume ? ContainerEnvironment.validName(source.replacingOccurrences(of: "-", with: "_").replacingOccurrences(of: ".", with: "_")) : source.hasPrefix("/") else {
            throw ContainerFailure("Choose a mount source and an absolute container destination other than /. Commas and control characters are not supported.")
        }
        return "type=\(volume ? "volume" : "bind"),source=\(source),target=\(destination)" + (readOnly ? ",readonly" : "")
    }
}

struct ContainerVariable: Identifiable, Sendable {
    var id = UUID()
    var name = ""
    var value = ""
}

enum ContainerEnvironment {
    static func validName(_ value: String) -> Bool {
        value.range(of: #"^[A-Za-z_][A-Za-z0-9_]*$"#, options: .regularExpression) != nil && value.utf8.count <= 256
    }
    static func parse(_ data: Data) throws -> [String: String] {
        guard data.count <= 262_144, let text = String(data: data, encoding: .utf8), !text.contains("\0") else {
            throw ContainerFailure("Choose a UTF-8 environment file smaller than 256 KB.")
        }
        var values: [String: String] = [:]
        for (index, raw) in text.components(separatedBy: .newlines).enumerated() {
            let line = String(raw.drop(while: { $0 == " " || $0 == "\t" }))
            if line.isEmpty || line.hasPrefix("#") { continue }
            guard let equals = line.firstIndex(of: "="), validName(String(line[..<equals])) else {
                throw ContainerFailure("Environment line \(index + 1) must use NAME=value. Shell commands and inherited variables are not supported.")
            }
            values[String(line[..<equals])] = String(line[line.index(after: equals)...])
        }
        guard values.count <= 256 else { throw ContainerFailure("Use at most 256 environment variables.") }
        return values
    }
    static func merged(_ file: [String: String], overrides: [ContainerVariable]) throws -> [String: String] {
        guard file.allSatisfy({ validName($0.key) && !$0.value.contains("\n") && !$0.value.contains("\r") && !$0.value.contains("\0") }) else {
            throw ContainerFailure("Environment variables require valid names and single-line values.")
        }
        var result = file
        var seen = Set<String>()
        for variable in overrides {
            guard validName(variable.name), seen.insert(variable.name).inserted,
                  !variable.value.contains("\n"), !variable.value.contains("\r"), !variable.value.contains("\0") else {
                throw ContainerFailure("Use unique variable names and single-line values. Inline variables override the file.")
            }
            result[variable.name] = variable.value
        }
        _ = try parse(Data(result.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: "\n").utf8))
        return result
    }
}

@MainActor
final class ContainerCreation: ObservableObject {
    @Published var name = ""
    @Published var hostPort = ""
    @Published var containerPort = ""
    @Published var cpu = ""
    @Published var memory = ""
    @Published var restart = "no"
    @Published var variables: [ContainerVariable] = []
    @Published var mounts: [ContainerMount] = []
    @Published var fileValues: [String: String] = [:]
    @Published var fileName: String?
    @Published var error: String?
    @Published var review = false
    let image: LocalContainerImage
    let endpoint: ContainerEndpoint
    private(set) var sheet: NSWindow?
    private var completion: ((ContainerRunDraft?) -> Void)?
    init(image: LocalContainerImage, endpoint: ContainerEndpoint) { self.image = image; self.endpoint = endpoint }
    func draft() throws -> ContainerRunDraft {
        var value = ContainerRunDraft(name: name.trimmingCharacters(in: .whitespaces),
            hostPort: hostPort.isEmpty ? nil : Int(hostPort) ?? -1,
            containerPort: containerPort.isEmpty ? nil : Int(containerPort) ?? -1)
        value.environment = try ContainerEnvironment.merged(fileValues, overrides: variables)
        value.mounts = mounts; value.restart = restart
        value.cpus = cpu.isEmpty ? nil : Double(cpu) ?? -1
        value.memoryMiB = memory.isEmpty ? nil : Int(memory) ?? -1
        _ = try value.arguments(imageID: image.id)
        return value
    }
    func show(on parent: NSWindow, completion: @escaping (ContainerRunDraft?) -> Void) {
        self.completion = completion
        let sheet = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 620, height: 650), styleMask: [.titled, .fullSizeContentView], backing: .buffered, defer: false)
        sheet.titleVisibility = .hidden; sheet.titlebarAppearsTransparent = true
        sheet.isOpaque = false; sheet.backgroundColor = .clear; sheet.isReleasedWhenClosed = false
        let surface = SurfaceView(radius: 12)
        let host = NSHostingView(rootView: ContainerCreationView(model: self, palette: Theme.current))
        host.translatesAutoresizingMaskIntoConstraints = false; surface.addSubview(host)
        NSLayoutConstraint.activate([host.leadingAnchor.constraint(equalTo: surface.leadingAnchor), host.trailingAnchor.constraint(equalTo: surface.trailingAnchor), host.topAnchor.constraint(equalTo: surface.topAnchor), host.bottomAnchor.constraint(equalTo: surface.bottomAnchor)])
        sheet.contentView = surface; self.sheet = sheet
        parent.beginSheet(sheet)
    }
    func finish(_ create: Bool) {
        do {
            let result = create ? try draft() : nil
            if let sheet { sheet.sheetParent?.endSheet(sheet); sheet.orderOut(nil) }
            self.sheet = nil
            let callback = completion; completion = nil
            variables = []; fileValues = [:]; fileName = nil
            callback?(result)
        } catch { self.error = error.localizedDescription }
    }
    func reviewConfiguration() {
        do { _ = try draft(); error = nil; review = true } catch { self.error = error.localizedDescription }
    }
    func chooseEnvironment() {
        guard let sheet else { return }
        let panel = NSOpenPanel(); panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        panel.showsHiddenFiles = true; panel.message = "Choose a local NAME=value environment file"
        panel.beginSheetModal(for: sheet) { response in
            guard response == .OK, let url = panel.url else { return }
            // Read only a bounded prefix, even if the file grows after selection.
            do {
                let attrs = try url.resourceValues(forKeys: [.isRegularFileKey])
                guard attrs.isRegularFile == true else { throw ContainerFailure("Choose a regular environment file.") }
                let handle = try FileHandle(forReadingFrom: url); defer { try? handle.close() }
                self.fileValues = try ContainerEnvironment.parse(try handle.read(upToCount: 262_145) ?? Data())
                self.fileName = url.lastPathComponent; self.error = nil
            } catch { self.error = (error as? ContainerFailure)?.message ?? "The environment file could not be read." }
        }
    }
    func chooseMount() {
        guard let sheet else { return }
        let panel = NSOpenPanel(); panel.canChooseDirectories = true; panel.canChooseFiles = false
        panel.beginSheetModal(for: sheet) { response in
            guard response == .OK, let url = panel.url else { return }
            self.mounts.append(ContainerMount(source: url.path))
        }
    }
}

private struct ContainerCreationView: View {
    @ObservedObject var model: ContainerCreation
    let palette: Palette
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(model.review ? "Review your container" : "Create a container").font(.title2.weight(.semibold))
            Text("\(model.image.name) · \(model.endpoint.title)").foregroundStyle(Color(nsColor: palette.muted)).lineLimit(2)
            if let error = model.error { Text(error).foregroundStyle(Color(nsColor: palette.danger)) }
            ScrollView {
                if model.review { review } else { form }
            }
            Divider()
            HStack {
                Button("Cancel") { model.finish(false) }.keyboardShortcut(.cancelAction)
                Spacer()
                if model.review {
                    Button("Back") { model.review = false }
                    Button("Create and Start") { model.finish(true) }.keyboardShortcut(.defaultAction)
                } else { Button("Review configuration") { model.reviewConfiguration() }.keyboardShortcut(.defaultAction) }
            }
        }.padding(24).foregroundStyle(Color(nsColor: palette.ink)).tint(Color(nsColor: palette.accent))
    }
    private var form: some View {
        VStack(alignment: .leading, spacing: 16) {
            TextField("Container name", text: $model.name)
            Text("Ports").font(.headline)
            HStack { TextField("Mac port (optional)", text: $model.hostPort); Text("→"); TextField("Container port", text: $model.containerPort) }
            Text("Published to localhost only.").font(.caption)
            Divider()
            Text("Environment").font(.headline)
            HStack {
                Button("Choose .env file…") { model.chooseEnvironment() }
                if let name = model.fileName { Text(name).lineLimit(1); Button("Remove") { model.fileValues = [:]; model.fileName = nil } }
            }
            Text("NAME=value, one per line. Quotes and $ expressions are literal. Inline values take precedence. Values stay masked and are not saved as a profile.").font(.caption)
            if !model.fileValues.isEmpty { Text(model.fileValues.keys.sorted().joined(separator: ", ")).font(.caption).textSelection(.enabled) }
            ForEach($model.variables) { $variable in
                HStack { TextField("NAME", text: $variable.name); SecureField("Value", text: $variable.value); Button("Remove") { model.variables.removeAll { $0.id == variable.id } } }
            }
            Button("Add variable") { model.variables.append(ContainerVariable()) }.disabled(model.variables.count >= 256)
            Divider()
            Text("Storage").font(.headline)
            ForEach($model.mounts) { $mount in
                VStack(alignment: .leading) {
                    HStack { Text(mount.volume ? "Existing volume" : "Mac folder"); Spacer(); Button("Remove") { model.mounts.removeAll { $0.id == mount.id } } }
                    if mount.volume { TextField("Volume name", text: $mount.source) } else { Text(mount.source).font(.caption).lineLimit(2) }
                    TextField("Container path, e.g. /data", text: $mount.destination)
                    Toggle("Read only", isOn: $mount.readOnly)
                }
            }
            HStack { Button("Choose folder…") { model.chooseMount() }; Button("Use existing volume") { model.mounts.append(ContainerMount(volume: true)) } }.disabled(model.mounts.count >= 16)
            Text("The runtime must share the chosen Mac folder with its virtual machine.").font(.caption)
            Divider()
            Text("Resources & restart").font(.headline)
            HStack { TextField("CPU cores (optional)", text: $model.cpu); TextField("Memory in MiB (optional)", text: $model.memory) }
            Picker("Restart policy", selection: $model.restart) {
                ForEach(["no", "on-failure", "always", "unless-stopped"], id: \.self) { Text($0).tag($0) }
            }
            Text("Blank limits use the runtime defaults. Uses the image’s startup command; no image download.").font(.caption)
        }.textFieldStyle(.roundedBorder).padding(2)
    }
    private var review: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(model.name).font(.headline)
            Text(model.hostPort.isEmpty ? "No published ports" : "localhost:\(model.hostPort) → \(model.containerPort)")
            Text("Restart: \(model.restart) · CPU: \(model.cpu.isEmpty ? "default" : model.cpu) · Memory: \(model.memory.isEmpty ? "default" : model.memory + " MiB")")
            Text("Environment").font(.headline)
            ForEach(((try? model.draft().environment.keys.sorted()) ?? []), id: \.self) { name in
                Text("\(name) = ••••••••" + (model.variables.contains { $0.name == name } ? " (inline)" : " (file)"))
            }
            Text("Mounts").font(.headline)
            if model.mounts.isEmpty { Text("No additional mounts") }
            ForEach(model.mounts) { mount in Text("\(mount.source) → \(mount.destination) · \(mount.readOnly ? "read only" : "read/write")") }
            Text("Creating starts the workload with this configuration. Environment changes apply only to this new container.").font(.callout)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}
