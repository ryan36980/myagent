#!/bin/sh
# Fix ownership of mounted data volumes.
# On Docker Desktop for Windows, files created inside mounted volumes
# may appear as root-owned, causing Permission denied for user 1000.
chown -R 1000:1000 /app/memory /app/sessions /app/skills /app/backups 2>/dev/null || true
chown 1000:1000 /app/auth_tokens.json 2>/dev/null || true

# Drop to non-root user and exec the gateway
exec su -s /bin/sh openclaw -c /app/openclaw-light
