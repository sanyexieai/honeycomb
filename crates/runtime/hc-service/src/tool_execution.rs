use anyhow::{Context, Result, bail};
use hc_protocol::ApiNamespace;
use hc_toolchain::ToolExecutionOutcome;
use serde_json::{Map, Value};

use crate::{
    ServiceConfig,
    tool::{McpToolCallRequest, call_configured_mcp_tool},
};

#[derive(Debug, Clone)]
pub struct ToolInvocationPlan {
    pub tool_id: String,
    pub goal: String,
    pub command: Vec<String>,
    pub kind: ToolInvocationKind,
}

#[derive(Debug, Clone)]
pub enum ToolInvocationKind {
    Mcp(McpToolInvocation),
}

#[derive(Debug, Clone)]
pub struct McpToolInvocation {
    pub namespace: ApiNamespace,
    pub session_id: Option<String>,
    pub server_id: String,
    pub tool_name: String,
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ToolInvocationOutcome {
    pub tool_id: String,
    pub goal: String,
    pub command: Vec<String>,
    pub success: bool,
    pub summary: String,
    pub observations: Vec<String>,
    pub raw_result: Option<Value>,
    pub server_id: Option<String>,
    pub tool_name: Option<String>,
}

impl ToolInvocationOutcome {
    pub fn into_tool_execution_outcome(self) -> ToolExecutionOutcome {
        ToolExecutionOutcome {
            tool_id: self.tool_id,
            parent_tool_id: None,
            invoked_tool_ids: Vec::new(),
            goal: self.goal,
            command: self.command,
            success: self.success,
            summary: self.summary,
            observations: self.observations,
        }
    }

    pub fn raw_result(&self) -> Result<&Value> {
        self.raw_result
            .as_ref()
            .context("tool invocation produced no raw result")
    }
}

pub fn execute_tool_invocation(
    config: &ServiceConfig,
    plan: &ToolInvocationPlan,
) -> Result<ToolInvocationOutcome> {
    match &plan.kind {
        ToolInvocationKind::Mcp(invocation) => {
            execute_mcp_tool_invocation(config, plan, invocation)
        }
    }
}

fn execute_mcp_tool_invocation(
    config: &ServiceConfig,
    plan: &ToolInvocationPlan,
    invocation: &McpToolInvocation,
) -> Result<ToolInvocationOutcome> {
    let response = call_configured_mcp_tool(
        config,
        McpToolCallRequest {
            namespace: invocation.namespace.clone(),
            tenant_id: None,
            user_id: None,
            session_id: invocation.session_id.clone(),
            server_id: invocation.server_id.clone(),
            tool_name: invocation.tool_name.clone(),
            arguments: invocation.arguments.clone(),
        },
    )?;
    let result = response.result;
    let success = !is_mcp_error(&result);
    Ok(ToolInvocationOutcome {
        tool_id: plan.tool_id.clone(),
        goal: plan.goal.clone(),
        command: plan.command.clone(),
        success,
        summary: if success {
            "mcp tool call completed".to_owned()
        } else {
            "mcp tool call returned an error result".to_owned()
        },
        observations: mcp_result_observations(&result),
        raw_result: Some(result),
        server_id: Some(invocation.server_id.clone()),
        tool_name: Some(invocation.tool_name.clone()),
    })
}

pub fn mcp_invocation_plan(
    tool_id: impl Into<String>,
    goal: impl Into<String>,
    command: Vec<String>,
    namespace: ApiNamespace,
    session_id: Option<String>,
    server_id: impl Into<String>,
    tool_name: impl Into<String>,
    arguments: Map<String, Value>,
) -> ToolInvocationPlan {
    ToolInvocationPlan {
        tool_id: tool_id.into(),
        goal: goal.into(),
        command,
        kind: ToolInvocationKind::Mcp(McpToolInvocation {
            namespace,
            session_id,
            server_id: server_id.into(),
            tool_name: tool_name.into(),
            arguments,
        }),
    }
}

pub fn mcp_result_observations(result: &Value) -> Vec<String> {
    let mut observations = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content.iter().take(40) {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                observations.push(format!("text: {text}"));
            } else {
                observations.push(format!("content: {item}"));
            }
        }
        if content.len() > 40 {
            observations.push("content: ... truncated".to_owned());
        }
    }
    if observations.is_empty() {
        observations.push(format!("result: {result}"));
    }
    observations
}

fn is_mcp_error(value: &Value) -> bool {
    value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn require_mcp_metadata(outcome: &ToolInvocationOutcome) -> Result<(&str, &str, &Value)> {
    let server_id = outcome
        .server_id
        .as_deref()
        .context("mcp invocation outcome missed server id")?;
    let tool_name = outcome
        .tool_name
        .as_deref()
        .context("mcp invocation outcome missed tool name")?;
    let raw_result = outcome.raw_result()?;
    if server_id.is_empty() || tool_name.is_empty() {
        bail!("mcp invocation outcome metadata was empty");
    }
    Ok((server_id, tool_name, raw_result))
}
