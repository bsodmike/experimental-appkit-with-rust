# The whole workflow, for the Rust engine and the macOS app.
#
# Recipes are POSIX sh, so nothing here depends on which shell you use. For
# tab-completion in fish:
#
#     just --completions fish > ~/.config/fish/completions/just.fish
#
# Rust recipes work anywhere. Recipes that need Xcode say so when they cannot run.

set shell := ["/bin/sh", "-cu"]

app        := "Crustty"
mac_dir    := "native/macos"
project    := "native/macos/" + app + ".xcodeproj"
build_dir  := "native/macos/build"
cxx        := env_var_or_default("CXX", "c++")

# List the recipes.
default:
    @just --list --unsorted

# ---------------------------------------------------------------- setup

# What is installed, what is missing, and what this machine is.
doctor:
    #!/usr/bin/env sh
    printf '%-14s %s\n' "os" "$(uname -s) $(uname -m)"
    if command -v sw_vers >/dev/null 2>&1; then
        printf '%-14s %s\n' "macos" "$(sw_vers -productVersion)"
        printf '%-14s %s\n' "chip" "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
    fi
    for tool in cargo rustc just xcodegen xcodebuild watchexec; do
        if command -v "$tool" >/dev/null 2>&1; then
            # The path matters as much as the version: two copies of a tool on
            # PATH is a confusing afternoon, and this is where it shows up.
            printf '%-14s %-28s %s\n' "$tool" "$("$tool" --version 2>&1 | head -1)" \
                "$(command -v "$tool")"
        else
            printf '%-14s %s\n' "$tool" "MISSING -- run: just bootstrap"
        fi
    done
    printf '%-14s %s\n' "c++" "$({{cxx}} --version 2>&1 | head -1)"
    if command -v xcodebuild >/dev/null 2>&1; then
        printf '%-14s %s\n' "signing" "$(security find-identity -v -p codesigning 2>/dev/null | grep -c 'Developer ID Application') Developer ID identities"
    fi

# Install the tools the Mac side needs, and only the ones that are missing.
#
# `just` is deliberately not in the list: if this recipe is running then just is
# already installed, and brewing it again would put a second copy on PATH with
# the winner decided by ordering. A cargo-installed just is just as good.
bootstrap:
    #!/usr/bin/env sh
    set -eu
    missing=""
    for tool in xcodegen watchexec; do
        command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
    done
    if [ -z "$missing" ]; then
        echo "everything is already installed"
        exit 0
    fi
    if ! command -v brew >/dev/null 2>&1; then
        echo "Homebrew is not installed, and these are missing:$missing" >&2
        echo "See https://brew.sh, or install them however you prefer." >&2
        exit 1
    fi
    echo "installing:$missing"
    brew install $missing

# ---------------------------------------------------------------- rust

# Build the engine and regenerate the C header.
rust *args:
    cargo build -p terminal-ffi {{args}}

# The link flags the C side needs (PRD §14, gotcha 4).
native-libs:
    @cargo rustc -q -p terminal-ffi --lib -- --print native-static-libs 2>&1 | sed -n 's/^note: native-static-libs: //p' | tail -1

# ---------------------------------------------------------------- build

# Generate the Xcode project from project.yml. The .xcodeproj is disposable.
generate:
    #!/usr/bin/env sh
    set -eu
    if ! command -v xcodegen >/dev/null 2>&1; then
        echo "xcodegen is not installed -- run: just bootstrap" >&2
        exit 1
    fi
    # project.yml always references this file; a fresh clone has no local
    # signing identity, so an empty one keeps generation working.
    if [ ! -f {{mac_dir}}/Local.xcconfig ]; then
        echo "// No local signing identity. See Local.xcconfig.example." \
            > {{mac_dir}}/Local.xcconfig
    fi
    cd {{mac_dir}} && xcodegen generate --quiet

# Build the app (Debug). Cargo runs as a build phase, so this works from clean.
build: generate
    xcodebuild -project {{project}} -scheme {{app}} -configuration Debug build \
        CURRENT_PROJECT_VERSION="$(git rev-list --count HEAD)" | tail -5

# Build and run, with the app's log output in this terminal.
run: build
    exec {{build_dir}}/Debug/{{app}}.app/Contents/MacOS/{{app}}

# Rebuild and relaunch whenever a source file changes.
watch:
    watchexec -r -e rs,cpp,h,mm,yml -- just run

# Generate the project and open it in Xcode.
xcode: generate
    open {{project}}

# ---------------------------------------------------------------- test

# Everything that can be tested on this machine.
test: test-rust test-glue
    @if command -v xcodebuild >/dev/null 2>&1; then just test-mac; else echo "skipping test-mac: no Xcode"; fi

# The Rust workspace.
test-rust:
    cargo test --workspace

# The platform-free frontend core, linked against the real staticlib.
# This is the tier that runs on Linux, and it is deliberately the large one.
test-glue: (rust)
    #!/usr/bin/env sh
    set -eu
    libs=$(just native-libs)
    mkdir -p target
    {{cxx}} -std=c++17 -Wall -Wextra -Werror -O1 -o target/glue-tests \
        {{mac_dir}}/Tests/glue_tests.cpp {{mac_dir}}/Glue/*.cpp \
        -I{{mac_dir}}/Glue -Icrates/terminal-ffi/include \
        target/debug/libterminal_ffi.a ${libs}
    ./target/glue-tests

# The AppKit tests, which need a Mac.
test-mac: generate
    xcodebuild -project {{project}} -scheme {{app}} test | tail -5

# The C program that drives the whole boundary (PRD §14).
smoke:
    ./crates/terminal-ffi/examples/run-smoke.sh

# ---------------------------------------------------------------- ship

# A Release build, signed ad-hoc: it runs on this Mac and nowhere else.
release: generate
    cargo build -p terminal-ffi --release
    xcodebuild -project {{project}} -scheme {{app}} -configuration Release build \
        CURRENT_PROJECT_VERSION="$(git rev-list --count HEAD)" | tail -5
    @echo "built {{build_dir}}/Release/{{app}}.app"

# A Release build for both architectures. Needs the second Rust target:
#     rustup target add x86_64-apple-darwin
release-universal: generate
    #!/usr/bin/env sh
    set -eu
    cargo build -p terminal-ffi --release --target aarch64-apple-darwin
    cargo build -p terminal-ffi --release --target x86_64-apple-darwin
    mkdir -p target/universal/release
    lipo -create -output target/universal/release/libterminal_ffi.a \
        target/aarch64-apple-darwin/release/libterminal_ffi.a \
        target/x86_64-apple-darwin/release/libterminal_ffi.a
    xcodebuild -project {{project}} -scheme {{app}} -configuration Release build \
        ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO \
        RUST_LIB_DIR="$PWD/target/universal/release" \
        CURRENT_PROJECT_VERSION="$(git rev-list --count HEAD)" | tail -5

# A Release build signed with a Developer ID certificate.
# Set SIGN_IDENTITY and DEV_TEAM in native/macos/Local.xcconfig first.
release-signed: generate
    #!/usr/bin/env sh
    set -eu
    if ! grep -q "Developer ID" {{mac_dir}}/Local.xcconfig 2>/dev/null; then
        echo "no signing identity in {{mac_dir}}/Local.xcconfig" >&2
        echo "copy Local.xcconfig.example and fill it in; see:" >&2
        echo "    security find-identity -v -p codesigning" >&2
        exit 1
    fi
    cargo build -p terminal-ffi --release
    xcodebuild -project {{project}} -scheme {{app}} -configuration Release build \
        CURRENT_PROJECT_VERSION="$(git rev-list --count HEAD)" | tail -5
    codesign -dv --entitlements - {{build_dir}}/Release/{{app}}.app

# Package the Release build as a disk image.
dmg:
    #!/usr/bin/env sh
    set -eu
    test -d {{build_dir}}/Release/{{app}}.app || { echo "run: just release" >&2; exit 1; }
    rm -f {{build_dir}}/{{app}}.dmg
    hdiutil create -volname {{app}} -srcfolder {{build_dir}}/Release/{{app}}.app \
        -ov -format UDZO {{build_dir}}/{{app}}.dmg
    @echo "built {{build_dir}}/{{app}}.dmg"

# Submit the disk image to Apple and staple the ticket to it.
# Needs a keychain profile:
#     xcrun notarytool store-credentials crustty-notary --apple-id ... --team-id ...
notarize:
    #!/usr/bin/env sh
    set -eu
    test -f {{build_dir}}/{{app}}.dmg || { echo "run: just dmg" >&2; exit 1; }
    xcrun notarytool submit {{build_dir}}/{{app}}.dmg --keychain-profile crustty-notary --wait
    xcrun stapler staple {{build_dir}}/{{app}}.dmg
    xcrun stapler validate {{build_dir}}/{{app}}.dmg

# ---------------------------------------------------------------- upkeep

fmt:
    cargo fmt --all
    @command -v clang-format >/dev/null 2>&1 && clang-format -i {{mac_dir}}/Glue/*.cpp {{mac_dir}}/Glue/*.h {{mac_dir}}/Sources/*.mm {{mac_dir}}/Sources/*.h {{mac_dir}}/Tests/*.cpp || echo "clang-format not installed: skipping the native side"

lint:
    cargo clippy --workspace --all-targets -- -D warnings

clean:
    cargo clean
    rm -rf {{build_dir}} {{project}} target/glue-tests
