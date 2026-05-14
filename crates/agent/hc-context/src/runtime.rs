use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hc_memory::MemoryNamespace;
use hc_store::store::WorkspaceNamespace;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use hc_protocol::{DEFAULT_TENANT_ID, DEFAULT_USER_ID};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeVariableScope {
    Global,
    Project,
    Tenant,
    User,
    Session,
    Task,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeVariableLayer {
    pub scope: RuntimeVariableScope,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
}

impl RuntimeVariableLayer {
    pub fn new(scope: RuntimeVariableScope) -> Self {
        Self {
            scope,
            values: Map::new(),
            source: None,
        }
    }

    pub fn with_value(mut self, key: impl Into<String>, value: Value) -> Self {
        self.values.insert(key.into(), value);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
}

impl RuntimeIdentity {
    pub fn new(
        tenant_id: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        let tenant_id = normalize_or_default(tenant_id.into(), DEFAULT_TENANT_ID);
        let user_id = normalize_or_default(user_id.into(), DEFAULT_USER_ID);
        let session_id = normalize_optional(session_id.into())
            .unwrap_or_else(|| default_session_id(&tenant_id, &user_id));
        Self {
            tenant_id,
            user_id,
            session_id,
        }
    }

    pub fn from_optional(
        tenant_id: Option<String>,
        user_id: Option<String>,
        session_id: Option<String>,
    ) -> Self {
        let tenant_id = tenant_id
            .and_then(normalize_optional)
            .unwrap_or_else(|| DEFAULT_TENANT_ID.to_owned());
        let user_id = user_id
            .and_then(normalize_optional)
            .unwrap_or_else(|| DEFAULT_USER_ID.to_owned());
        let session_id = session_id
            .and_then(normalize_optional)
            .unwrap_or_else(|| default_session_id(&tenant_id, &user_id));
        Self {
            tenant_id,
            user_id,
            session_id,
        }
    }

    pub fn workspace_namespace(&self) -> WorkspaceNamespace {
        WorkspaceNamespace::new(self.tenant_id.clone(), self.user_id.clone())
    }

    pub fn memory_namespace(&self) -> MemoryNamespace {
        MemoryNamespace::new(self.tenant_id.clone(), self.user_id.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeVariables {
    pub identity: RuntimeIdentity,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(default)]
    pub layers: Vec<RuntimeVariableLayer>,
}

impl RuntimeVariables {
    pub fn new(identity: RuntimeIdentity) -> Self {
        let mut values = Map::new();
        insert_identity_values(&mut values, &identity);
        Self {
            identity,
            values,
            layers: Vec::new(),
        }
    }

    pub fn from_layers(identity: RuntimeIdentity, layers: Vec<RuntimeVariableLayer>) -> Self {
        let mut runtime = Self::new(identity);
        for layer in layers {
            runtime.merge_layer(layer);
        }
        runtime
    }

    pub fn merge_layer(&mut self, layer: RuntimeVariableLayer) {
        for (key, value) in &layer.values {
            self.values.insert(key.clone(), value.clone());
        }
        insert_identity_values(&mut self.values, &self.identity);
        self.layers.push(layer);
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn mcp_arguments(&self) -> Map<String, Value> {
        let mut arguments = Map::new();
        insert_identity_values(&mut arguments, &self.identity);
        arguments.insert("runtime".to_owned(), Value::Object(self.values.clone()));
        arguments
    }

    pub fn inject_mcp_arguments(&self, arguments: &mut Map<String, Value>) {
        arguments
            .entry("tenant_id".to_owned())
            .or_insert_with(|| Value::String(self.identity.tenant_id.clone()));
        arguments
            .entry("user_id".to_owned())
            .or_insert_with(|| Value::String(self.identity.user_id.clone()));
        arguments
            .entry("session_id".to_owned())
            .or_insert_with(|| Value::String(self.identity.session_id.clone()));
        arguments
            .entry("runtime".to_owned())
            .or_insert_with(|| Value::Object(self.values.clone()));
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeVariableRepository {
    root: PathBuf,
}

impl RuntimeVariableRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn load(
        &self,
        identity: RuntimeIdentity,
        task_values: Option<Map<String, Value>>,
    ) -> Result<RuntimeVariables> {
        let mut layers = Vec::new();
        for (scope, path) in variable_layer_paths(&self.root, &identity) {
            if let Some(layer) = read_variable_layer(scope, &path)? {
                layers.push(layer);
            }
        }
        if let Some(values) = task_values {
            layers.push(RuntimeVariableLayer {
                scope: RuntimeVariableScope::Task,
                values,
                source: None,
            });
        }
        Ok(RuntimeVariables::from_layers(identity, layers))
    }
}

pub fn default_session_id(tenant_id: &str, user_id: &str) -> String {
    format!("session.{tenant_id}.{user_id}.default")
}

pub fn normalize_optional(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

pub fn runtime_identity_prompt(identity: &RuntimeIdentity) -> String {
    format!(
        "Runtime identity context:\n- tenant_id: {}\n- user_id: {}\n- session_id: {}\n\nUse these runtime identifiers for platform services and tool calls when needed. Do not ask the user for tenant IDs, user IDs, session IDs, device IDs, MCP server IDs, tool names, or other implementation identifiers.",
        identity.tenant_id, identity.user_id, identity.session_id
    )
}

fn normalize_or_default(value: String, default: &str) -> String {
    normalize_optional(value).unwrap_or_else(|| default.to_owned())
}

fn insert_identity_values(values: &mut Map<String, Value>, identity: &RuntimeIdentity) {
    values.insert(
        "tenant_id".to_owned(),
        Value::String(identity.tenant_id.clone()),
    );
    values.insert(
        "user_id".to_owned(),
        Value::String(identity.user_id.clone()),
    );
    values.insert(
        "session_id".to_owned(),
        Value::String(identity.session_id.clone()),
    );
}

fn variable_layer_paths(
    root: &Path,
    identity: &RuntimeIdentity,
) -> Vec<(RuntimeVariableScope, PathBuf)> {
    let tenant_root = root.join("tenants").join(&identity.tenant_id);
    let user_root = tenant_root.join("users").join(&identity.user_id);
    vec![
        (
            RuntimeVariableScope::Global,
            root.join("runtime").join("variables.json"),
        ),
        (
            RuntimeVariableScope::Project,
            root.join("project").join("variables.json"),
        ),
        (
            RuntimeVariableScope::Tenant,
            tenant_root.join("variables.json"),
        ),
        (RuntimeVariableScope::User, user_root.join("variables.json")),
        (
            RuntimeVariableScope::Session,
            user_root
                .join("sessions")
                .join(format!("{}.variables.json", identity.session_id)),
        ),
    ]
}

fn read_variable_layer(
    scope: RuntimeVariableScope,
    path: &Path,
) -> Result<Option<RuntimeVariableLayer>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read runtime variables {}", path.display()))?;
    let values = parse_variable_values(&content)
        .with_context(|| format!("failed to parse runtime variables {}", path.display()))?;
    Ok(Some(RuntimeVariableLayer {
        scope,
        values,
        source: Some(path.to_owned()),
    }))
}

fn parse_variable_values(content: &str) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_str(content)?;
    match value {
        Value::Object(mut object) => {
            if let Some(Value::Object(values)) = object.remove("values") {
                Ok(values)
            } else {
                Ok(object)
            }
        }
        other => {
            let mut values = Map::new();
            values.insert("value".to_owned(), other);
            Ok(values)
        }
    }
}

pub fn task_values(
    entries: impl IntoIterator<Item = (impl Into<String>, Value)>,
) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn runtime_identity_defaults_blank_values() {
        let identity =
            RuntimeIdentity::from_optional(Some(" ".to_owned()), Some("user-1".to_owned()), None);

        assert_eq!(identity.tenant_id, DEFAULT_TENANT_ID);
        assert_eq!(identity.user_id, "user-1");
        assert_eq!(identity.session_id, "session.local.user-1.default");
    }

    #[test]
    fn runtime_repository_merges_layers_in_order() {
        let root = unique_temp_dir("hc-runtime-test");
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("project")).unwrap();
        fs::create_dir_all(root.join("tenants/t1/users/u1/sessions")).unwrap();
        fs::write(
            root.join("runtime/variables.json"),
            r#"{"locale":"en-US","theme":"global"}"#,
        )
        .unwrap();
        fs::write(
            root.join("project/variables.json"),
            r#"{"values":{"theme":"project","timezone":"UTC"}}"#,
        )
        .unwrap();
        fs::write(
            root.join("tenants/t1/users/u1/variables.json"),
            r#"{"locale":"zh-CN"}"#,
        )
        .unwrap();
        fs::write(
            root.join("tenants/t1/users/u1/sessions/session.t1.u1.default.variables.json"),
            r#"{"turn":"active"}"#,
        )
        .unwrap();

        let identity =
            RuntimeIdentity::from_optional(Some("t1".to_owned()), Some("u1".to_owned()), None);
        let runtime = RuntimeVariableRepository::new(&root)
            .load(
                identity,
                Some(task_values([(
                    "deadline",
                    Value::String("soon".to_owned()),
                )])),
            )
            .unwrap();

        assert_eq!(runtime.value("locale").unwrap(), "zh-CN");
        assert_eq!(runtime.value("theme").unwrap(), "project");
        assert_eq!(runtime.value("timezone").unwrap(), "UTC");
        assert_eq!(runtime.value("turn").unwrap(), "active");
        assert_eq!(runtime.value("deadline").unwrap(), "soon");
        assert_eq!(runtime.value("tenant_id").unwrap(), "t1");
        assert_eq!(runtime.layers.len(), 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_injects_mcp_arguments_without_overwriting_explicit_values() {
        let runtime = RuntimeVariables::new(RuntimeIdentity::from_optional(
            Some("t1".to_owned()),
            Some("u1".to_owned()),
            Some("s1".to_owned()),
        ));
        let mut arguments = Map::new();
        arguments.insert("user_id".to_owned(), Value::String("explicit".to_owned()));

        runtime.inject_mcp_arguments(&mut arguments);

        assert_eq!(arguments.get("tenant_id").unwrap(), "t1");
        assert_eq!(arguments.get("user_id").unwrap(), "explicit");
        assert_eq!(arguments.get("session_id").unwrap(), "s1");
        assert!(arguments.get("runtime").is_some());
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{now}"))
    }
}
