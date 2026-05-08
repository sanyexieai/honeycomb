//! Per-chat-session swarm binding slot: at most one `active_task_id` (ADR-004 Phase 2).

use std::path::Path;

use anyhow::{Context, Result};
use hc_bootstrap::wall_clock_ms;
use hc_context::runtime::{default_session_id, DEFAULT_TENANT_ID, DEFAULT_USER_ID};
use hc_protocol::ChatRequest;
use hc_store::{
    store::{WorkspaceNamespace, WorkspaceStore},
    task_coordination::coordination_segment_slug,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionSwarmActiveTaskRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_task_id: Option<String>,
    #[serde(default)]
    pub(crate) updated_at_ms: u64,
}

/// Same session key as swarm observability (`emit_swarm_observability_from_classified`).
pub(crate) fn swarm_session_key(request: &ChatRequest) -> String {
    let tenant_raw = request.memory.namespace.tenant_id.trim();
    let user_raw = request.memory.namespace.user_id.trim();
    let tenant_eff = if tenant_raw.is_empty() {
        DEFAULT_TENANT_ID
    } else {
        tenant_raw
    };
    let user_eff = if user_raw.is_empty() {
        DEFAULT_USER_ID
    } else {
        user_raw
    };
    request
        .session_id
        .as_ref()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_session_id(tenant_eff, user_eff))
}

fn relative_record_path(session_key: &str) -> std::path::PathBuf {
    let slug = coordination_segment_slug(session_key);
    let fname = if slug.is_empty() {
        "default.json".to_owned()
    } else {
        format!("{slug}.json")
    };
    std::path::PathBuf::from("conversation")
        .join("session_swarm")
        .join(fname)
}

pub(crate) fn load_session_swarm_active_task_id(
    workspace_root: &Path,
    namespace: &WorkspaceNamespace,
    session_key: &str,
) -> Result<Option<String>> {
    let store = WorkspaceStore::new(workspace_root);
    let rel = relative_record_path(session_key);
    let abs = store.resolve_in_namespace(namespace, &rel);
    if !abs.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&abs)
        .with_context(|| format!("read session swarm state {}", abs.display()))?;
    let record: SessionSwarmActiveTaskRecord = serde_json::from_str(&raw)
        .with_context(|| format!("parse session swarm state {}", abs.display()))?;
    Ok(record.active_task_id)
}

/// When the client omits [`ChatRequest::active_task_id`], returns the last persisted task id for `session_key`.
pub(crate) fn persisted_conversation_active_task_hint(
    workspace_root: &Path,
    namespace: &WorkspaceNamespace,
    request: &ChatRequest,
) -> Option<String> {
    if request.active_task_id.is_some() {
        return None;
    }
    let key = swarm_session_key(request);
    load_session_swarm_active_task_id(workspace_root, namespace, &key).ok().flatten()
}

pub(crate) fn persist_session_swarm_active_task_binding(
    workspace_root: &Path,
    namespace: &WorkspaceNamespace,
    session_key: &str,
    active_task_id: Option<&str>,
) -> Result<()> {
    let store = WorkspaceStore::new(workspace_root);
    let rel = relative_record_path(session_key);
    let record = SessionSwarmActiveTaskRecord {
        active_task_id: active_task_id.map(str::to_owned),
        updated_at_ms: wall_clock_ms(),
    };
    let payload = serde_json::to_string_pretty(&record).context("serialize session swarm state")?;
    store.write_text_in_namespace(namespace, rel, &payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hc_protocol::{ApiMemoryQuery, ApiNamespace};

    fn chat_request(session_id: Option<&str>) -> ChatRequest {
        ChatRequest {
            tenant_id: Some(DEFAULT_TENANT_ID.to_owned()),
            user_id: Some(DEFAULT_USER_ID.to_owned()),
            session_id: session_id.map(|s| s.to_owned()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: None,
            messages: vec![],
            model: None,
            provider: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace {
                    tenant_id: DEFAULT_TENANT_ID.to_owned(),
                    user_id: DEFAULT_USER_ID.to_owned(),
                },
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        }
    }

    #[test]
    fn swarm_session_key_stable_with_defaults() {
        let r = chat_request(None);
        let a = swarm_session_key(&r);
        let b = swarm_session_key(&r);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn roundtrip_persist_then_load() {
        let dir =
            std::env::temp_dir().join(format!("hc-session-swarm-state-{}", wall_clock_ms()));
        let ns = WorkspaceNamespace::local_default();
        let session = "sess-test-1";
        persist_session_swarm_active_task_binding(
            &dir,
            &ns,
            session,
            Some("task-alpha"),
        )
        .unwrap();
        let loaded = load_session_swarm_active_task_id(&dir, &ns, session).unwrap();
        assert_eq!(loaded.as_deref(), Some("task-alpha"));
        persist_session_swarm_active_task_binding(&dir, &ns, session, None).unwrap();
        let cleared = load_session_swarm_active_task_id(&dir, &ns, session).unwrap();
        assert_eq!(cleared, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
