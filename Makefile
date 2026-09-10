# blindspot — Rust core as a static library, thin Swift/AppKit shell.
#
# No .xcodeproj on purpose. The product is one binary, an Info.plist and a header;
# a project file would be a second source of truth for the build settings below.

APP        := Blindspot
BUNDLE_ID  := com.seifboukerdenna.blindspot
VERSION    := 0.1.0

BUILD      := build
BUNDLE     := $(BUILD)/$(APP).app
CONTENTS   := $(BUNDLE)/Contents
BINARY     := $(CONTENTS)/MacOS/blindspot

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
              -framework AppKit -framework Carbon \
              -L $(CORE_DIR)/target/release -lblindspot_core

.PHONY: all core header app sign run clean test bench bench-shell check

all: sign

core: $(CORE_LIB)

$(CORE_LIB): $(CORE_SRC) $(CORE_DIR)/Cargo.toml
	cargo build --release --manifest-path $(CORE_DIR)/Cargo.toml

header: $(HEADER)

$(HEADER): $(CORE_DIR)/src/ffi.rs $(CORE_DIR)/cbindgen.toml
	@mkdir -p include
	cbindgen --config $(CORE_DIR)/cbindgen.toml --crate blindspot_core --output $@ $(CORE_DIR)

app: $(BINARY)

$(BINARY): $(SWIFT_SRC) $(CORE_LIB) $(HEADER) shell/Info.plist
	@mkdir -p $(CONTENTS)/MacOS $(CONTENTS)/Resources
	swiftc $(SWIFTFLAGS) -o $@ $(SWIFT_SRC)
	sed -e 's/@BUNDLE_ID@/$(BUNDLE_ID)/g' -e 's/@VERSION@/$(VERSION)/g' \
	    shell/Info.plist > $(CONTENTS)/Info.plist

# Ad-hoc signature. Enough for personal use; notarization stays an open question per
# CLAUDE.md and is only needed once the app is handed to someone else.
#
# No --deep: Apple deprecated it for signing, and there is nothing nested to sign —
# one Mach-O and a plist. Signing is not conditional on the binary being newer,
# because a re-sign is cheap and a stale signature is a confusing failure mode.
sign: app
	codesign --force --sign - --identifier $(BUNDLE_ID) $(BUNDLE)

run: sign
	@pkill -x blindspot 2>/dev/null || true
	open $(BUNDLE)

test:
	cargo test --manifest-path $(CORE_DIR)/Cargo.toml

check:
	cargo clippy --manifest-path $(CORE_DIR)/Cargo.toml --all-targets -- -D warnings

bench:
	cargo bench --manifest-path $(CORE_DIR)/Cargo.toml

# The criterion bench above covers the Rust ranker, which measured 0.15% of the real
# keystroke path. This covers the other 99.85%, which is all AppKit. It links the
# shipping Bridge/ResultsView rather than a copy, and exits non-zero past the budget.
bench-shell: $(CORE_LIB) $(HEADER)
	@mkdir -p $(BUILD)
	swiftc $(SWIFTFLAGS) -o $(BUILD)/latency-bench \
	    $(BENCH_SRC) shell/Bridge.swift shell/ResultsView.swift
	@$(BUILD)/latency-bench

clean:
	cargo clean --manifest-path $(CORE_DIR)/Cargo.toml
	rm -rf $(BUILD)
