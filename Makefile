# blindspot — Rust core as a static library, thin Swift/AppKit shell.
#
# No .xcodeproj on purpose. The product has a launcher, native helpers and an Info.plist;
# a project file would be a second source of truth for the build settings below.

APP        := Blindspot
BUNDLE_ID  := com.seifboukerdenna.blindspot
VERSION    := 0.3.2
# Where Settings → Status looks for updates (shell/Updater.swift). Forks set their own.
RELEASE_REPO ?= SeifBoukerdenna/blindspot

BUILD      := build
BUNDLE     := $(BUILD)/$(APP).app
CONTENTS   := $(BUNDLE)/Contents
BINARY     := $(CONTENTS)/MacOS/blindspot
RESOURCE_STAMP := $(CONTENTS)/Resources/.release-resources
SEMANTIC   := $(CONTENTS)/Helpers/blindspot-semantic
VECTORS    := $(CONTENTS)/Helpers/blindspot-vectors
EXTRACT    := $(CONTENTS)/Helpers/blindspot-extract
VECTOR_DIR := helpers/vector-worker
VECTOR_SRC := $(shell find $(VECTOR_DIR)/src -name '*.rs')

CORE_DIR   := core
# Since the retrieval crate split, core is a workspace member, so cargo writes to the
# workspace root's target directory and core/target holds only pre-split leftovers.
# Linking that stale path paired a freshly generated header with an old archive, which
# put BsResult's fields at different offsets on each side of the ABI.
TARGET_DIR := target
CORE_LIB   := $(TARGET_DIR)/release/libblindspot_core.a
CORE_SRC   := $(shell find $(CORE_DIR)/src -name '*.rs')
HEADER     := include/blindspot.h
SWIFT_SRC  := $(wildcard shell/*.swift)
BENCH_SRC  := bench/LatencyBench.swift

# Pinned rather than inherited from the host SDK so swiftc cannot silently adopt an
# API that only exists on the machine that happened to build it.
#
# The supported platform remains Tahoe; the shell uses native AppKit materials.
DEPLOY     := 26.0
ARCH       := arm64

# `cargo rustc -- --print native-static-libs` reports only -lSystem -lc -lm for this
# crate, all of which swiftc's clang driver links by default. Nothing extra needed.
# `-swift-version 6` on purpose: strict concurrency is what forces the Carbon callback
# to be an explicit `nonisolated` function with a checked `MainActor.assumeIsolated`,
# rather than a closure that happens to work.
SWIFTFLAGS := -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) \
              -import-objc-header $(HEADER) \
              -framework AppKit -framework Carbon -framework Vision -framework ServiceManagement \
              -framework Quartz -framework EventKit \
              -L $(TARGET_DIR)/release -lblindspot_core

.PHONY: all core header app sign package install version media run clean test test-retrieval test-actions test-updater test-ui test-index-dashboard bench-search test-content test-semantic test-vectors smoke-panel bench bench-shell check check-header

all: sign

core: $(CORE_LIB)

RETRIEVAL_SRC := $(shell find crates -name '*.rs')
$(CORE_LIB): $(CORE_SRC) $(RETRIEVAL_SRC) $(CORE_DIR)/Cargo.toml Cargo.lock Makefile
	MACOSX_DEPLOYMENT_TARGET=$(DEPLOY) cargo build --release --locked --manifest-path $(CORE_DIR)/Cargo.toml

header: $(HEADER)

$(HEADER): $(CORE_DIR)/src/ffi.rs $(wildcard $(CORE_DIR)/src/ffi/*.rs) $(CORE_DIR)/cbindgen.toml
	@mkdir -p include
	cbindgen --config $(CORE_DIR)/cbindgen.toml --crate blindspot_core --output $@ $(CORE_DIR)

app: $(BINARY) $(SEMANTIC) $(VECTORS) $(EXTRACT) $(RESOURCE_STAMP)

$(VECTORS): $(VECTOR_SRC) $(VECTOR_DIR)/Cargo.toml $(VECTOR_DIR)/Cargo.lock Makefile
	MACOSX_DEPLOYMENT_TARGET=$(DEPLOY) cargo build --release --locked --manifest-path $(VECTOR_DIR)/Cargo.toml
	@mkdir -p $(CONTENTS)/Helpers
	cp $(VECTOR_DIR)/target/release/blindspot-vector-worker $@

$(EXTRACT): helpers/ExtractWorker.swift helpers/OfficeExtract.swift Makefile
	@mkdir -p $(CONTENTS)/Helpers
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) -parse-as-library \
	    -framework PDFKit -larchive -o $@ helpers/ExtractWorker.swift helpers/OfficeExtract.swift

$(SEMANTIC): helpers/SemanticWorker.swift Makefile
	@mkdir -p $(CONTENTS)/Helpers
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) -parse-as-library \
	    -framework NaturalLanguage -o $@ helpers/SemanticWorker.swift

$(BINARY): $(SWIFT_SRC) $(CORE_LIB) $(HEADER) shell/Info.plist Makefile
	@mkdir -p $(CONTENTS)/MacOS $(CONTENTS)/Resources
	swiftc $(SWIFTFLAGS) -o $@ $(SWIFT_SRC)
	sed -e 's/@BUNDLE_ID@/$(BUNDLE_ID)/g' -e 's/@VERSION@/$(VERSION)/g' -e 's|@RELEASE_REPO@|$(RELEASE_REPO)|g' \
	    shell/Info.plist > $(CONTENTS)/Info.plist

RESOURCE_FILES := $(shell find docs/licenses -type f -print 2>/dev/null)
$(RESOURCE_STAMP): scripts/package-resources.py Cargo.lock helpers/vector-worker/Cargo.lock \
		docs/new-features.md shell/AppIcon.icns $(RESOURCE_FILES)
	python3 scripts/package-resources.py --root . --output $(BUILD)/release-licenses
	@mkdir -p $(CONTENTS)/Resources/ThirdPartyLicenses
	cp docs/new-features.md $(CONTENTS)/Resources/README.md
	cp shell/AppIcon.icns $(CONTENTS)/Resources/AppIcon.icns
	cp -R $(BUILD)/release-licenses/. $(CONTENTS)/Resources/ThirdPartyLicenses/
	@touch $@

# Developer ID, not ad-hoc, since clipboard history. An ad-hoc identity is the binary's
# hash, which changes on every build. `NSPasteboard.h` documents a pasteboard-access
# grant; if a future macOS enforces it and keys the grant to identity the way TCC does,
# an ad-hoc build would lose it on every rebuild. Future-proofing, not a fix: on macOS
# 26.4 no grant was ever requested. `make SIGN_ID=-` restores ad-hoc.
#
# --timestamp=none because Developer ID signing otherwise contacts Apple's timestamp
# server on every build: a network call CLAUDE.md rules out, and a build that fails
# offline. A secure timestamp only matters for notarization, and releases are not
# notarized: they are published through GitHub Releases alone.
#
# No --deep: sign the nested helpers explicitly before their enclosing bundle. Not
# conditional on the binary being newer, because a stale signature is a confusing way
# to fail and a re-sign is cheap.
SIGN_ID ?= Developer ID Application: seif boukerdenna (VZR89A8Z89)

sign: app
	codesign --force --sign "$(SIGN_ID)" --timestamp=none --identifier $(BUNDLE_ID).extract $(EXTRACT)
	codesign --force --sign "$(SIGN_ID)" --timestamp=none --identifier $(BUNDLE_ID).semantic $(SEMANTIC)
	codesign --force --sign "$(SIGN_ID)" --timestamp=none --identifier $(BUNDLE_ID).vectors $(VECTORS)
	codesign --force --sign "$(SIGN_ID)" --timestamp=none --identifier $(BUNDLE_ID) $(BUNDLE)

# The download: the signed app with the feature guide as START-HERE.md, plus the guide as a
# standalone cheatsheet. Not dependent on `sign`, so the Release workflow packages exactly the
# bundle it verified. The staging copy is removed so no second Blindspot.app lingers for
# LaunchServices to register.
DIST := $(BUILD)/$(APP)-$(VERSION)

package:
	@test -d $(BUNDLE) || { echo "error: $(BUNDLE) is missing; run make sign first" >&2; exit 1; }
	codesign --verify --deep --strict $(BUNDLE)
	rm -rf $(DIST) $(DIST).zip
	mkdir -p $(DIST)
	ditto $(BUNDLE) $(DIST)/$(APP).app
	cp docs/new-features.md $(DIST)/START-HERE.md
	cd $(BUILD) && ditto -c -k --sequesterRsrc --keepParent $(APP)-$(VERSION) $(APP)-$(VERSION).zip
	rm -rf $(DIST)
	cp docs/new-features.md $(DIST)-Cheatsheet.md

# Replaces ~/Applications/Blindspot.app with this build and relaunches it (scripts/install.sh).
install: sign
	scripts/install.sh $(BUNDLE)

version:
	@echo $(VERSION)

# Codex CLI/IDE entry points. No implicit publication or hook trust changes.
# SCOPE=tooling/docs/... narrows checks; BASE=<revision> includes committed changes;
# PLAN=1 prints the command plan without running it. Delivery always checks release scope.
export SCOPE BASE PLAN
.PHONY: agent-context agent-doctor agent-check agent-deliver test-agent-tools
agent-context:
	@python3 scripts/agent.py context

agent-doctor:
	@python3 scripts/agent.py doctor

agent-check:
	@python3 scripts/agent.py check

agent-deliver:
	@python3 scripts/agent.py deliver

test-agent-tools:
	@python3 bench/AgentWorkflowTests.py

# README screenshots and GIFs (scripts/capture-media.sh). The harness and helpers live in a plain
# directory, not an .app, so LaunchServices never registers a second Blindspot. The helpers sit
# where the core looks for them, beside the executable's parent, so demo PDFs and Word files index.
MEDIA_ROOT := $(BUILD)/media/Capture

media: $(CORE_LIB) $(HEADER)
	@mkdir -p $(MEDIA_ROOT)/MacOS $(MEDIA_ROOT)/Helpers
	swiftc $(SWIFTFLAGS) -o $(MEDIA_ROOT)/MacOS/media-capture bench/MediaCapture.swift \
	    $(filter-out shell/AppDelegate.swift,$(SWIFT_SRC))
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) -parse-as-library \
	    -framework PDFKit -larchive -o $(MEDIA_ROOT)/Helpers/blindspot-extract helpers/ExtractWorker.swift helpers/OfficeExtract.swift
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) -parse-as-library \
	    -framework NaturalLanguage -o $(MEDIA_ROOT)/Helpers/blindspot-semantic helpers/SemanticWorker.swift
	MACOSX_DEPLOYMENT_TARGET=$(DEPLOY) cargo build --release --locked --manifest-path $(VECTOR_DIR)/Cargo.toml
	cp $(VECTOR_DIR)/target/release/blindspot-vector-worker $(MEDIA_ROOT)/Helpers/blindspot-vectors
	scripts/capture-media.sh $(MEDIA_ROOT)/MacOS/media-capture

run: sign
	@pkill -x blindspot 2>/dev/null || true
	open $(BUNDLE)

test:
	cargo test --locked --manifest-path $(CORE_DIR)/Cargo.toml

# The pure retrieval crate: chunking now, ranking as it lands. Seconds, no index needed.
test-retrieval:
	cargo test --locked --manifest-path crates/retrieval/Cargo.toml

test-actions: $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/action-tests bench/ActionTests.swift shell/Actions.swift shell/Bridge.swift shell/Context.swift shell/LocalRequest.swift shell/Schedule.swift shell/TextCapture.swift
	$(BUILD)/action-tests

# Self-contained: the updater compiles alone, and its tests build real ad-hoc-signed fixture apps.
test-updater:
	@mkdir -p $(BUILD)
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) \
	    -o $(BUILD)/updater-tests bench/UpdaterTests.swift shell/Updater.swift
	$(BUILD)/updater-tests

smoke-panel: $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/panel-smoke bench/PanelSmoke.swift \
	    $(filter-out shell/AppDelegate.swift,$(SWIFT_SRC))
	env -u HOME $(BUILD)/panel-smoke

test-ui: test-index-dashboard smoke-panel

test-index-dashboard: $(CORE_LIB) $(HEADER)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/index-dashboard-tests bench/IndexDashboardTests.swift \
	    shell/IndexDashboard.swift shell/Bridge.swift shell/Theme.swift
	$(BUILD)/index-dashboard-tests

test-content: $(BUILD)/content-tests
	$(BUILD)/content-tests

.PHONY: test-passages
test-passages: $(EXTRACT)
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) \
	    -framework AppKit -framework Quartz -framework PDFKit -o $(BUILD)/passage-tests bench/PassageTests.swift shell/Preview.swift
	$(BUILD)/passage-tests $(abspath $(EXTRACT))
	python3 bench/OfficeExtractTests.py $(EXTRACT)

test-semantic: $(SEMANTIC)
	python3 bench/SemanticWorkerTests.py $(SEMANTIC)

test-vectors: $(VECTORS)
	python3 bench/VectorWorkerTests.py $(VECTORS)

$(BUILD)/content-tests: bench/ContentWatcherTests.swift shell/ContentWatcher.swift shell/Bridge.swift $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $@ bench/ContentWatcherTests.swift shell/ContentWatcher.swift shell/Bridge.swift

check:
	cargo clippy --locked --workspace --all-targets -- -D warnings

# Regenerates the header beside the committed one and fails on any difference, so an ffi.rs
# change cannot land with a stale C view of the ABI. The `$(HEADER)` rule would instead rewrite
# it silently, and only when checkout happened to leave ffi.rs with the newer mtime.
check-header:
	@mkdir -p $(BUILD)
	cbindgen --config $(CORE_DIR)/cbindgen.toml --crate blindspot_core --output $(BUILD)/blindspot.h $(CORE_DIR)
	diff -u $(HEADER) $(BUILD)/blindspot.h

bench:
	cargo bench --manifest-path $(CORE_DIR)/Cargo.toml

# Search quality against bench/search-queries.toml, read-only on the live index. Not in CI: it
# needs a real index of a real checkout. Point it elsewhere with `make bench-search DB=… HELPERS=…`,
# and add `BENCH_FLAGS=--lexical` to measure exact search alone.
DB       ?= $(HOME)/.local/share/blindspot/content.sqlite
HELPERS  ?= $(HOME)/Applications/$(APP).app/Contents/Helpers
BENCH_FLAGS ?=

bench-search:
	cargo run --release --locked --manifest-path $(CORE_DIR)/Cargo.toml --example search_bench -- \
	    "$(DB)" bench/search-queries.toml --helpers "$(HELPERS)" $(BENCH_FLAGS)

# The criterion bench above covers the Rust ranker, which measured 0.15% of the real
# keystroke path. This covers the other 99.85%, which is all AppKit. It links the
# shipping Bridge/ResultsView rather than a copy, and exits non-zero past the budget.
bench-shell: $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/latency-bench \
	    $(BENCH_SRC) shell/Bridge.swift shell/ResultsView.swift shell/Theme.swift
	@$(BUILD)/latency-bench

clean:
	cargo clean --manifest-path $(CORE_DIR)/Cargo.toml
	rm -rf $(BUILD)
