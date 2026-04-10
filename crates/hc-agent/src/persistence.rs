use std::path::{Path, PathBuf};

use anyhow::Result;
use hc_capability::CapabilityRepository;
use hc_memory::{MemoryNamespace, MemoryRepository, MemoryVisibility};
use hc_persona::PersonaRepository;
use hc_store::store::WorkspaceNamespace;
use serde::{Deserialize, Serialize};

use crate::{
    bootstrap::MaterializedAgent,
    incubation::{IncubationReport, build_memory_record_from_report},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedAgentAssets {
    pub persona_path: PathBuf,
    pub capability_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedIncubationArtifacts {
    pub memory_path: PathBuf,
}

pub fn persist_materialized_agents(
    workspace_root: impl AsRef<Path>,
    agents: &[MaterializedAgent],
) -> Result<Vec<PersistedAgentAssets>> {
    let workspace_root = workspace_root.as_ref();
    let mut persisted = Vec::new();

    for agent in agents {
        let namespace = WorkspaceNamespace::new(
            agent.persona.namespace.tenant_id.clone(),
            agent.persona.namespace.user_id.clone(),
        );
        let persona_repo = PersonaRepository::with_namespace(workspace_root, namespace.clone());
        let capability_repo = CapabilityRepository::with_namespace(workspace_root, namespace);

        let persona_path = persona_repo.write_profile(&agent.persona)?;
        let mut capability_paths = Vec::new();
        for capability in &agent.capabilities {
            capability_paths.push(capability_repo.write_profile(capability)?);
        }

        persisted.push(PersistedAgentAssets {
            persona_path,
            capability_paths,
        });
    }

    Ok(persisted)
}

pub fn persist_incubation_report(
    workspace_root: impl AsRef<Path>,
    namespace: WorkspaceNamespace,
    report: &IncubationReport,
) -> Result<PersistedIncubationArtifacts> {
    let repository =
        MemoryRepository::with_namespace(workspace_root.as_ref().to_path_buf(), namespace.clone());
    let record = build_memory_record_from_report(report)
        .with_namespace(MemoryNamespace::new(namespace.tenant_id, namespace.user_id))
        .with_visibility(MemoryVisibility::Private);
    let memory_path = repository.write_record(&record)?;

    Ok(PersistedIncubationArtifacts { memory_path })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::Context;
    use hc_core::{RuntimeNamespace, RuntimeSupervisor};

    use crate::{
        IncubationObservation, IncubationReport, PromotionDecision, TaskNamespace, TaskRequest,
        bootstrap_task, materialize_plan,
    };

    use super::*;

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("honeycomb-{}-{}-{}", name, std::process::id(), nanos))
    }

    #[test]
    fn materialized_agents_can_be_persisted_to_workspace() {
        let root = unique_temp_dir("agent-persist");
        let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
            .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
        let plan = bootstrap_task(&task);
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session_in_namespace(
            "demo",
            RuntimeNamespace::new("tenant-a", "user-a"),
        );

        let agents = materialize_plan(&mut runtime, &session.id, &plan)
            .context("plan should materialize")
            .expect("materialization should succeed");

        let persisted = persist_materialized_agents(&root, &agents)
            .context("agents should persist")
            .expect("persistence should succeed");

        assert_eq!(persisted.len(), agents.len());
        assert!(persisted[0].persona_path.exists());
        assert_eq!(persisted[0].capability_paths.len(), 1);
        assert!(persisted[0].capability_paths[0].exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incubation_reports_can_be_persisted_to_memory_workspace() {
        let root = unique_temp_dir("incubation-persist");
        let report = IncubationReport {
            task_id: "task.demo".to_owned(),
            instance_id: "instance.0001".to_owned(),
            observations: vec![IncubationObservation {
                kind: "strength".to_owned(),
                detail: "handled review well".to_owned(),
            }],
            promotion: PromotionDecision::ContinueIncubating,
        };

        let persisted = persist_incubation_report(
            &root,
            WorkspaceNamespace::new("tenant-a", "user-a"),
            &report,
        )
        .context("incubation report should persist")
        .expect("memory persistence should succeed");

        assert!(persisted.memory_path.exists());
        assert!(persisted
            .memory_path
            .to_string_lossy()
            .replace('/', "\\")
            .contains("tenants\\tenant-a\\users\\user-a\\memory\\task"));

        let _ = fs::remove_dir_all(root);
    }
}
