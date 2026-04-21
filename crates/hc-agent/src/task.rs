use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskBudget {
    pub token_budget: u32,
    pub time_budget_minutes: u32,
    pub evolution_reserve_tokens: u32,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            token_budget: 12_000,
            time_budget_minutes: 60,
            evolution_reserve_tokens: 1_200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskNamespace {
    pub tenant_id: String,
    pub user_id: String,
}

impl TaskNamespace {
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

impl Default for TaskNamespace {
    fn default() -> Self {
        Self::local_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRequest {
    pub id: String,
    #[serde(default)]
    pub namespace: TaskNamespace,
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub budget: TaskBudget,
    pub project_ref: Option<String>,
    pub context_refs: Vec<String>,
}

impl TaskRequest {
    pub fn new(id: impl Into<String>, title: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            namespace: TaskNamespace::local_default(),
            title: title.into(),
            goal: goal.into(),
            budget: TaskBudget::default(),
            project_ref: None,
            context_refs: Vec::new(),
        }
    }

    pub fn with_context_ref(mut self, context_ref: impl Into<String>) -> Self {
        self.context_refs.push(context_ref.into());
        self
    }

    pub fn with_namespace(mut self, namespace: TaskNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    pub fn with_project_ref(mut self, project_ref: impl Into<String>) -> Self {
        self.project_ref = Some(project_ref.into());
        self
    }

    pub fn with_budget(mut self, budget: TaskBudget) -> Self {
        self.budget = budget;
        self
    }

    pub fn with_token_budget(mut self, token_budget: u32) -> Self {
        self.budget.token_budget = token_budget;
        self
    }

    pub fn with_time_budget_minutes(mut self, minutes: u32) -> Self {
        self.budget.time_budget_minutes = minutes;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskContext {
    pub session_id: Option<String>,
    pub initiating_instance_id: Option<String>,
    pub notes: Vec<String>,
}
