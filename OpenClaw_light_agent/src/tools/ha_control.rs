//! Home Assistant control tool.
//!
//! Calls Home Assistant REST API to control smart home devices.

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::Tool;
use crate::config::HomeAssistantConfig;
use crate::error::{GatewayError, Result};

pub struct HaControlTool {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl HaControlTool {
    pub fn new(client: reqwest::Client, config: &HomeAssistantConfig) -> Self {
        Self {
            client,
            base_url: config.url.trim_end_matches('/').to_string(),
            token: config.token.clone(),
        }
    }
}

#[async_trait]
impl Tool for HaControlTool {
    fn name(&self) -> &str {
        "ha_control"
    }

    fn description(&self) -> &str {
        "Control Home Assistant devices. Can call services (turn_on, turn_off, set_temperature, etc.) \
         or query device states."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["call_service", "get_state"],
                    "description": "Action type"
                },
                "domain": {
                    "type": "string",
                    "description": "Service domain (e.g., light, climate, cover, switch)"
                },
                "service": {
                    "type": "string",
                    "description": "Service name (e.g., turn_on, turn_off, set_temperature)"
                },
                "entity_id": {
                    "type": "string",
                    "description": "Entity ID (e.g., light.living_room)"
                },
                "data": {
                    "type": "object",
                    "description": "Additional service data (e.g., temperature, brightness)"
                }
            },
            "required": ["action", "entity_id"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let action = input["action"].as_str().unwrap_or("get_state");
        let entity_id = input["entity_id"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "ha_control".into(),
                message: "entity_id is required".into(),
            })?;

        match action {
            "call_service" => {
                let domain = input["domain"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "ha_control".into(),
                        message: "domain is required for call_service".into(),
                    })?;
                let service = input["service"]
                    .as_str()
                    .ok_or_else(|| GatewayError::Tool {
                        tool: "ha_control".into(),
                        message: "service is required for call_service".into(),
                    })?;

                let url = format!("{}/api/services/{}/{}", self.base_url, domain, service);

                let mut body = if let Some(data) = input.get("data") {
                    data.clone()
                } else {
                    json!({})
                };

                // Ensure entity_id is in the body
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("entity_id".into(), json!(entity_id));
                }

                debug!(domain, service, entity_id, "calling HA service");

                let resp = self
                    .client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", self.token))
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| GatewayError::Tool {
                        tool: "ha_control".into(),
                        message: e.to_string(),
                    })?;

                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();

                if status.is_success() {
                    Ok(format!("Service {}/{} called successfully for {}", domain, service, entity_id))
                } else {
                    Ok(format!("Error {}: {}", status, text))
                }
            }
            "get_state" => {
                let url = format!("{}/api/states/{}", self.base_url, entity_id);

                debug!(entity_id, "querying HA state");

                let resp = self
                    .client
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", self.token))
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| GatewayError::Tool {
                        tool: "ha_control".into(),
                        message: e.to_string(),
                    })?;

                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();

                if status.is_success() {
                    Ok(text)
                } else {
                    Ok(format!("Error {}: {}", status, text))
                }
            }
            _ => Err(GatewayError::Tool {
                tool: "ha_control".into(),
                message: format!("unknown action: {}", action),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HomeAssistantConfig;

    fn mock_config(base_url: &str) -> HomeAssistantConfig {
        HomeAssistantConfig {
            url: base_url.to_string(),
            token: "test-token".into(),
        }
    }

    #[test]
    fn tool_metadata() {
        let tool = HaControlTool::new(reqwest::Client::new(), &mock_config("http://ha:8123"));
        assert_eq!(tool.name(), "ha_control");
        assert!(!tool.description().is_empty());
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
        assert!(required.contains(&json!("entity_id")));
    }

    #[tokio::test]
    async fn missing_entity_id_returns_error() {
        let tool = HaControlTool::new(reqwest::Client::new(), &mock_config("http://ha:8123"));
        let err = tool.execute(json!({"action": "get_state"})).await.unwrap_err();
        assert!(err.to_string().contains("entity_id"));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = HaControlTool::new(reqwest::Client::new(), &mock_config("http://ha:8123"));
        let err = tool
            .execute(json!({"action": "explode", "entity_id": "light.x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown action"));
    }

    #[tokio::test]
    async fn get_state_success() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/states/light.living_room"))
            .and(wiremock::matchers::header("Authorization", "Bearer test-token"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(r#"{"state":"on","attributes":{}}"#),
            )
            .mount(&server)
            .await;

        let tool = HaControlTool::new(reqwest::Client::new(), &mock_config(&server.uri()));
        let result = tool
            .execute(json!({"action": "get_state", "entity_id": "light.living_room"}))
            .await
            .unwrap();
        assert!(result.contains("on"));
    }

    #[tokio::test]
    async fn call_service_success() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/services/light/turn_on"))
            .and(wiremock::matchers::header("Authorization", "Bearer test-token"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;

        let tool = HaControlTool::new(reqwest::Client::new(), &mock_config(&server.uri()));
        let result = tool
            .execute(json!({
                "action": "call_service",
                "domain": "light",
                "service": "turn_on",
                "entity_id": "light.living_room"
            }))
            .await
            .unwrap();
        assert!(result.contains("successfully"));
    }

    #[tokio::test]
    async fn call_service_missing_domain() {
        let tool = HaControlTool::new(reqwest::Client::new(), &mock_config("http://ha:8123"));
        let err = tool
            .execute(json!({
                "action": "call_service",
                "entity_id": "light.x"
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("domain"));
    }
}
