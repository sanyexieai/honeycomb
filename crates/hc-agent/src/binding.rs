use serde::{Deserialize, Serialize};
use hc_responder::ResponderBinding;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindingNamespace {
    pub tenant_id: String,
    pub user_id: String,
}

impl BindingNamespace {
    pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        }
    }

    pub fn local_default() -> Self {
        Self::new("local", "default")
    }
}

impl Default for BindingNamespace {
    fn default() -> Self {
        Self::local_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeBinding {
    pub instance_id: String,
    #[serde(default)]
    pub namespace: BindingNamespace,
    pub persona_ref: Option<String>,
    pub capability_refs: Vec<String>,
    pub memory_scope_refs: Vec<String>,
    pub responder_binding_ref: Option<String>,
    pub responder: Option<ResponderBinding>,
}

impl AgentRuntimeBinding {
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            namespace: BindingNamespace::local_default(),
            persona_ref: None,
            capability_refs: Vec::new(),
            memory_scope_refs: Vec::new(),
            responder_binding_ref: None,
            responder: None,
        }
    }

    pub fn with_namespace(mut self, namespace: BindingNamespace) -> Self {
        self.namespace = namespace;
        self
    }
}
