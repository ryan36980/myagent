//! Get current time tool.

use async_trait::async_trait;
use serde_json::json;

use super::Tool;
use crate::error::Result;

pub struct GetTimeTool;

impl GetTimeTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GetTimeTool {
    fn name(&self) -> &str {
        "get_time"
    }

    fn description(&self) -> &str {
        "Get the current date and time. Useful for time-aware responses and scheduling."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "timezone": {
                    "type": "string",
                    "description": "Timezone offset like +08:00 (default: local time)"
                }
            }
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String> {
        let now = chrono::Local::now();
        Ok(now.format("%Y-%m-%d %H:%M:%S %Z").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_metadata() {
        let tool = GetTimeTool::new();
        assert_eq!(tool.name(), "get_time");
        assert!(!tool.description().is_empty());
        assert!(tool.input_schema().is_object());
    }

    #[tokio::test]
    async fn returns_current_datetime() {
        let tool = GetTimeTool::new();
        let result = tool.execute(json!({})).await.unwrap();
        // Should match YYYY-MM-DD HH:MM:SS pattern
        assert!(result.len() >= 19, "too short: {result}");
        assert!(result.contains('-'), "missing date separator: {result}");
        assert!(result.contains(':'), "missing time separator: {result}");
    }
}
