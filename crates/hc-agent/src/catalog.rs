//! 将工作区根目录下的能力定义（`agent-definitions/`）、用户命名空间下的 `agents/`，以及可选的本会话目录（`agent-runtime/sessions/<slug>/agent/`）合并为路由与聊天使用的有效配置。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};

use crate::profile::{
    AgentDefinitionLayer, AgentProfile, AgentProfileFrontmatter, AgentRepository,
};
use crate::session_bundle::load_session_agent_profile;

/// 工作区相对路径：每个子目录代表一种能力 agent，内含 `agent.md` 及可选提示词片段。
pub const WORKSPACE_AGENT_DEFINITIONS_DIR: &str = "agent-definitions";

#[derive(Debug, Clone)]
pub struct AgentCatalog {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl AgentCatalog {
    pub fn new(root: impl Into<std::path::PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    /// 工作区能力定义 + 用户 `agents/` + 可选「本会话」`agent-runtime/sessions/<slug>/agent/`。
    pub fn list_effective_profiles(&self) -> Result<Vec<AgentProfile>> {
        self.list_effective_profiles_with_session(None)
    }

    /// 与 [`Self::list_effective_profiles`] 相同，但若提供 `session_key` 则并入本会话目录下的 agent（高优先级、独立 id）。
    pub fn list_effective_profiles_with_session(
        &self,
        session_key: Option<&str>,
    ) -> Result<Vec<AgentProfile>> {
        let workspace = load_workspace_capability_profiles(&self.store)?;
        let user = AgentRepository::with_namespace(self.store.root().to_path_buf(), self.namespace.clone())
            .list_profiles()?;
        let mut merged = merge_workspace_and_user_agents(workspace, user)?;
        if let Some(key) = session_key.map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(session) = load_session_agent_profile(&self.store, &self.namespace, key)? {
                merged.retain(|profile| profile.id != session.id);
                merged.push(session);
            }
        }
        merged.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(merged)
    }
}

/// 仅加载工作区能力层（不读用户目录）。
pub fn load_workspace_capability_profiles(store: &WorkspaceStore) -> Result<Vec<AgentProfile>> {
    let defs_root = store.resolve(WORKSPACE_AGENT_DEFINITIONS_DIR);
    if !defs_root.exists() {
        return Ok(Vec::new());
    }

    let mut slugs = Vec::new();
    for entry in fs::read_dir(&defs_root).with_context(|| format!("failed to read {}", defs_root.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.is_empty() {
                slugs.push(name);
            }
        }
    }
    slugs.sort();

    let mut out = Vec::new();
    for slug in slugs {
        let agent_md_rel = Path::new(WORKSPACE_AGENT_DEFINITIONS_DIR).join(&slug).join("agent.md");
        if !store.resolve(&agent_md_rel).exists() {
            continue;
        }
        let stored: StoredMarkdown<AgentProfileFrontmatter> = store.read_markdown(&agent_md_rel)?;
        let mut profile = AgentProfile::from_document(stored.frontmatter, stored.body)?;
        profile.definition_layer = AgentDefinitionLayer::WorkspaceCapability;
        profile.relative_path = agent_md_rel.to_string_lossy().replace('\\', "/");

        let dir = store.resolve(Path::new(WORKSPACE_AGENT_DEFINITIONS_DIR).join(&slug));
        let mut extra = String::new();
        for (label, file) in [
            ("system_prompt.md", dir.join("system_prompt.md")),
            ("prompt.md", dir.join("prompt.md")),
        ] {
            if let Ok(text) = fs::read_to_string(&file) {
                let t = text.trim();
                if !t.is_empty() {
                    if !extra.is_empty() {
                        extra.push_str("\n\n");
                    }
                    extra.push_str(&format!("### {label}\n\n{t}"));
                }
            }
        }
        if let Ok(text) = fs::read_to_string(dir.join("description.md")) {
            profile.capability_description = text.trim().to_owned();
        }
        if !extra.trim().is_empty() {
            profile.instructions = format!("{}\n\n{}", extra.trim(), profile.instructions.trim());
        }
        out.push(profile);
    }

    Ok(out)
}

fn merge_workspace_and_user_agents(
    workspace: Vec<AgentProfile>,
    user: Vec<AgentProfile>,
) -> Result<Vec<AgentProfile>> {
    let mut by_workspace_id: HashMap<String, AgentProfile> = HashMap::new();
    for w in workspace {
        by_workspace_id.insert(w.id.clone(), w);
    }

    let mut merged = Vec::new();
    let mut same_id_merged = HashSet::<String>::new();

    for u in user {
        if let Some(parent_id) = u
            .extends_workspace_agent
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if parent_id != u.id.as_str() {
                if let Some(base) = by_workspace_id.get(parent_id).cloned() {
                    merged.push(merge_runtime_onto_workspace(base, u));
                    continue;
                }
            }
        }

        if let Some(base) = by_workspace_id.get(&u.id).cloned() {
            same_id_merged.insert(u.id.clone());
            merged.push(merge_runtime_onto_workspace(base, u));
            continue;
        }

        merged.push(u);
    }

    for (wid, w) in by_workspace_id {
        if !same_id_merged.contains(&wid) {
            merged.push(w);
        }
    }

    merged.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(merged)
}

fn merge_runtime_onto_workspace(base: AgentProfile, overlay: AgentProfile) -> AgentProfile {
    let mut out = overlay;
    out.definition_layer = AgentDefinitionLayer::UserRuntime;
    out.extends_workspace_agent = None;

    if out.name.trim().is_empty() {
        out.name = base.name.clone();
    }

    out.instructions = format!(
        "[Workspace capability: {}]\n{}\n\n[Runtime overlay]\n{}",
        base.id,
        base.instructions.trim(),
        out.instructions.trim()
    );

    let desc_o = out.capability_description.trim();
    let desc_b = base.capability_description.trim();
    if desc_o.is_empty() && !desc_b.is_empty() {
        out.capability_description = base.capability_description.clone();
    }

    out.intent_hints = merge_unique_strings(base.intent_hints, out.intent_hints);
    out.routing_examples = merge_unique_strings(base.routing_examples, out.routing_examples);
    out.negative_routing_examples = merge_unique_strings(
        base.negative_routing_examples,
        out.negative_routing_examples,
    );
    out.tool_refs = merge_unique_strings(base.tool_refs, out.tool_refs);
    out.memory_scope_refs = merge_unique_strings(base.memory_scope_refs, out.memory_scope_refs);
    out.prompt_refs = merge_unique_strings(base.prompt_refs, out.prompt_refs);
    out.tags = merge_unique_strings(base.tags, out.tags);

    out.project_id = out.project_id.or(base.project_id);
    out.domain_id = out.domain_id.or(base.domain_id);
    out
}

fn merge_unique_strings(mut base: Vec<String>, extra: Vec<String>) -> Vec<String> {
    for s in extra {
        if !base.iter().any(|existing| existing == &s) {
            base.push(s);
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::AgentKind;
    use crate::session_bundle::ensure_session_agent_bundle;
    use hc_conversation::ConversationPolicy;
    use tempfile::tempdir;

    fn write_workspace_agent(
        root: &std::path::Path,
        slug: &str,
        id: &str,
        body: &str,
    ) -> Result<()> {
        let dir = root.join(WORKSPACE_AGENT_DEFINITIONS_DIR).join(slug);
        fs::create_dir_all(&dir)?;
        let fm = format!(
            "---\nid: {id}\ntype: agent_profile\ntitle: Test\ntags: []\n---\n\n{body}"
        );
        fs::write(dir.join("agent.md"), fm)?;
        Ok(())
    }

    #[test]
    fn same_id_merges_workspace_into_runtime_instructions() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_workspace_agent(root, "demo.agent", "demo.agent", "base body").unwrap();
        fs::write(
            root.join(WORKSPACE_AGENT_DEFINITIONS_DIR)
                .join("demo.agent")
                .join("system_prompt.md"),
            "SYS",
        )
        .unwrap();

        let store = WorkspaceStore::new(root);
        let ws = load_workspace_capability_profiles(&store).unwrap();
        assert_eq!(ws.len(), 1);
        assert!(ws[0].instructions.contains("SYS"));

        let user = AgentProfile {
            id: "demo.agent".to_owned(),
            name: "U".to_owned(),
            kind: AgentKind::Router,
            project_id: None,
            domain_id: None,
            priority: 10,
            intent_hints: vec!["hint".to_owned()],
            routing_examples: vec![],
            negative_routing_examples: vec![],
            tool_refs: vec![],
            memory_scope_refs: vec![],
            prompt_refs: vec![],
            tags: vec![],
            responder_ref: None,
            state_schema_ref: None,
            conversation_policy: ConversationPolicy::default(),
            definition_layer: AgentDefinitionLayer::UserRuntime,
            extends_workspace_agent: None,
            status: None,
            capability_description: String::new(),
            instructions: "user only".to_owned(),
            relative_path: String::new(),
        };

        let merged = merge_workspace_and_user_agents(ws, vec![user]).unwrap();
        assert_eq!(merged.len(), 1);
        assert!(merged[0].instructions.contains("base body"));
        assert!(merged[0].instructions.contains("user only"));
        assert_eq!(merged[0].definition_layer, AgentDefinitionLayer::UserRuntime);
    }

    #[test]
    fn extends_different_id_keeps_workspace_parent_visible() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write_workspace_agent(root, "core", "core.router", "core").unwrap();

        let store = WorkspaceStore::new(root);
        let ws = load_workspace_capability_profiles(&store).unwrap();

        let user = AgentProfile {
            id: "my.router".to_owned(),
            name: "Mine".to_owned(),
            kind: AgentKind::Router,
            project_id: None,
            domain_id: None,
            priority: 5,
            intent_hints: vec![],
            routing_examples: vec![],
            negative_routing_examples: vec![],
            tool_refs: vec![],
            memory_scope_refs: vec![],
            prompt_refs: vec![],
            tags: vec![],
            responder_ref: None,
            state_schema_ref: None,
            conversation_policy: ConversationPolicy::default(),
            definition_layer: AgentDefinitionLayer::UserRuntime,
            extends_workspace_agent: Some("core.router".to_owned()),
            status: None,
            capability_description: String::new(),
            instructions: "mine".to_owned(),
            relative_path: String::new(),
        };

        let merged = merge_workspace_and_user_agents(ws, vec![user]).unwrap();
        let ids: Vec<_> = merged.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"core.router"));
        assert!(ids.contains(&"my.router"));
    }

    #[test]
    fn session_bundle_merges_session_runtime_profile() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let store = WorkspaceStore::new(root);
        let ns = WorkspaceNamespace::local_default();
        let session = "unit-session-alpha";
        ensure_session_agent_bundle(&store, &ns, session).unwrap();
        let catalog = AgentCatalog::new(root, ns);
        let profiles = catalog
            .list_effective_profiles_with_session(Some(session))
            .unwrap();
        let session_profile = profiles
            .iter()
            .find(|p| p.definition_layer == AgentDefinitionLayer::SessionRuntime)
            .expect("session profile");
        assert!(session_profile.id.starts_with("agent.session."));
        assert_eq!(session_profile.status.as_deref(), Some("temporary"));
        assert!(session_profile.priority >= 2_000_000_000);
    }
}
