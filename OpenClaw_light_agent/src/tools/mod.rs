//! Tool registry and trait definition.
//!
//! Each tool implements the `Tool` trait. The agent invokes tools
//! by name during the ReAct loop.

pub mod agent_tool;
pub mod backup;
pub mod cron;
pub mod exec;
pub mod file;
pub mod get_time;
pub mod ha_control;
pub mod html_utils;
pub mod mcp;
pub mod memory;
pub mod web_fetch;
pub mod web_search;

use async_trait::async_trait;

use crate::channel::types::ToolDefinition;
use crate::error::Result;

/// Trait for agent tools (ha_control, web_fetch, get_time, cron, etc.).
///
/// Adding a new tool requires implementing this trait in ~20-50 lines.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name as used in LLM function calling.
    fn name(&self) -> &str;

    /// Human-readable description of what this tool does.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> serde_json::Value;

    /// Execute the tool with the given input and return a text result.
    async fn execute(&self, input: serde_json::Value) -> Result<String>;
}

/// Tool registry that holds all available tools.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        Self { tools }
    }

    /// Get tool definitions for LLM function calling.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Execute a tool by name.
    pub async fn execute(&self, name: &str, input: serde_json::Value) -> Result<String> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| crate::error::GatewayError::Tool {
                tool: name.to_string(),
                message: "tool not found".to_string(),
            })?;

        tool.execute(input).await
    }
}
