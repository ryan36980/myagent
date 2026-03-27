> [Configuration Guide](configuration.md) > Build & Deployment

# Build & Deployment

## 0. Download Pre-built Binaries

The fastest way — download the binary for your platform from the [Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) page. No Docker or Rust installation required.

| File | Platform | Use Case |
|------|----------|----------|
| `openclaw-light-<version>-x86_64-unknown-linux-musl` | Linux x86_64 | Servers, PCs |
| `openclaw-light-<version>-aarch64-unknown-linux-musl` | Linux ARM64 | Raspberry Pi 4/5, ARM servers |
| `openclaw-light-<version>-armv7-unknown-linux-musleabihf` | Linux ARMv7 | Raspberry Pi 2/3, older ARM |
| `openclaw-light-<version>-x86_64-pc-windows-gnu.exe` | Windows x86_64 | No runtime installation needed |

All Linux binaries are musl statically linked with no external dependencies. After downloading:

```bash
# Linux
chmod +x openclaw-light-*
mkdir -p sessions memory skills
./openclaw-light-* --config openclaw.json

# Windows — run directly
openclaw-light-*.exe
```

See the [Configuration Guide](configuration.md) for config file setup. To build from source instead, see below.

---

## 1. Docker Deployment (Recommended)

The default method, suitable for Linux servers.

```bash
# Build the image
docker build -t openclaw-light .

# Create data directories
mkdir -p sessions memory skills

# Start
docker compose up -d

# View logs
docker compose logs -f
```

See [Complete Examples & Deployment](config-examples.md) for deployment file relationships and quick steps.

---

## 2. Native Windows (exe)

No Docker required — a single exe is all you need.

### 2.1 Cross-Compiling the Windows exe

The project compiles Linux binaries by default. Use the unified build script to cross-compile a Windows exe:

```bash
./scripts/docker-build.sh windows
```

Build output:
```
dist/x86_64-pc-windows-gnu/openclaw-light.exe    # ~4MB
```

> The script handles MSYS2 path conversion automatically — works on Windows, Linux, and macOS.

<details>
<summary>Under the hood: manual docker run command</summary>

```bash
# Requires Rust >= 1.85 (edition2024 support)
# The project rust-toolchain.toml pins 1.84, so override with RUSTUP_TOOLCHAIN

MSYS_NO_PATHCONV=1 docker run --rm \
  -v "$(pwd -W):/app" \
  -w /app \
  -e RUSTUP_TOOLCHAIN=1.85.1 \
  rust:1.85 bash -c '
    apt-get update -qq && apt-get install -y -qq gcc-mingw-w64-x86-64 >/dev/null 2>&1 &&
    rustup target add x86_64-pc-windows-gnu &&
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc &&
    cargo build --release --target x86_64-pc-windows-gnu &&
    mkdir -p dist/x86_64-pc-windows-gnu &&
    cp target/x86_64-pc-windows-gnu/release/openclaw-light.exe \
       dist/x86_64-pc-windows-gnu/openclaw-light.exe
  '
```

> **Note:** The above command is for Windows Git Bash / MSYS2. On Linux / macOS, replace `$(pwd -W)` with `$(pwd)` and remove `MSYS_NO_PATHCONV=1`.

</details>

### 2.2 Deployment Directory Structure

Copy the following files to the target Windows machine:

```
openclaw-light/
├── openclaw-light.exe    ← build artifact, no runtime installation needed
├── openclaw.json            ← configuration file
├── sessions/                ← empty directory, create manually
├── memory/                  ← empty directory, create manually
└── skills/                  ← empty directory, create manually
```

### 2.3 Configuration & Startup

**openclaw.json example** (using a local LLM):
```json5
{
  "provider": "vllm",
  "model": "Qwen/Qwen2.5-72B-Instruct",
  "providerConfig": {
    "baseUrl": "http://10.123.104.7:8000/v1",
    "apiKeyEnv": "VLLM_API_KEY"
  },
  "channels": {
    "httpApi": {
      "enabled": true,
      "listen": "0.0.0.0:8080"
    }
  },
  "tools": {
    "allow": ["get_time", "memory", "web_fetch", "web_search"]
  }
}
```

**CMD startup:**
```cmd
set VLLM_API_KEY=xxx_optical
set RUST_LOG=info
openclaw-light.exe
```

**PowerShell startup:**
```powershell
$env:VLLM_API_KEY = "xxx_optical"
$env:RUST_LOG = "info"
.\openclaw-light.exe
```

### 2.4 Notes

- **No external dependencies**: the exe is statically linked — no Visual C++ runtime or DLLs needed
- **TLS certificates**: HTTPS requests (e.g. cloud LLM APIs) use built-in webpki root certificates; no system certificates required
- **Firewall**: if the HTTP API channel is enabled, allow the corresponding port (default 8080)
- **Data directories**: `sessions/`, `memory/`, `skills/` must exist, otherwise writes will fail at runtime

---

## 3. Native Linux (Static Binary)

The project uses a Docker multi-stage build to produce a musl statically-linked Linux binary by default:

```bash
# Compile (inside Docker)
docker build -t openclaw-light .

# Extract the binary from the image to dist/
mkdir -p dist
docker create --name tmp openclaw-light
docker cp tmp:/app/openclaw-light dist/openclaw-light
docker rm tmp

# Run directly (no Docker needed)
chmod +x dist/openclaw-light
VLLM_API_KEY=xxx_optical dist/openclaw-light
```

Supported Linux target architectures (via the `MUSL_TARGET` build argument):

| Build Argument | Target | Use Case |
|---------------|--------|----------|
| `x86_64-musl` (default) | x86_64 | Servers, PCs |
| `aarch64-musl` | ARM64 | Raspberry Pi 4/5, ARM servers |
| `armv7-musleabihf` | ARMv7 | Raspberry Pi 3, older ARM devices |

```bash
# Example: compile for ARM64
docker build --build-arg MUSL_TARGET=aarch64-musl -t openclaw-light:arm64 .
```

---

## 4. systemd Service Deployment

For Linux bare-metal / Raspberry Pi deployment. Refer to `deploy/openclaw-light.service`:

```bash
sudo cp deploy/openclaw-light.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now openclaw-light
```

Note that `ProtectSystem=strict` requires correct `ReadWritePaths` — see [Troubleshooting §8](troubleshooting.md#8-arm--systemd-bare-metal-deployment) for details.
