//! ADR-006 **experience lane** policy stubs (Part B-0).
//!
//! Full outcome capture / curator / playbook projection is not implemented here; this module only
//! centralizes **gates** so future callers can ask “is learning allowed for this task?” without
//! scattering env reads.

use crate::TaskRequest;

fn env_flag_truthy(raw: Option<String>) -> bool {
    raw.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Workspace / deployment allows learning features (tenant-wide stub until multi-tenant config exists).
#[must_use]
pub fn tenant_learning_allowed_from_env() -> bool {
    env_flag_truthy(std::env::var("HC_TENANT_LEARNING_ALLOWED").ok())
}

/// Effective gate: task opt-in **and** tenant/env allowance.
#[must_use]
pub fn task_learning_effective(task: &TaskRequest) -> bool {
    task.enable_learning && tenant_learning_allowed_from_env()
}
