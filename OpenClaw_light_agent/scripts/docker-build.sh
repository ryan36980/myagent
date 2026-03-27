#!/usr/bin/env bash
# OpenClaw Light — Docker build script
#
# Usage:
#   ./scripts/docker-build.sh                     # Build all 4 targets
#   ./scripts/docker-build.sh aarch64              # Build ARM64 only
#   ./scripts/docker-build.sh armv7                # Build ARMv7 only
#   ./scripts/docker-build.sh x86_64               # Build x86-64 only
#   ./scripts/docker-build.sh windows              # Build Windows exe only
#   ./scripts/docker-build.sh x86_64 windows       # Build Linux x64 + Windows
#   ./scripts/docker-build.sh --verify x86_64      # Build twice, compare SHA-256
#   ./scripts/docker-build.sh --verify             # Verify all 4 targets

set -euo pipefail

# ---------------------------------------------------------------------------
# Target mapping: short name -> MUSL_TARGET -> Rust triple
# ---------------------------------------------------------------------------
declare -A TARGET_MAP=(
    [aarch64]="aarch64-musl"
    [armv7]="armv7-musleabihf"
    [x86_64]="x86_64-musl"
    [windows]=""
)

declare -A TRIPLE_MAP=(
    [aarch64]="aarch64-unknown-linux-musl"
    [armv7]="armv7-unknown-linux-musleabihf"
    [x86_64]="x86_64-unknown-linux-musl"
    [windows]="x86_64-pc-windows-gnu"
)

ALL_TARGETS=(aarch64 armv7 x86_64 windows)
DIST_DIR="dist"
VERIFY=false

# Read version from Cargo.toml for release artifact naming.
VERSION="v$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
PROJECT="openclaw-light"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m==>\033[0m %s\n' "$*" >&2; exit 1; }

# Release artifact filename for a given short target name.
release_name() {
    local short="$1"
    local triple="${TRIPLE_MAP[$short]}"
    if [[ "${short}" == "windows" ]]; then
        echo "${PROJECT}-${VERSION}-${triple}.exe"
    else
        echo "${PROJECT}-${VERSION}-${triple}"
    fi
}

# Resolve host working directory for docker -v bind mount.
# On MSYS2 / Git Bash, `pwd -W` returns a Windows path (C:/Users/...)
# which Docker Desktop expects.  On Linux / macOS, plain `pwd` suffices.
host_pwd() {
    pwd -W 2>/dev/null || pwd
}

build_linux() {
    local short="$1"
    local musl_target="${TARGET_MAP[$short]}"
    local triple="${TRIPLE_MAP[$short]}"
    local image_tag="openclaw-build:${short}"
    local out="${DIST_DIR}/$(release_name "${short}")"

    log "Building ${triple} (MUSL_TARGET=${musl_target})"

    docker build \
        --build-arg "MUSL_TARGET=${musl_target}" \
        -t "${image_tag}" \
        .

    mkdir -p "${DIST_DIR}"

    # Extract binary from build image directly to release name
    local container_id
    container_id=$(docker create "${image_tag}")
    docker cp "${container_id}:/app/openclaw-light" "${out}"
    docker rm "${container_id}" > /dev/null

    local size
    size=$(stat -c%s "${out}" 2>/dev/null || stat -f%z "${out}")
    local size_mb
    size_mb=$(awk "BEGIN { printf \"%.1f\", ${size}/1048576 }")

    ok "Built ${out} (${size_mb} MB)"
}

build_windows() {
    local triple="${TRIPLE_MAP[windows]}"
    local out="${DIST_DIR}/$(release_name windows)"

    log "Building ${triple} (Windows cross-compile via mingw)"

    mkdir -p "${DIST_DIR}"

    # MSYS_NO_PATHCONV=1 prevents MSYS2 from mangling /app-style paths.
    MSYS_NO_PATHCONV=1 docker run --rm \
        -v "$(host_pwd):/app" \
        -w /app \
        -e RUSTUP_TOOLCHAIN=1.85.1 \
        rust:1.85 bash -c '
            apt-get update -qq && apt-get install -y -qq gcc-mingw-w64-x86-64 >/dev/null 2>&1 &&
            rustup target add x86_64-pc-windows-gnu &&
            export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc &&
            cargo build --release --target x86_64-pc-windows-gnu &&
            cp target/x86_64-pc-windows-gnu/release/openclaw-light.exe \
               dist/openclaw-light.exe.tmp
        '

    # Rename temp file to versioned release name
    mv "${DIST_DIR}/openclaw-light.exe.tmp" "${out}"

    local size
    size=$(stat -c%s "${out}" 2>/dev/null || stat -f%z "${out}")
    local size_mb
    size_mb=$(awk "BEGIN { printf \"%.1f\", ${size}/1048576 }")

    ok "Built ${out} (${size_mb} MB)"
}

build_target() {
    local short="$1"
    if [[ "${short}" == "windows" ]]; then
        build_windows
    else
        build_linux "${short}"
    fi
}

sha_of() {
    sha256sum "$1" | awk '{print $1}'
}

verify_target() {
    local short="$1"
    local out="${DIST_DIR}/$(release_name "${short}")"

    log "Verify: building $(release_name "${short}") — pass 1"
    build_target "${short}"
    local sha1
    sha1=$(sha_of "${out}")
    cp "${out}" "${out}.pass1"

    log "Verify: building $(release_name "${short}") — pass 2"
    build_target "${short}"
    local sha2
    sha2=$(sha_of "${out}")

    printf '  Build 1: sha256=%s\n' "${sha1}"
    printf '  Build 2: sha256=%s\n' "${sha2}"

    rm -f "${out}.pass1"

    if [[ "${sha1}" == "${sha2}" ]]; then
        ok "PASS: $(release_name "${short}") builds are bit-for-bit identical"
    else
        fail "FAIL: $(release_name "${short}") builds differ!"
    fi
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
targets=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --verify) VERIFY=true; shift ;;
        aarch64|armv7|x86_64|windows) targets+=("$1"); shift ;;
        *) fail "Unknown argument: $1" ;;
    esac
done

# Default to all targets if none specified
if [[ ${#targets[@]} -eq 0 ]]; then
    targets=("${ALL_TARGETS[@]}")
fi

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
log "Targets: ${targets[*]}"

for t in "${targets[@]}"; do
    if [[ "${VERIFY}" == true ]]; then
        verify_target "${t}"
    else
        build_target "${t}"
    fi
done

# ---------------------------------------------------------------------------
# Summary: list release artifacts
# ---------------------------------------------------------------------------
log "Release artifacts (${VERSION}):"
for t in "${targets[@]}"; do
    name="$(release_name "$t")"
    if [[ -f "${DIST_DIR}/${name}" ]]; then
        size=$(stat -c%s "${DIST_DIR}/${name}" 2>/dev/null \
            || stat -f%z "${DIST_DIR}/${name}")
        size_mb=$(awk "BEGIN { printf \"%.1f\", ${size}/1048576 }")
        printf '  %s  (%s MB)\n' "${name}" "${size_mb}"
    fi
done

ok "Done."
