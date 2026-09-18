# Build and work on Blindspot

For the downloaded app, start with [the installation and onboarding guide](docs/quick-start.md).
This page is for contributors working in the existing repository; no project scaffolding or
agent-specific configuration is required.

## Prerequisites

- An Apple silicon Mac running macOS 26 or later.
- Xcode 26 with its command-line tools selected. CI's exact toolchain pins live in
  [.github/workflows/ci.yml](.github/workflows/ci.yml).
- Rust/Cargo and `cbindgen` (`cargo install cbindgen --version 0.29.4 --locked`).
- A Developer ID identity for stable local signing, or explicit ad-hoc signing for a personal build.

```sh
git clone https://github.com/SeifBoukerdenna/blindspot.git
cd blindspot
make agent-context
make app
```

The Makefile builds the Rust workspace library and Swift shell directly; there is no Xcode
project. Dependencies are locked in Cargo.lock and the vector helper's own lockfile.

## Install a personal build

```sh
make install SIGN_ID=-
```

This builds, signs ad hoc, installs to `~/Applications/Blindspot.app`, and opens the app.
Ad-hoc signatures can cause macOS permissions to be requested again after rebuilding.
With your own identity, use `SIGN_ID="Developer ID Application: …"` instead. The repository's
default identity is configured in Makefile; it must exist in your keychain to use `make install`
without an override. Signing disables timestamps; builds are not notarized.

First launch guides the shortcut and optional document, clipboard, Accessibility and Ollama
setup. These features are not prerequisites for compiling or launching the app.

## Make and verify changes

Read [AGENTS.md](AGENTS.md) and the relevant [source map](docs/agent-map.md) section.
Preserve existing working-tree edits and user data. Tests use disposable fixtures; never repair,
rebuild or erase a live index as a way to make a test pass.

```sh
make agent-check PLAN=1
make agent-check
```

The helper chooses checks from changed paths. Focused targets include `test-onboarding`,
`test-containers`, `test-actions`, `smoke-panel`, and the search/helper checks in the source map.
Native UI fixtures need a logged-in macOS session; loopback fixtures need local socket access.
Scale/stress benchmarks are separate and are not routine validation.

For a maintainer app-code delivery with an existing signed installation, use:

```sh
make agent-deliver PLAN=1
make agent-deliver
```

This performs full verification, signing, packaging, a verified rollback archive, installation,
and installed-state checks. It does not bump the version or publish. See the
[delivery workflow](docs/agent-workflow.md) for requirements and recovery.

## Optional tools and publication

Codex CLI/IDE, Claude Code, and ordinary editors all use the same repository instructions.
[CLAUDE.md](CLAUDE.md) points to the shared agreement; optional hooks are reminders, not a
substitute for checks. No global agent configuration, hook trust or plugin setup is required.

[Media capture](media/README.md#capture-new-media) uses fictional data in an isolated home.
[Release instructions](docs/releasing.md) describe the user-run `./scripts/release.sh` command.
Commit and push the complete feature work, including new source files, before publishing.
