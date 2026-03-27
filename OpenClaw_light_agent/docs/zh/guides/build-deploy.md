> [配置指南](configuration.md) > 构建与部署

# 构建与部署

## 0. 下载预编译二进制

最快的方式 —— 直接从 [Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) 页下载对应平台的二进制文件，无需安装 Docker 或 Rust。

| 文件 | 平台 | 说明 |
|------|------|------|
| `openclaw-light-<version>-x86_64-unknown-linux-musl` | Linux x86_64 | 服务器、PC |
| `openclaw-light-<version>-aarch64-unknown-linux-musl` | Linux ARM64 | 树莓派 4/5、ARM 服务器 |
| `openclaw-light-<version>-armv7-unknown-linux-musleabihf` | Linux ARMv7 | 树莓派 2/3、旧版 ARM |
| `openclaw-light-<version>-x86_64-pc-windows-gnu.exe` | Windows x86_64 | 无需安装运行时 |

所有 Linux 二进制均为 musl 静态链接，无外部依赖。下载后：

```bash
# Linux
chmod +x openclaw-light-*
mkdir -p sessions memory skills
./openclaw-light-* --config openclaw.json

# Windows — 直接双击或命令行运行
openclaw-light-*.exe
```

配置文件编写参考 [配置指南](configuration.md)。如需从源码构建，见下文。

---

## 1. Docker 部署（推荐）

默认方式，适用于 Linux 服务器。

```bash
# 构建镜像
docker build -t openclaw-light .

# 创建数据目录
mkdir -p sessions memory skills

# 启动
docker compose up -d

# 查看日志
docker compose logs -f
```

详见 [完整示例与部署](config-examples.md) 中的部署文件关系和快速步骤。

---

## 2. Windows 原生运行（exe）

不需要 Docker，单个 exe 即可运行。

### 2.1 交叉编译 Windows exe

项目默认编译 Linux 二进制。通过统一构建脚本交叉编译生成 Windows exe：

```bash
./scripts/docker-build.sh windows
```

编译产物位于：
```
dist/x86_64-pc-windows-gnu/openclaw-light.exe    # ~4MB
```

> 脚本自动处理 MSYS2 路径转换，Windows / Linux / macOS 均可直接运行。

<details>
<summary>原理：手动 docker run 命令</summary>

```bash
# 需要 Rust ≥1.85（edition2024 支持）
# 项目 rust-toolchain.toml 锁定了 1.84，需用 RUSTUP_TOOLCHAIN 覆盖

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

> **注意：** 上述命令在 Windows Git Bash / MSYS2 下运行。Linux / macOS 用户将 `$(pwd -W)` 替换为 `$(pwd)`，并去掉 `MSYS_NO_PATHCONV=1`。

</details>

### 2.2 部署目录结构

将以下文件复制到目标 Windows 机器：

```
openclaw-light/
├── openclaw-light.exe    ← 编译产物，无需安装运行时
├── openclaw.json            ← 配置文件
├── sessions/                ← 空目录，手动创建
├── memory/                  ← 空目录，手动创建
└── skills/                  ← 空目录，手动创建
```

### 2.3 配置与启动

**openclaw.json 示例**（使用本地 LLM）：
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

**CMD 启动：**
```cmd
set VLLM_API_KEY=xxx_optical
set RUST_LOG=info
openclaw-light.exe
```

**PowerShell 启动：**
```powershell
$env:VLLM_API_KEY = "xxx_optical"
$env:RUST_LOG = "info"
.\openclaw-light.exe
```

### 2.4 注意事项

- **无外部依赖**：exe 静态链接，不需要 Visual C++ 运行时或 DLL
- **TLS 证书**：HTTPS 请求（如云端 LLM API）使用内置的 webpki 根证书，无需系统证书
- **防火墙**：如启用 HTTP API 通道，需放行对应端口（默认 8080）
- **数据目录**：`sessions/`、`memory/`、`skills/` 必须存在，否则运行时写入会报错

---

## 3. Linux 原生运行（静态链接）

项目默认通过 Docker 多阶段构建生成 musl 静态链接的 Linux 二进制：

```bash
# 编译（在 Docker 内完成）
docker build -t openclaw-light .

# 从镜像中提取二进制到 dist/
mkdir -p dist
docker create --name tmp openclaw-light
docker cp tmp:/app/openclaw-light dist/openclaw-light
docker rm tmp

# 直接运行（无需 Docker）
chmod +x dist/openclaw-light
VLLM_API_KEY=xxx_optical dist/openclaw-light
```

支持的 Linux 目标架构（通过 `MUSL_TARGET` 构建参数）：

| 构建参数 | 目标 | 适用场景 |
|---------|------|---------|
| `x86_64-musl`（默认） | x86_64 | 服务器、PC |
| `aarch64-musl` | ARM64 | 树莓派 4/5、ARM 服务器 |
| `armv7-musleabihf` | ARMv7 | 树莓派 3、旧版 ARM 设备 |

```bash
# 示例：编译 ARM64 版本
docker build --build-arg MUSL_TARGET=aarch64-musl -t openclaw-light:arm64 .
```

---

## 4. systemd 服务部署

适用于 Linux 裸机 / 树莓派部署。参考 `deploy/openclaw-light.service`：

```bash
sudo cp deploy/openclaw-light.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now openclaw-light
```

注意 `ProtectSystem=strict` 要求正确设置 `ReadWritePaths`，详见 [故障排除 §8](troubleshooting.md#8-arm--systemd-裸机部署)。
