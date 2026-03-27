#!/bin/bash
# Deploy OpenClaw Light to a remote device
#
# Usage:
#   ./scripts/deploy.sh                          # 默认 aarch64 -> pi@raspberrypi.local
#   DEPLOY_HOST=user@host ./scripts/deploy.sh    # 自定义主机
#   ./scripts/deploy.sh armv7                    # 指定目标架构
set -euo pipefail

# ---------------------------------------------------------------------------
# Target mapping (same as docker-build.sh)
# ---------------------------------------------------------------------------
declare -A TRIPLE_MAP=(
    [aarch64]="aarch64-unknown-linux-musl"
    [armv7]="armv7-unknown-linux-musleabihf"
    [x86_64]="x86_64-unknown-linux-musl"
)

ARCH="${1:-aarch64}"
TRIPLE="${TRIPLE_MAP[$ARCH]:-}"
if [[ -z "$TRIPLE" ]]; then
    echo "Unknown architecture: $ARCH"
    echo "Supported: aarch64, armv7, x86_64"
    exit 1
fi

REMOTE_HOST="${DEPLOY_HOST:-pi@raspberrypi.local}"
REMOTE_DIR="${DEPLOY_DIR:-/opt/openclaw}"
BINARY="dist/${TRIPLE}/openclaw-light"

# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------
if [[ ! -f "$BINARY" ]]; then
    echo "Binary not found: $BINARY"
    echo "Run './scripts/docker-build.sh ${ARCH}' first."
    exit 1
fi

SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY")
SIZE_MB=$(awk "BEGIN { printf \"%.1f\", ${SIZE}/1048576 }")
echo "Deploying ${BINARY} (${SIZE_MB} MB) to ${REMOTE_HOST}:${REMOTE_DIR}"

# ---------------------------------------------------------------------------
# Deploy
# ---------------------------------------------------------------------------
# Create remote directory structure
ssh "$REMOTE_HOST" "sudo mkdir -p ${REMOTE_DIR}/sessions && sudo chown -R \$(whoami) ${REMOTE_DIR}"

# Copy binary
scp "$BINARY" "${REMOTE_HOST}:${REMOTE_DIR}/openclaw-light"
ssh "$REMOTE_HOST" "chmod +x ${REMOTE_DIR}/openclaw-light"

# Copy config example (always) + systemd service
scp config/openclaw.json.example "${REMOTE_HOST}:${REMOTE_DIR}/openclaw.json.example"
scp deploy/openclaw-light.service "${REMOTE_HOST}:/tmp/openclaw-light.service"

# Install systemd service if not yet installed, or update it
ssh "$REMOTE_HOST" "sudo mv /tmp/openclaw-light.service /etc/systemd/system/ && sudo systemctl daemon-reload"

# ---------------------------------------------------------------------------
# Post-deploy: auto-restart or first-time guide
# ---------------------------------------------------------------------------
read -r HAS_CONFIG HAS_ENV IS_ACTIVE < <(ssh "$REMOTE_HOST" "
    echo \$(test -f ${REMOTE_DIR}/openclaw.json && echo yes || echo no) \
         \$(test -f ${REMOTE_DIR}/.env && echo yes || echo no) \
         \$(systemctl is-active openclaw-light 2>/dev/null || echo inactive)
")

if [[ "$HAS_CONFIG" == "yes" && "$HAS_ENV" == "yes" && "$IS_ACTIVE" == "active" ]]; then
    echo "Restarting openclaw-light..."
    ssh "$REMOTE_HOST" "sudo systemctl restart openclaw-light"
    sleep 2
    echo ""
    ssh "$REMOTE_HOST" "sudo journalctl -u openclaw-light -n 20 --no-pager"
    echo ""
    echo "Deploy + restart complete."
else
    echo ""
    echo "Deploy complete! First-time setup on ${REMOTE_HOST}:"
    echo ""
    [[ "$HAS_CONFIG" != "yes" ]] && \
    echo "  1. Create config:" && \
    echo "       cp ${REMOTE_DIR}/openclaw.json.example ${REMOTE_DIR}/openclaw.json" && \
    echo "       nano ${REMOTE_DIR}/openclaw.json" && \
    echo ""
    [[ "$HAS_ENV" != "yes" ]] && \
    echo "  2. Create .env with API keys:" && \
    echo "       cat > ${REMOTE_DIR}/.env << 'ENVEOF'" && \
    echo "       TELEGRAM_BOT_TOKEN=..." && \
    echo "       ANTHROPIC_API_KEY=..." && \
    echo "       GROQ_API_KEY=..." && \
    echo "       HA_TOKEN=..." && \
    echo "       ENVEOF" && \
    echo "       chmod 600 ${REMOTE_DIR}/.env" && \
    echo ""
    echo "  3. Create service user (if not exists):"
    echo "       sudo useradd -r -s /usr/sbin/nologin -g openclaw openclaw"
    echo "       sudo chown -R openclaw:openclaw ${REMOTE_DIR}"
    echo ""
    echo "  4. Start service:"
    echo "       sudo systemctl enable --now openclaw-light"
    echo "       sudo journalctl -u openclaw-light -f"
fi
