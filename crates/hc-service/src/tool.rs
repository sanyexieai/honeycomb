use anyhow::Result;
use hc_protocol::{ApiNamespace, McpServerListResponse, McpServerSummary};
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::McpServerRepository;

use crate::ServiceConfig;

pub fn list_mcp_servers(
    config: &ServiceConfig,
    namespace: ApiNamespace,
) -> Result<McpServerListResponse> {
    let repository = McpServerRepository::with_namespace(
        config.workspace_root.clone(),
        WorkspaceNamespace::new(namespace.tenant_id, namespace.user_id),
    );
    let servers = repository
        .list_servers()?
        .into_iter()
        .map(|server| McpServerSummary {
            id: server.id,
            name: server.name,
            description: server.description,
            enabled: server.enabled,
            transport: format!("{:?}", server.transport),
            url: server.url,
            command: server.command,
            tags: server.tags,
        })
        .collect();
    Ok(McpServerListResponse { servers })
}
