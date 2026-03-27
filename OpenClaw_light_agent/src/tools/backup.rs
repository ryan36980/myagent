//! Backup management tool for the agent.
//!
//! Allows the agent to query status, enable/disable automatic backups,
//! and trigger an immediate backup run.  The runtime enabled/disabled
//! state is persisted in `backups/state.json` so it survives restarts.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::Tool;
use crate::backup;
use crate::config::BackupConfig;
use crate::error::{GatewayError, Result};

/// Persisted runtime state for backup enable/disable toggle.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupState {
    enabled: bool,
}

pub struct BackupTool {
    config: BackupConfig,
    state_file: PathBuf,
}

impl BackupTool {
    pub fn new(config: &BackupConfig) -> Self {
        let state_file = Path::new(&config.dir).join("state.json");
        Self {
            config: config.clone(),
            state_file,
        }
    }

    /// Read the persisted state, falling back to the config default.
    async fn load_state(&self) -> bool {
        match tokio::fs::read_to_string(&self.state_file).await {
            Ok(content) => serde_json::from_str::<BackupState>(&content)
                .map(|s| s.enabled)
                .unwrap_or(self.config.enabled),
            Err(_) => self.config.enabled,
        }
    }

    /// Persist the enabled/disabled state.
    async fn save_state(&self, enabled: bool) -> Result<()> {
        // Ensure directory exists
        if let Some(parent) = self.state_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let state = BackupState { enabled };
        let content = serde_json::to_string(&state)?;
        tokio::fs::write(&self.state_file, content).await?;
        Ok(())
    }

    /// Build a runtime config with the persisted enabled state.
    async fn effective_config(&self) -> BackupConfig {
        let enabled = self.load_state().await;
        BackupConfig {
            enabled,
            ..self.config.clone()
        }
    }
}

#[async_trait]
impl Tool for BackupTool {
    fn name(&self) -> &str {
        "backup"
    }

    fn description(&self) -> &str {
        "Manage automatic backups. Actions: status (show current state), \
         enable (turn on auto-backup), disable (turn off auto-backup), \
         run (create backup immediately)."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "enable", "disable", "run"],
                    "description": "Action to perform"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let action = input["action"]
            .as_str()
            .unwrap_or("status");

        match action {
            "status" => {
                let enabled = self.load_state().await;
                let dir = Path::new(&self.config.dir);
                let (last_ts, total_bytes) = backup::status(dir).await;

                let last_str = match last_ts {
                    Some(ts) => format!("{ts} (Unix timestamp)"),
                    None => "never".into(),
                };

                Ok(format!(
                    "Backup status:\n\
                     - enabled: {enabled}\n\
                     - directory: {}\n\
                     - interval: {}h\n\
                     - retention: {}d\n\
                     - max size: {} MB\n\
                     - last backup: {last_str}\n\
                     - total size: {} bytes",
                    self.config.dir,
                    self.config.interval_hours,
                    self.config.retention_days,
                    self.config.max_size_mb,
                    total_bytes,
                ))
            }
            "enable" => {
                self.save_state(true).await?;
                Ok("Automatic backup enabled.".into())
            }
            "disable" => {
                self.save_state(false).await?;
                Ok("Automatic backup disabled.".into())
            }
            "run" => {
                let mut cfg = self.effective_config().await;
                // Force enabled + zero interval to ensure immediate run
                cfg.enabled = true;
                cfg.interval_hours = 0;

                match backup::maybe_run(&cfg).await? {
                    Some(msg) => Ok(msg),
                    None => Ok("Backup completed (no files to archive).".into()),
                }
            }
            _ => Err(GatewayError::Tool {
                tool: "backup".into(),
                message: format!("unknown action: {action}. Use status/enable/disable/run."),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_metadata() {
        let tool = BackupTool::new(&BackupConfig::default());
        assert_eq!(tool.name(), "backup");
        assert!(!tool.description().is_empty());
        assert!(tool.input_schema().is_object());
    }

    #[tokio::test]
    async fn status_action() {
        let dir = tempfile::tempdir().unwrap();
        let config = BackupConfig {
            dir: dir.path().to_string_lossy().into(),
            ..BackupConfig::default()
        };
        let tool = BackupTool::new(&config);
        let result = tool.execute(json!({"action": "status"})).await.unwrap();
        assert!(result.contains("enabled: true"));
        assert!(result.contains("last backup: never"));
    }

    #[tokio::test]
    async fn enable_disable_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let config = BackupConfig {
            dir: dir.path().to_string_lossy().into(),
            ..BackupConfig::default()
        };
        let tool = BackupTool::new(&config);

        // Disable
        tool.execute(json!({"action": "disable"})).await.unwrap();
        assert!(!tool.load_state().await);

        // Enable
        tool.execute(json!({"action": "enable"})).await.unwrap();
        assert!(tool.load_state().await);
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let config = BackupConfig::default();
        let tool = BackupTool::new(&config);
        let result = tool.execute(json!({"action": "invalid"})).await;
        assert!(result.is_err());
    }
}
