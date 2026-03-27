//! Automatic backup: tar-based archiving with age-based cleanup.
//!
//! Called from the main cron tick (60s interval).  Each invocation is a
//! cheap metadata check (~μs) that only shells out to `tar` when the
//! configured `interval_hours` has elapsed since the last backup.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::warn;

use crate::config::BackupConfig;
use crate::error::Result;

/// Files and directories to include in the backup (if they exist).
const BACKUP_SOURCES: &[&str] = &[
    "openclaw-light",
    "openclaw.json",
    ".env",
    "auth_tokens.json",
    "sessions",
    "memory",
    "skills",
];

/// Read the runtime enabled state from `state.json` in the backup dir,
/// falling back to the config default if the file doesn't exist.
pub async fn runtime_enabled(config: &BackupConfig) -> bool {
    #[derive(serde::Deserialize)]
    struct State {
        enabled: bool,
    }
    let path = Path::new(&config.dir).join("state.json");
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => serde_json::from_str::<State>(&content)
            .map(|s| s.enabled)
            .unwrap_or(config.enabled),
        Err(_) => config.enabled,
    }
}

/// Check whether a backup is due and, if so, create one.
///
/// Respects the runtime toggle in `state.json` (set via the backup tool).
///
/// Returns `Ok(Some(msg))` when a backup was created, `Ok(None)` when
/// skipped (too recent or disabled), or `Err` on failure.
pub async fn maybe_run(config: &BackupConfig) -> Result<Option<String>> {
    if !runtime_enabled(config).await {
        return Ok(None);
    }

    let dir = Path::new(&config.dir);

    // Ensure backup directory exists
    tokio::fs::create_dir_all(dir).await?;

    // Find the most recent .tar.gz and check its age
    let interval = Duration::from_secs(config.interval_hours * 3600);
    if let Some(newest) = newest_backup_age(dir).await? {
        if newest < interval {
            return Ok(None);
        }
    }

    // Collect existing sources
    let mut sources: Vec<&str> = Vec::new();
    for src in BACKUP_SOURCES {
        if tokio::fs::metadata(src).await.is_ok() {
            sources.push(src);
        }
    }

    if sources.is_empty() {
        return Ok(Some("backup: no files to archive".into()));
    }

    // Build archive filename with Unix timestamp
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let archive = dir.join(format!("openclaw-{ts}.tar.gz"));

    // Shell out to tar
    let mut cmd = tokio::process::Command::new("tar");
    cmd.arg("czf").arg(&archive);
    for src in &sources {
        cmd.arg(src);
    }
    let output = cmd.output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::error::GatewayError::Agent(format!(
            "tar failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(200).collect::<String>()
        )));
    }

    // Cleanup old backups (by age, then by total size)
    let (age_deleted, size_deleted) =
        cleanup_old(dir, config.retention_days, config.max_size_mb).await;

    let size = tokio::fs::metadata(&archive)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let mut msg = format!(
        "backup created: {} ({} bytes, {} sources, {} old removed",
        archive.display(),
        size,
        sources.len(),
        age_deleted,
    );
    if size_deleted > 0 {
        msg.push_str(&format!(", {} over-size removed", size_deleted));
    }
    msg.push(')');

    Ok(Some(msg))
}

/// Return the age of the newest `.tar.gz` in `dir`, or `None` if empty.
async fn newest_backup_age(dir: &Path) -> Result<Option<Duration>> {
    let mut newest: Option<SystemTime> = None;

    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("openclaw-") || !name.ends_with(".tar.gz") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await {
            if let Ok(modified) = meta.modified() {
                newest = Some(match newest {
                    Some(prev) if modified > prev => modified,
                    Some(prev) => prev,
                    None => modified,
                });
            }
        }
    }

    match newest {
        Some(t) => Ok(Some(
            SystemTime::now()
                .duration_since(t)
                .unwrap_or(Duration::ZERO),
        )),
        None => Ok(None),
    }
}

/// Delete old `.tar.gz` files using a two-pass strategy:
/// 1. Remove backups older than `retention_days`.
/// 2. If total size still exceeds `max_size_mb`, remove oldest first.
///
/// Returns `(age_deleted, size_deleted)`.
async fn cleanup_old(dir: &Path, retention_days: u64, max_size_mb: u64) -> (usize, usize) {
    let max_age = Duration::from_secs(retention_days * 86400);
    let mut age_deleted = 0;

    // Collect all backup entries with metadata
    let mut backups: Vec<(std::path::PathBuf, SystemTime, u64)> = Vec::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("openclaw-") || !name.ends_with(".tar.gz") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await {
            if let Ok(modified) = meta.modified() {
                backups.push((entry.path(), modified, meta.len()));
            }
        }
    }

    // Pass 1: delete by age
    let now = SystemTime::now();
    let mut remaining: Vec<(std::path::PathBuf, SystemTime, u64)> = Vec::new();
    for (path, modified, size) in backups {
        let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
        if age > max_age {
            if let Err(e) = tokio::fs::remove_file(&path).await {
                warn!(path = %path.display(), error = %e, "failed to delete old backup");
                remaining.push((path, modified, size));
            } else {
                age_deleted += 1;
            }
        } else {
            remaining.push((path, modified, size));
        }
    }

    // Pass 2: delete by total size (oldest first)
    let max_bytes = max_size_mb * 1_048_576;
    let mut total_bytes: u64 = remaining.iter().map(|(_, _, s)| s).sum();

    if total_bytes <= max_bytes {
        return (age_deleted, 0);
    }

    // Sort oldest first (ascending modified time)
    remaining.sort_by_key(|(_, modified, _)| *modified);

    let mut size_deleted = 0;
    for (path, _, size) in &remaining {
        if total_bytes <= max_bytes {
            break;
        }
        if let Err(e) = tokio::fs::remove_file(path).await {
            warn!(path = %path.display(), error = %e, "failed to delete over-size backup");
        } else {
            total_bytes -= size;
            size_deleted += 1;
        }
    }

    (age_deleted, size_deleted)
}

/// Return the modification time of the newest backup as a Unix timestamp,
/// and the total size (bytes) of all backups in the directory.
pub async fn status(dir: &Path) -> (Option<i64>, u64) {
    let mut newest_ts: Option<i64> = None;
    let mut total_bytes: u64 = 0;

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return (None, 0),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("openclaw-") || !name.ends_with(".tar.gz") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await {
            total_bytes += meta.len();
            if let Ok(modified) = meta.modified() {
                let ts = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                newest_ts = Some(match newest_ts {
                    Some(prev) if ts > prev => ts,
                    Some(prev) => prev,
                    None => ts,
                });
            }
        }
    }

    (newest_ts, total_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_sources_list_not_empty() {
        assert!(!BACKUP_SOURCES.is_empty());
        assert!(BACKUP_SOURCES.contains(&"openclaw.json"));
        assert!(BACKUP_SOURCES.contains(&"sessions"));
    }

    #[tokio::test]
    async fn maybe_run_disabled() {
        let config = BackupConfig {
            enabled: false,
            ..BackupConfig::default()
        };
        let result = maybe_run(&config).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn newest_backup_age_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let age = newest_backup_age(dir.path()).await.unwrap();
        assert!(age.is_none());
    }

    #[tokio::test]
    async fn cleanup_old_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (age_deleted, size_deleted) = cleanup_old(dir.path(), 7, 200).await;
        assert_eq!(age_deleted, 0);
        assert_eq!(size_deleted, 0);
    }

    #[tokio::test]
    async fn status_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (ts, size) = status(dir.path()).await;
        assert!(ts.is_none());
        assert_eq!(size, 0);
    }
}
