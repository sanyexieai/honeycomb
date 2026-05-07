//! Human responder inbox (Markdown-backed) for API/CLI reuse.

use anyhow::Result;
use hc_protocol::ApiNamespace;
use hc_responder::{HumanInboxItem, HumanInboxRepository};
use hc_store::store::WorkspaceNamespace;

use crate::ServiceConfig;

fn workspace_namespace(namespace: &ApiNamespace) -> WorkspaceNamespace {
    WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

fn inbox_repository(config: &ServiceConfig, namespace: ApiNamespace) -> HumanInboxRepository {
    HumanInboxRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace(&namespace),
    )
}

pub fn list_human_inbox_pending(
    config: &ServiceConfig,
    namespace: ApiNamespace,
) -> Result<Vec<HumanInboxItem>> {
    inbox_repository(config, namespace).list_pending()
}

pub fn complete_human_inbox_item(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    item_id: &str,
    response_body: &str,
    answered_at_ms: u64,
) -> Result<String> {
    let path = inbox_repository(config, namespace).complete_pending(
        item_id,
        response_body,
        answered_at_ms,
    )?;
    Ok(path.to_string_lossy().replace('\\', "/"))
}
