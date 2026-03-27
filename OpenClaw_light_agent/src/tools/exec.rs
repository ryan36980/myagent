//! Shell command execution tool.
//!
//! Spawns a child process via `sh -c` with configurable timeout, output
//! truncation, and an optional skills directory prepended to `PATH`.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::Tool;
use crate::config::ExecConfig;
use crate::error::{GatewayError, Result};

/// Maximum allowed timeout (seconds).
const MAX_TIMEOUT_SECS: u64 = 300;

pub struct ExecTool {
    timeout_secs: u64,
    max_output_bytes: usize,
    work_dir: PathBuf,
    skills_dir: PathBuf,
    /// Pre-computed PATH value with skills_dir prepended.
    path_env: String,
    /// Description string including skills_dir for the LLM.
    tool_description: String,
}

impl ExecTool {
    pub fn new(cfg: &ExecConfig) -> Self {
        let work_dir = PathBuf::from(&cfg.work_dir);
        let skills_dir = PathBuf::from(&cfg.skills_dir);

        // Prepend skills_dir to PATH
        let sys_path = std::env::var("PATH").unwrap_or_default();
        let path_env = format!("{}:{}", skills_dir.display(), sys_path);

        let tool_description = format!(
            "Execute shell commands to solve problems. Use this to:\n\
             - Run scripts (bash, python3, etc.) and one-off commands\n\
             - Process data, parse files, do calculations\n\
             - Check system status, network, disk, processes\n\
             - Make HTTP requests with curl\n\
             - Create reusable scripts in the skills directory ({})\n\n\
             Skill scripts saved to {} are always in PATH — just run the name directly.\n\
             Use `ls {}` to see your saved skills.\n\n\
             Returns: exit code, stdout, and stderr.",
            skills_dir.display(),
            skills_dir.display(),
            skills_dir.display(),
        );

        Self {
            timeout_secs: cfg.timeout_secs.min(MAX_TIMEOUT_SECS),
            max_output_bytes: cfg.max_output_bytes,
            work_dir,
            skills_dir,
            path_env,
            tool_description,
        }
    }
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30, max: 300)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "exec".into(),
                message: "command is required".into(),
            })?;

        let timeout_secs = input["timeout"]
            .as_u64()
            .unwrap_or(self.timeout_secs)
            .min(MAX_TIMEOUT_SECS);

        debug!(command, timeout_secs, "executing command");

        // Ensure skills dir exists
        let _ = tokio::fs::create_dir_all(&self.skills_dir).await;

        let mut child = tokio::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .env("PATH", &self.path_env)
            .env("HOME", "/app")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .spawn()
            .map_err(|e| GatewayError::Tool {
                tool: "exec".into(),
                message: format!("failed to spawn process: {e}"),
            })?;

        // Read stdout/stderr via take() so we don't consume `child`.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let timeout_dur = std::time::Duration::from_secs(timeout_secs);
        let max_bytes = self.max_output_bytes;

        let fut = async {
            let stdout_bytes = match stdout_pipe {
                Some(mut r) => {
                    let mut buf = Vec::new();
                    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut buf).await?;
                    buf
                }
                None => Vec::new(),
            };
            let stderr_bytes = match stderr_pipe {
                Some(mut r) => {
                    let mut buf = Vec::new();
                    tokio::io::AsyncReadExt::read_to_end(&mut r, &mut buf).await?;
                    buf
                }
                None => Vec::new(),
            };
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, stdout_bytes, stderr_bytes))
        };

        match tokio::time::timeout(timeout_dur, fut).await {
            Ok(Ok((status, stdout_bytes, stderr_bytes))) => {
                let half = max_bytes / 2;
                let stdout = truncate_output(&stdout_bytes, half);
                let stderr = truncate_output(&stderr_bytes, half);
                let code = status.code().unwrap_or(-1);

                let mut result = format!("Exit code: {code}");
                if !stdout.is_empty() {
                    result.push_str(&format!("\n--- stdout ---\n{stdout}"));
                }
                if !stderr.is_empty() {
                    result.push_str(&format!("\n--- stderr ---\n{stderr}"));
                }

                Ok(result)
            }
            Ok(Err(e)) => Err(GatewayError::Tool {
                tool: "exec".into(),
                message: format!("process error: {e}"),
            }),
            Err(_) => {
                // Timeout — kill the child process
                let _ = child.kill().await;
                Ok(format!(
                    "Error: Command timed out after {timeout_secs} seconds (killed)"
                ))
            }
        }
    }
}

/// Truncate a byte slice to at most `max_bytes`, converting to a lossy UTF-8
/// string.  Appends a truncation notice if the output was cut.
fn truncate_output(bytes: &[u8], max_bytes: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.len() <= max_bytes {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let truncated = String::from_utf8_lossy(&bytes[..max_bytes]);
        format!(
            "{}... [truncated, {} total bytes]",
            truncated,
            bytes.len()
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_config_defaults() {
        let cfg = ExecConfig::default();
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_output_bytes, 8192);
        assert_eq!(cfg.work_dir, ".");
        assert_eq!(cfg.skills_dir, "./skills");
    }

    #[test]
    fn truncate_output_empty() {
        assert_eq!(truncate_output(b"", 100), "");
    }

    #[test]
    fn truncate_output_within_limit() {
        assert_eq!(truncate_output(b"hello", 100), "hello");
    }

    #[test]
    fn truncate_output_exceeds_limit() {
        let long = b"hello world this is a long string";
        let result = truncate_output(long, 5);
        assert!(result.starts_with("hello"));
        assert!(result.contains("truncated"));
        assert!(result.contains(&format!("{}", long.len())));
    }

    #[test]
    fn timeout_capped_at_max() {
        let cfg = ExecConfig {
            timeout_secs: 999,
            ..ExecConfig::default()
        };
        let tool = ExecTool::new(&cfg);
        assert_eq!(tool.timeout_secs, MAX_TIMEOUT_SECS);
    }

    #[tokio::test]
    async fn exec_echo_hello() {
        let cfg = ExecConfig::default();
        let tool = ExecTool::new(&cfg);
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.contains("Exit code: 0"));
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn exec_exit_code() {
        let cfg = ExecConfig::default();
        let tool = ExecTool::new(&cfg);
        let result = tool
            .execute(json!({"command": "exit 42"}))
            .await
            .unwrap();
        assert!(result.contains("Exit code: 42"));
    }

    #[tokio::test]
    async fn exec_stderr() {
        let cfg = ExecConfig::default();
        let tool = ExecTool::new(&cfg);
        let result = tool
            .execute(json!({"command": "echo err >&2"}))
            .await
            .unwrap();
        assert!(result.contains("stderr"));
        assert!(result.contains("err"));
    }

    #[tokio::test]
    async fn exec_timeout() {
        let cfg = ExecConfig {
            timeout_secs: 1,
            ..ExecConfig::default()
        };
        let tool = ExecTool::new(&cfg);
        let result = tool
            .execute(json!({"command": "sleep 60", "timeout": 1}))
            .await
            .unwrap();
        assert!(result.contains("timed out"));
    }
}
