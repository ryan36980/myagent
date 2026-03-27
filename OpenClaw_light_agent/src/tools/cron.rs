//! Cron-style scheduled task tool.
//!
//! Manages scheduled tasks stored in a JSON file.
//! Tasks are executed by the main loop checking pending schedules.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::Tool;
use crate::error::{GatewayError, Result};
use crate::tools::memory::ChatContext;

/// A scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    pub cron_expr: String,
    pub description: String,
    pub command: String,
    pub channel: String,
    pub chat_id: String,
    pub created_at: i64,
    #[serde(default)]
    pub last_run: Option<i64>,
    /// ISO 8601 / RFC 3339 timestamp for one-shot scheduling.
    /// When set, the task fires once at this time instead of using cron_expr.
    #[serde(default)]
    pub schedule_at: Option<String>,
    /// If true, the task is deleted after it runs once.
    #[serde(default)]
    pub delete_after_run: bool,
    /// Delivery mode: "announce" (default, send to channel), "webhook", or "none".
    #[serde(default = "default_delivery_mode")]
    pub delivery_mode: String,
    /// Webhook URL for delivery_mode="webhook".
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// If true, execute with an isolated (temporary) session.
    #[serde(default)]
    pub isolated: bool,
}

fn default_delivery_mode() -> String {
    "announce".into()
}

pub struct CronTool {
    data_dir: String,
    context: Arc<Mutex<ChatContext>>,
}

impl CronTool {
    pub fn new(data_dir: &str, context: Arc<Mutex<ChatContext>>) -> Self {
        Self {
            data_dir: data_dir.to_string(),
            context,
        }
    }

    fn cron_file(&self) -> String {
        format!("{}/cron.json", self.data_dir)
    }

    async fn load_tasks(&self) -> Vec<CronTask> {
        load_all_tasks(&self.cron_file()).await
    }

    async fn save_tasks(&self, tasks: &[CronTask]) -> Result<()> {
        let path = self.cron_file();
        let content = serde_json::to_string_pretty(tasks)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }
}

/// Load all cron tasks from the given file path.
pub async fn load_all_tasks(path: &str) -> Vec<CronTask> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Save all cron tasks to the given file path.
pub async fn save_all_tasks(path: &str, tasks: &[CronTask]) -> Result<()> {
    let content = serde_json::to_string_pretty(tasks)?;
    tokio::fs::write(path, content).await?;
    Ok(())
}

/// Check if a `schedule_at` timestamp matches the current time (within the same minute).
///
/// Accepts RFC 3339 (e.g. `2026-02-18T10:30:00+08:00`) or naive datetime
/// (`2026-02-18 10:30:00`, interpreted as local time).
pub fn schedule_at_matches(schedule_at: &str, now: &chrono::DateTime<chrono::Local>) -> bool {
    use chrono::TimeZone;

    let target_minute = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(schedule_at) {
        dt.with_timezone(&chrono::Local).timestamp() / 60
    } else if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(schedule_at, "%Y-%m-%d %H:%M:%S")
    {
        match chrono::Local.from_local_datetime(&naive).single() {
            Some(dt) => dt.timestamp() / 60,
            None => return false,
        }
    } else if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(schedule_at, "%Y-%m-%dT%H:%M:%S")
    {
        match chrono::Local.from_local_datetime(&naive).single() {
            Some(dt) => dt.timestamp() / 60,
            None => return false,
        }
    } else {
        warn!(schedule_at, "failed to parse schedule_at timestamp");
        return false;
    };

    let now_minute = now.timestamp() / 60;
    target_minute == now_minute
}

/// 5-field cron matching: `min hour dom mon dow`.
/// Supports `*`, literal numbers, comma-separated lists, ranges (`1-5`),
/// step values (`*/5`), and range+step (`1-30/2`).
pub fn cron_matches(expr: &str, now: &chrono::DateTime<chrono::Local>) -> bool {
    use chrono::Datelike;
    use chrono::Timelike;

    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        warn!(expr, "invalid cron expression (expected 5 fields)");
        return false;
    }

    let checks = [
        (fields[0], now.minute()),
        (fields[1], now.hour()),
        (fields[2], now.day()),
        (fields[3], now.month()),
        (fields[4], now.weekday().num_days_from_sunday()),
    ];

    checks.iter().all(|(field, value)| field_matches(field, *value))
}

/// Check if a single cron field matches a value.
/// Supports `*`, literal number, comma-separated list, ranges (`1-5`),
/// step values (`*/5`), and range+step (`1-30/2`).
fn field_matches(field: &str, value: u32) -> bool {
    if field == "*" {
        return true;
    }
    for part in field.split(',') {
        let part = part.trim();
        if part_matches(part, value) {
            return true;
        }
    }
    false
}

/// Check if a single comma-separated part matches a value.
/// Handles: `*`, `*/step`, `N`, `N-M`, `N-M/step`.
fn part_matches(part: &str, value: u32) -> bool {
    // Split on '/' for step values
    let (range_part, step) = if let Some((r, s)) = part.split_once('/') {
        let step = match s.parse::<u32>() {
            Ok(n) if n > 0 => n,
            _ => return false,
        };
        (r, Some(step))
    } else {
        (part, None)
    };

    match (range_part, step) {
        // */step — matches if value % step == 0
        ("*", Some(step)) => value % step == 0,
        // * without step — already handled above, but for safety
        ("*", None) => true,
        // N-M/step or N-M
        (r, _) if r.contains('-') => {
            if let Some((start_s, end_s)) = r.split_once('-') {
                let start = match start_s.parse::<u32>() {
                    Ok(n) => n,
                    Err(_) => return false,
                };
                let end = match end_s.parse::<u32>() {
                    Ok(n) => n,
                    Err(_) => return false,
                };
                if value < start || value > end {
                    return false;
                }
                match step {
                    Some(step) => (value - start) % step == 0,
                    None => true,
                }
            } else {
                false
            }
        }
        // Plain number
        (n, None) => n.parse::<u32>().ok() == Some(value),
        // Number/step doesn't make sense, but handle gracefully
        (n, Some(_step)) => n.parse::<u32>().ok() == Some(value),
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        "cron"
    }

    fn description(&self) -> &str {
        "Manage scheduled tasks. Can list, add, or remove cron-style scheduled tasks. \
         Tasks use standard cron expressions (e.g., '*/5 * * * *' for every 5 min, \
         '0 9-17 * * 1-5' for weekdays 9-17). Supports ranges, steps, and one-shot \
         scheduling via schedule_at."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "remove"],
                    "description": "Action: list all tasks, add a new task, or remove a task"
                },
                "cron_expr": {
                    "type": "string",
                    "description": "Cron expression (for add action, e.g., '0 7 * * *'). Not needed if schedule_at is set."
                },
                "schedule_at": {
                    "type": "string",
                    "description": "ISO 8601 timestamp for one-shot execution (e.g., '2026-02-18T10:30:00+08:00'). Overrides cron_expr."
                },
                "delete_after_run": {
                    "type": "boolean",
                    "description": "If true, delete the task after it runs once (default: true for schedule_at tasks)"
                },
                "description": {
                    "type": "string",
                    "description": "Human-readable description of the task"
                },
                "command": {
                    "type": "string",
                    "description": "Command or action to execute (e.g., 'check weather', 'run backup')"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (for remove action)"
                },
                "delivery_mode": {
                    "type": "string",
                    "enum": ["announce", "webhook", "none"],
                    "description": "How to deliver results: announce (send to chat, default), webhook (POST to URL), none (silent)"
                },
                "webhook_url": {
                    "type": "string",
                    "description": "URL for webhook delivery mode"
                },
                "isolated": {
                    "type": "boolean",
                    "description": "If true, execute with a fresh temporary session (no conversation history)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let action = input["action"].as_str().unwrap_or("list");

        match action {
            "list" => {
                let tasks = self.load_tasks().await;
                if tasks.is_empty() {
                    Ok("No scheduled tasks.".to_string())
                } else {
                    let list: Vec<String> = tasks
                        .iter()
                        .map(|t| {
                            let schedule = if let Some(ref at) = t.schedule_at {
                                format!("at {}", at)
                            } else {
                                t.cron_expr.clone()
                            };
                            let mut flags = Vec::new();
                            if t.delete_after_run {
                                flags.push("once");
                            }
                            if t.isolated {
                                flags.push("isolated");
                            }
                            if t.delivery_mode != "announce" {
                                flags.push(&t.delivery_mode);
                            }
                            let flag_str = if flags.is_empty() {
                                String::new()
                            } else {
                                format!(" [{}]", flags.join(","))
                            };
                            format!("- [{}] {} ({}): {}{}", t.id, t.description, schedule, t.command, flag_str)
                        })
                        .collect();
                    Ok(list.join("\n"))
                }
            }
            "add" => {
                let schedule_at = input["schedule_at"].as_str().map(String::from);
                let cron_expr = input["cron_expr"].as_str().unwrap_or("").to_string();

                // Must have either cron_expr or schedule_at
                if cron_expr.is_empty() && schedule_at.is_none() {
                    return Err(GatewayError::Tool {
                        tool: "cron".into(),
                        message: "either cron_expr or schedule_at is required for add".into(),
                    });
                }

                let description = input["description"]
                    .as_str()
                    .unwrap_or("unnamed task");
                let command = input["command"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "cron".into(),
                        message: "command is required for add".into(),
                    })?;

                // Default delete_after_run to true for schedule_at tasks
                let delete_after_run = input["delete_after_run"]
                    .as_bool()
                    .unwrap_or(schedule_at.is_some());

                let delivery_mode = input["delivery_mode"]
                    .as_str()
                    .unwrap_or("announce")
                    .to_string();
                let webhook_url = input["webhook_url"].as_str().map(String::from);
                let isolated = input["isolated"].as_bool().unwrap_or(false);

                // Auto-inject channel + chat_id from shared context
                let ctx = self.context.lock().await;

                let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
                let task = CronTask {
                    id: id.clone(),
                    cron_expr,
                    description: description.to_string(),
                    command: command.to_string(),
                    channel: ctx.channel.clone(),
                    chat_id: ctx.chat_id.clone(),
                    created_at: chrono::Utc::now().timestamp(),
                    last_run: None,
                    schedule_at,
                    delete_after_run,
                    delivery_mode,
                    webhook_url,
                    isolated,
                };

                let label = if task.schedule_at.is_some() {
                    format!("at {}", task.schedule_at.as_deref().unwrap_or("?"))
                } else {
                    task.cron_expr.clone()
                };

                let mut tasks = self.load_tasks().await;
                tasks.push(task);
                self.save_tasks(&tasks).await?;

                debug!(id = %id, schedule = %label, "added cron task");
                Ok(format!("Task added with ID: {}", id))
            }
            "remove" => {
                let task_id = input["task_id"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "cron".into(),
                        message: "task_id is required for remove".into(),
                    })?;

                let mut tasks = self.load_tasks().await;
                let before = tasks.len();
                tasks.retain(|t| t.id != task_id);

                if tasks.len() == before {
                    warn!(task_id, "cron task not found");
                    Ok(format!("Task {} not found", task_id))
                } else {
                    self.save_tasks(&tasks).await?;
                    debug!(task_id, "removed cron task");
                    Ok(format!("Task {} removed", task_id))
                }
            }
            _ => Err(GatewayError::Tool {
                tool: "cron".into(),
                message: format!("unknown action: {}", action),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cron_match_exact() {
        // "30 14 * * *" = 14:30 every day
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 14, 30, 0)
            .unwrap();
        assert!(cron_matches("30 14 * * *", &dt));
    }

    #[test]
    fn cron_match_wildcard() {
        // "* * * * *" = every minute
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 6, 1, 3, 45, 0)
            .unwrap();
        assert!(cron_matches("* * * * *", &dt));
    }

    #[test]
    fn cron_match_comma_list() {
        // "0,30 * * * *" = at minute 0 and 30
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 1, 1, 12, 30, 0)
            .unwrap();
        assert!(cron_matches("0,30 * * * *", &dt));

        let dt2 = chrono::Local
            .with_ymd_and_hms(2026, 1, 1, 12, 15, 0)
            .unwrap();
        assert!(!cron_matches("0,30 * * * *", &dt2));
    }

    #[test]
    fn cron_no_match() {
        // "0 7 * * *" = 7:00 AM — should not match 14:30
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 14, 30, 0)
            .unwrap();
        assert!(!cron_matches("0 7 * * *", &dt));
    }

    #[test]
    fn cron_match_range() {
        // "0 9-17 * * *" = every hour from 9 to 17 at minute 0
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 12, 0, 0)
            .unwrap();
        assert!(cron_matches("0 9-17 * * *", &dt));

        let dt_early = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 7, 0, 0)
            .unwrap();
        assert!(!cron_matches("0 9-17 * * *", &dt_early));
    }

    #[test]
    fn cron_match_step() {
        // "*/15 * * * *" = every 15 minutes (0, 15, 30, 45)
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 12, 30, 0)
            .unwrap();
        assert!(cron_matches("*/15 * * * *", &dt));

        let dt2 = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 12, 10, 0)
            .unwrap();
        assert!(!cron_matches("*/15 * * * *", &dt2));
    }

    #[test]
    fn cron_match_range_with_step() {
        // "1-30/2 * * * *" = odd minutes from 1-30 (1, 3, 5, ..., 29)
        let dt = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 12, 5, 0)
            .unwrap();
        assert!(cron_matches("1-30/2 * * * *", &dt));

        let dt2 = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 12, 6, 0)
            .unwrap();
        assert!(!cron_matches("1-30/2 * * * *", &dt2));

        // 31 is outside range
        let dt3 = chrono::Local
            .with_ymd_and_hms(2026, 2, 13, 12, 31, 0)
            .unwrap();
        assert!(!cron_matches("1-30/2 * * * *", &dt3));
    }

    #[test]
    fn field_matches_combinations() {
        // */5 at 0
        assert!(field_matches("*/5", 0));
        assert!(field_matches("*/5", 5));
        assert!(!field_matches("*/5", 3));

        // 1-5 range
        assert!(field_matches("1-5", 1));
        assert!(field_matches("1-5", 5));
        assert!(!field_matches("1-5", 0));
        assert!(!field_matches("1-5", 6));

        // comma + range mixed
        assert!(field_matches("0,10-15", 0));
        assert!(field_matches("0,10-15", 12));
        assert!(!field_matches("0,10-15", 5));
    }

    #[test]
    fn cron_task_serde_with_new_fields() {
        let task = CronTask {
            id: "abc123".into(),
            cron_expr: "0 7 * * *".into(),
            description: "morning alarm".into(),
            command: "good morning".into(),
            channel: "telegram".into(),
            chat_id: "42".into(),
            created_at: 1000000,
            last_run: Some(999999),
            schedule_at: Some("2026-02-18T10:30:00+08:00".into()),
            delete_after_run: true,
            delivery_mode: "webhook".into(),
            webhook_url: Some("https://example.com/hook".into()),
            isolated: true,
        };
        let json = serde_json::to_string(&task).unwrap();
        let back: CronTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel, "telegram");
        assert_eq!(back.chat_id, "42");
        assert_eq!(back.last_run, Some(999999));
        assert_eq!(back.schedule_at.as_deref(), Some("2026-02-18T10:30:00+08:00"));
        assert!(back.delete_after_run);
        assert_eq!(back.delivery_mode, "webhook");
        assert_eq!(back.webhook_url.as_deref(), Some("https://example.com/hook"));
        assert!(back.isolated);
    }

    #[test]
    fn cron_task_serde_backwards_compat() {
        // Old tasks without the new fields should deserialize with defaults
        let json = r#"{"id":"x","cron_expr":"* * * * *","description":"d","command":"c","channel":"","chat_id":"","created_at":0}"#;
        let task: CronTask = serde_json::from_str(json).unwrap();
        assert!(task.last_run.is_none());
        assert!(task.schedule_at.is_none());
        assert!(!task.delete_after_run);
        assert_eq!(task.delivery_mode, "announce");
        assert!(task.webhook_url.is_none());
        assert!(!task.isolated);
    }

    #[test]
    fn schedule_at_rfc3339_match() {
        let now = chrono::Local::now();
        let schedule = now.to_rfc3339();
        assert!(schedule_at_matches(&schedule, &now));
    }

    #[test]
    fn schedule_at_naive_match() {
        let now = chrono::Local::now();
        let naive = now.format("%Y-%m-%d %H:%M:%S").to_string();
        assert!(schedule_at_matches(&naive, &now));
    }

    #[test]
    fn schedule_at_no_match() {
        let now = chrono::Local::now();
        // 1 hour in the future
        let future = now + chrono::Duration::hours(1);
        let schedule = future.to_rfc3339();
        assert!(!schedule_at_matches(&schedule, &now));
    }
}
