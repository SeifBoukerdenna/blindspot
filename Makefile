# blindspot — Rust core as a static library, thin Swift/AppKit shell.
#
# No .xcodeproj on purpose. The product has a launcher, native helpers and an Info.plist;
# a project file would be a second source of truth for the build settings below.

APP        := Blindspot
BUNDLE_ID  := com.seifboukerdenna.blindspot
VERSION    := 0.2.8

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
CORE_LIB   := $(CORE_DIR)/target/release/libblindspot_core.a
CORE_SRC   := $(shell find $(CORE_DIR)/src -name '*.rs')
HEADER     := include/blindspot.h
SWIFT_SRC  := $(wildcard shell/*.swift)
BENCH_SRC  := bench/LatencyBench.swift

# Pinned rather than inherited from the host SDK so swiftc cannot silently adopt an
# API that only exists on the machine that happened to build it.
#
# 26.0 because the panel is built on `NSGlassEffectView`, which is
# `API_AVAILABLE(macos(26.0))`. Liquid Glass is what "native" looks like on Tahoe, and
# an availability branch to keep a 14.0 floor would mean two visual paths to keep in
# sync in a single-user app that only ever runs on this machine.
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
              -L $(CORE_DIR)/target/release -lblindspot_core

.PHONY: all core header app sign package install version run clean test test-actions test-content test-semantic test-vectors smoke-panel bench bench-shell check check-header

all: sign

core: $(CORE_LIB)

$(CORE_LIB): $(CORE_SRC) $(CORE_DIR)/Cargo.toml $(CORE_DIR)/Cargo.lock Makefile
	MACOSX_DEPLOYMENT_TARGET=$(DEPLOY) cargo build --release --locked --manifest-path $(CORE_DIR)/Cargo.toml

header: $(HEADER)

$(HEADER): $(CORE_DIR)/src/ffi.rs $(CORE_DIR)/cbindgen.toml
	@mkdir -p include
	cbindgen --config $(CORE_DIR)/cbindgen.toml --crate blindspot_core --output $@ $(CORE_DIR)

app: $(BINARY) $(SEMANTIC) $(VECTORS) $(EXTRACT) $(RESOURCE_STAMP)

$(VECTORS): $(VECTOR_SRC) $(VECTOR_DIR)/Cargo.toml $(VECTOR_DIR)/Cargo.lock Makefile
	MACOSX_DEPLOYMENT_TARGET=$(DEPLOY) cargo build --release --locked --manifest-path $(VECTOR_DIR)/Cargo.toml
	@mkdir -p $(CONTENTS)/Helpers
	cp $(VECTOR_DIR)/target/release/blindspot-vector-worker $@

$(EXTRACT): helpers/ExtractWorker.swift Makefile
	@mkdir -p $(CONTENTS)/Helpers
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) -parse-as-library \
	    -framework PDFKit -o $@ helpers/ExtractWorker.swift

$(SEMANTIC): helpers/SemanticWorker.swift Makefile
	@mkdir -p $(CONTENTS)/Helpers
	swiftc -O -swift-version 6 -target $(ARCH)-apple-macos$(DEPLOY) -parse-as-library \
	    -framework NaturalLanguage -o $@ helpers/SemanticWorker.swift

$(BINARY): $(SWIFT_SRC) $(CORE_LIB) $(HEADER) shell/Info.plist Makefile
	@mkdir -p $(CONTENTS)/MacOS $(CONTENTS)/Resources
	swiftc $(SWIFTFLAGS) -o $@ $(SWIFT_SRC)
	sed -e 's/@BUNDLE_ID@/$(BUNDLE_ID)/g' -e 's/@VERSION@/$(VERSION)/g' \
	    shell/Info.plist > $(CONTENTS)/Info.plist

RESOURCE_FILES := $(shell find docs/licenses -type f -print 2>/dev/null)
$(RESOURCE_STAMP): scripts/package-resources.py core/Cargo.lock helpers/vector-worker/Cargo.lock \
		docs/new-features.md $(RESOURCE_FILES)
	python3 scripts/package-resources.py --root . --output $(BUILD)/release-licenses
	@mkdir -p $(CONTENTS)/Resources/ThirdPartyLicenses
	cp docs/new-features.md $(CONTENTS)/Resources/README.md
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

run: sign
	@pkill -x blindspot 2>/dev/null || true
	open $(BUNDLE)

test:
	cargo test --locked --manifest-path $(CORE_DIR)/Cargo.toml

test-actions: $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/action-tests bench/ActionTests.swift shell/Actions.swift shell/Bridge.swift shell/Context.swift shell/LocalRequest.swift shell/Schedule.swift
	$(BUILD)/action-tests

smoke-panel: $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/panel-smoke bench/PanelSmoke.swift \
	    $(filter-out shell/AppDelegate.swift,$(SWIFT_SRC))
	$(BUILD)/panel-smoke

test-content: $(BUILD)/content-tests
	$(BUILD)/content-tests

test-semantic: $(SEMANTIC)
	python3 bench/SemanticWorkerTests.py $(SEMANTIC)

test-vectors: $(VECTORS)
	python3 bench/VectorWorkerTests.py $(VECTORS)

$(BUILD)/content-tests: bench/ContentWatcherTests.swift shell/ContentWatcher.swift shell/Bridge.swift $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $@ bench/ContentWatcherTests.swift shell/ContentWatcher.swift shell/Bridge.swift

check:
	cargo clippy --locked --manifest-path $(CORE_DIR)/Cargo.toml --all-targets -- -D warnings

# Regenerates the header beside the committed one and fails on any difference, so an ffi.rs
# change cannot land with a stale C view of the ABI. The `$(HEADER)` rule would instead rewrite
# it silently, and only when checkout happened to leave ffi.rs with the newer mtime.
check-header:
	@mkdir -p $(BUILD)
	cbindgen --config $(CORE_DIR)/cbindgen.toml --crate blindspot_core --output $(BUILD)/blindspot.h $(CORE_DIR)
	diff -u $(HEADER) $(BUILD)/blindspot.h

bench:
	cargo bench --manifest-path $(CORE_DIR)/Cargo.toml

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
