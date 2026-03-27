# OpenClaw Light — Reproducible multi-target build
#
# Usage:
#   docker build --build-arg MUSL_TARGET=x86_64-musl -t openclaw:x86_64 .
#   docker build --build-arg MUSL_TARGET=aarch64-musl -t openclaw:aarch64 .
#   docker build --build-arg MUSL_TARGET=armv7-musleabihf -t openclaw:armv7 .

# ---------------------------------------------------------------------------
# Build argument: selects the rust-musl-cross image tag and Rust target triple
# ---------------------------------------------------------------------------
ARG MUSL_TARGET=x86_64-musl

# ---------------------------------------------------------------------------
# Stage 1: Builder
# ---------------------------------------------------------------------------
FROM ghcr.io/rust-cross/rust-musl-cross:${MUSL_TARGET} AS builder

# Reproducibility environment
ENV CARGO_INCREMENTAL=0 \
    SOURCE_DATE_EPOCH=0

# Derive the Rust target triple from MUSL_TARGET
# x86_64-musl         -> x86_64-unknown-linux-musl
# aarch64-musl        -> aarch64-unknown-linux-musl
# armv7-musleabihf    -> armv7-unknown-linux-musleabihf
ARG MUSL_TARGET
ENV RUST_TARGET=${MUSL_TARGET:+placeholder}

# Map MUSL_TARGET to RUST_TARGET and generate .cargo/config.toml with correct linker
# The rust-musl-cross image names GCC as {full-triple}-gcc (e.g. x86_64-unknown-linux-musl-gcc)
RUN case "${MUSL_TARGET}" in \
      x86_64-musl)        TRIPLE="x86_64-unknown-linux-musl"      ;; \
      aarch64-musl)       TRIPLE="aarch64-unknown-linux-musl"     ;; \
      armv7-musleabihf)   TRIPLE="armv7-unknown-linux-musleabihf" ;; \
      *) echo "Unsupported MUSL_TARGET: ${MUSL_TARGET}" >&2; exit 1 ;; \
    esac && \
    echo "$TRIPLE" > /tmp/rust_target && \
    echo "$TRIPLE" | sed 's/-/_/g' > /tmp/rust_target_underscored

WORKDIR /app

# Remap build paths for reproducibility
ENV RUSTFLAGS="--remap-path-prefix=/app=. --remap-path-prefix=/root=~"

# --- Dependency caching layer ---
# Copy only manifests + lockfile first so dependency builds are cached
COPY Cargo.toml Cargo.lock ./

# Generate .cargo/config.toml with correct linker for this image
# (host .cargo/config.toml uses short names for `cross` tool; image needs full-triple names)
RUN mkdir -p .cargo && \
    TRIPLE=$(cat /tmp/rust_target) && \
    printf '[target.%s]\nlinker = "%s-gcc"\n' "$TRIPLE" "$TRIPLE" > .cargo/config.toml

# Create dummy src/lib.rs + src/main.rs to build dependencies
RUN mkdir src && \
    echo '' > src/lib.rs && \
    echo 'fn main() { println!("dummy"); }' > src/main.rs && \
    cargo build --release --locked \
      --target "$(cat /tmp/rust_target)" && \
    rm -rf src

# --- Full source build ---
COPY src/ src/

# Touch lib.rs and main.rs so cargo sees them as newer than the dummy
RUN touch src/lib.rs src/main.rs && \
    cargo build --release --locked \
      --target "$(cat /tmp/rust_target)" && \
    cp "target/$(cat /tmp/rust_target)/release/openclaw-light" /openclaw-light

# ---------------------------------------------------------------------------
# Stage 2: Test runner (optional — `docker build --target tester`)
#
# Build only — tests are run at `docker run` time so that API keys from
# `--env-file .env` are available.  No `#[ignore]` allowed; every test
# must pass with the real `.env`.
#
# Usage:
#   docker build --target tester -t test .
#   docker run --rm --env-file .env test
# ---------------------------------------------------------------------------
FROM builder AS tester
COPY tests/ tests/
# Compile tests (but don't run them yet — no API keys at build time)
RUN cargo test --release --locked --no-run \
      --target "$(cat /tmp/rust_target)"
# Default CMD: run all tests at container start (API keys from --env-file)
CMD cargo test --release --locked \
      --target "$(cat /tmp/rust_target)" \
      -- --test-threads=1

# ---------------------------------------------------------------------------
# Stage 3: Minimal runtime image (busybox shell + non-root)
# ---------------------------------------------------------------------------
FROM scratch

# Busybox: provides sh, ls, cat, grep, awk, sed, wget, tar, find, vi, etc.
COPY --from=busybox:musl /bin/busybox /bin/busybox

# Install busybox symlinks (sh, ls, cat, grep, etc.)
RUN ["/bin/busybox", "--install", "-s", "/bin"]

# Create non-root user (UID 1000)
RUN echo "openclaw:x:1000:1000::/app:/bin/sh" >> /etc/passwd && \
    echo "openclaw:x:1000:" >> /etc/group

# Create necessary directories
RUN mkdir -p /app /tmp /app/sessions /app/memory /app/skills

COPY --from=builder /openclaw-light /app/openclaw-light
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY entrypoint.sh /app/entrypoint.sh

# Own /app and /tmp by openclaw user; entrypoint starts as root for chown then drops
RUN chown -R 1000:1000 /app /tmp && chmod +x /app/entrypoint.sh

WORKDIR /app

ENTRYPOINT ["/app/entrypoint.sh"]
