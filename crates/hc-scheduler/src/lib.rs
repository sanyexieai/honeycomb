use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use hc_core::{
    JobRecord, RunMode, RunRequest, RuntimeCommand, RuntimeCommandResult, RuntimeSupervisor,
};
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStatus {
    Active,
    Paused,
    Cancelled,
}

impl Default for ScheduleStatus {
    fn default() -> Self {
        Self::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    Once,
    Interval,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleSpec {
    pub kind: ScheduleKind,
    pub run_at_unix: Option<u64>,
    pub interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTargetKind {
    Agent,
    Tool,
    Mcp,
    Command,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledTarget {
    pub kind: ScheduledTargetKind,
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default)]
    pub args: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    RunOnce,
    Skip,
}

impl Default for MisfirePolicy {
    fn default() -> Self {
        Self::RunOnce
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    Skip,
    Queue,
    Replace,
}

impl Default for OverlapPolicy {
    fn default() -> Self {
        Self::Skip
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulePolicy {
    #[serde(default)]
    pub misfire: MisfirePolicy,
    #[serde(default)]
    pub max_retries: u32,
    #[serde(default)]
    pub retry_delay_seconds: u64,
    #[serde(default)]
    pub overlap: OverlapPolicy,
}

impl Default for SchedulePolicy {
    fn default() -> Self {
        Self {
            misfire: MisfirePolicy::RunOnce,
            max_retries: 3,
            retry_delay_seconds: 60,
            overlap: OverlapPolicy::Skip,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleState {
    pub next_fire_at_unix: Option<u64>,
    pub last_fire_at_unix: Option<u64>,
    pub last_run_id: Option<String>,
    #[serde(default)]
    pub failure_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledTask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: ScheduleStatus,
    pub schedule: ScheduleSpec,
    pub target: ScheduledTarget,
    #[serde(default)]
    pub policy: SchedulePolicy,
    #[serde(default)]
    pub state: ScheduleState,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledRun {
    pub id: String,
    pub schedule_id: String,
    pub status: ScheduledRunStatus,
    pub scheduled_for_unix: u64,
    pub started_at_unix: Option<u64>,
    pub finished_at_unix: Option<u64>,
    pub target: ScheduledTarget,
    pub result_ref: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DispatchReceipt {
    pub run_id: String,
    pub target_kind: ScheduledTargetKind,
    pub target_ref: String,
    pub result_ref: Option<String>,
    pub status: String,
}

pub trait ScheduledDispatch {
    fn dispatch(&mut self, run: &ScheduledRun) -> Result<DispatchReceipt>;
}

pub struct CommandDispatchAdapter<'a> {
    runtime: &'a mut RuntimeSupervisor,
    instance_id: String,
}

impl<'a> CommandDispatchAdapter<'a> {
    pub fn new(runtime: &'a mut RuntimeSupervisor, instance_id: impl Into<String>) -> Self {
        Self {
            runtime,
            instance_id: instance_id.into(),
        }
    }

    pub fn run_request_from_target(target: &ScheduledTarget) -> Result<RunRequest> {
        if target.kind != ScheduledTargetKind::Command {
            bail!("command adapter only supports command targets");
        }
        let program = target
            .args
            .get("program")
            .and_then(Value::as_str)
            .or_else(|| (!target.r#ref.trim().is_empty()).then_some(target.r#ref.as_str()))
            .context("command target requires program or ref")?
            .to_owned();
        let args = target
            .args
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cwd = target
            .args
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let run_mode = target
            .args
            .get("run_mode")
            .and_then(Value::as_str)
            .map(parse_run_mode)
            .transpose()?
            .unwrap_or(RunMode::Process);
        let interactive = target
            .args
            .get("interactive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let allow_child_instance = target
            .args
            .get("allow_child_instance")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(RunRequest {
            program,
            args,
            cwd,
            run_mode,
            interactive,
            allow_child_instance,
        })
    }
}

impl ScheduledDispatch for CommandDispatchAdapter<'_> {
    fn dispatch(&mut self, run: &ScheduledRun) -> Result<DispatchReceipt> {
        let (receipt, _) = self.dispatch_command_run(run)?;
        Ok(receipt)
    }
}

impl CommandDispatchAdapter<'_> {
    pub fn dispatch_command_run(
        &mut self,
        run: &ScheduledRun,
    ) -> Result<(DispatchReceipt, JobRecord)> {
        let run_request = Self::run_request_from_target(&run.target)?;
        let job = match self.runtime.dispatch(RuntimeCommand::SubmitRunRequest {
            instance_id: self.instance_id.clone(),
            title: format!("scheduled run {}", run.id),
            run_request,
        })? {
            RuntimeCommandResult::Job(job) => job,
            other => bail!("unexpected runtime dispatch result: {other:?}"),
        };
        let receipt = DispatchReceipt {
            run_id: run.id.clone(),
            target_kind: run.target.kind.clone(),
            target_ref: run.target.r#ref.clone(),
            result_ref: Some(job.id.clone()),
            status: "queued".to_owned(),
        };
        Ok((receipt, job))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScheduledTaskFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    #[serde(default)]
    status: ScheduleStatus,
    schedule: ScheduleSpec,
    target: ScheduledTarget,
    #[serde(default)]
    policy: SchedulePolicy,
    #[serde(default)]
    state: ScheduleState,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScheduledRunFrontmatter {
    id: String,
    r#type: String,
    tenant_id: String,
    user_id: String,
    schedule_id: String,
    status: ScheduledRunStatus,
    scheduled_for_unix: u64,
    started_at_unix: Option<u64>,
    finished_at_unix: Option<u64>,
    target: ScheduledTarget,
    result_ref: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScheduleRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl ScheduleRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(schedule_id: &str) -> PathBuf {
        PathBuf::from("scheduled").join(format!("{}.md", slugify(schedule_id)))
    }

    pub fn run_relative_path_for(run_id: &str) -> PathBuf {
        PathBuf::from("scheduled")
            .join("runs")
            .join(format!("{}.md", slugify(run_id)))
    }

    pub fn write_schedule(&self, task: &ScheduledTask) -> Result<PathBuf> {
        validate_scheduled_task(task)?;
        let relative_path = if task.relative_path.trim().is_empty() {
            Self::relative_path_for(&task.id)
        } else {
            PathBuf::from(&task.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &ScheduledTaskFrontmatter::from_task(task, &self.namespace),
            task.notes.trim(),
        )
    }

    pub fn read_schedule(&self, relative_path: impl AsRef<Path>) -> Result<ScheduledTask> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<ScheduledTaskFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut task = ScheduledTask::from_document(stored.frontmatter, stored.body)?;
        task.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(task)
    }

    pub fn list_schedules(&self) -> Result<Vec<ScheduledTask>> {
        let root = self
            .store
            .resolve_in_namespace(&self.namespace, PathBuf::from("scheduled"));
        if !root.exists() {
            return Ok(Vec::new());
        }
        let namespace_root = self.store.resolve_in_namespace(&self.namespace, "");
        let mut paths = Vec::new();
        collect_markdown_files(&root, &mut paths)?;
        paths.retain(|path| {
            !path
                .components()
                .any(|component| component.as_os_str() == "runs")
        });
        paths.sort();

        let mut schedules = Vec::new();
        for path in paths {
            let relative = path.strip_prefix(&namespace_root).with_context(|| {
                format!("schedule path not under namespace: {}", path.display())
            })?;
            schedules.push(self.read_schedule(relative)?);
        }
        schedules.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(schedules)
    }

    pub fn get_schedule(&self, schedule_id: &str) -> Result<ScheduledTask> {
        self.read_schedule(Self::relative_path_for(schedule_id))
    }

    pub fn set_schedule_status(
        &self,
        schedule_id: &str,
        status: ScheduleStatus,
    ) -> Result<ScheduledTask> {
        let mut task = self.get_schedule(schedule_id)?;
        task.status = status;
        self.write_schedule(&task)?;
        Ok(task)
    }

    pub fn due_schedules(&self, now_unix: u64) -> Result<Vec<ScheduledTask>> {
        Ok(self
            .list_schedules()?
            .into_iter()
            .filter(|task| is_due(task, now_unix))
            .collect())
    }

    pub fn queue_due_runs(&self, now_unix: u64) -> Result<Vec<ScheduledRun>> {
        let mut runs = Vec::new();
        let existing_runs = self.list_runs()?;
        for mut task in self.due_schedules(now_unix)? {
            if task.policy.overlap == OverlapPolicy::Skip
                && has_unfinished_run(&existing_runs, &task.id)
            {
                task.state.next_fire_at_unix = next_fire_after(&task, now_unix);
                self.write_schedule(&task)?;
                continue;
            }
            let scheduled_for_unix = task
                .state
                .next_fire_at_unix
                .or(task.schedule.run_at_unix)
                .unwrap_or(now_unix);
            let run_id = format!("scheduled-run.{}.{}", now_unix, runs.len() + 1);
            let run = ScheduledRun {
                id: run_id,
                schedule_id: task.id.clone(),
                status: ScheduledRunStatus::Queued,
                scheduled_for_unix,
                started_at_unix: None,
                finished_at_unix: None,
                target: task.target.clone(),
                result_ref: None,
                error: None,
                relative_path: String::new(),
            };
            self.write_run(&run)?;
            task.state.last_fire_at_unix = Some(now_unix);
            task.state.last_run_id = Some(run.id.clone());
            task.state.next_fire_at_unix = next_fire_after(&task, now_unix);
            if task.state.next_fire_at_unix.is_none() && task.schedule.kind == ScheduleKind::Once {
                task.status = ScheduleStatus::Cancelled;
            }
            self.write_schedule(&task)?;
            runs.push(run);
        }
        Ok(runs)
    }

    pub fn write_run(&self, run: &ScheduledRun) -> Result<PathBuf> {
        let relative_path = if run.relative_path.trim().is_empty() {
            Self::run_relative_path_for(&run.id)
        } else {
            PathBuf::from(&run.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &ScheduledRunFrontmatter::from_run(run, &self.namespace),
            "",
        )
    }

    pub fn read_run(&self, relative_path: impl AsRef<Path>) -> Result<ScheduledRun> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<ScheduledRunFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut run = ScheduledRun::from_document(stored.frontmatter)?;
        run.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(run)
    }

    pub fn list_runs(&self) -> Result<Vec<ScheduledRun>> {
        let root = self
            .store
            .resolve_in_namespace(&self.namespace, PathBuf::from("scheduled").join("runs"));
        if !root.exists() {
            return Ok(Vec::new());
        }
        let namespace_root = self.store.resolve_in_namespace(&self.namespace, "");
        let mut paths = Vec::new();
        collect_markdown_files(&root, &mut paths)?;
        paths.sort();

        let mut runs = Vec::new();
        for path in paths {
            let relative = path.strip_prefix(&namespace_root).with_context(|| {
                format!("scheduled run path not under namespace: {}", path.display())
            })?;
            runs.push(self.read_run(relative)?);
        }
        runs.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(runs)
    }

    pub fn queued_runs(&self) -> Result<Vec<ScheduledRun>> {
        Ok(self
            .list_runs()?
            .into_iter()
            .filter(|run| run.status == ScheduledRunStatus::Queued)
            .collect())
    }
}

impl ScheduledTask {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        schedule: ScheduleSpec,
        target: ScheduledTarget,
    ) -> Self {
        let schedule = schedule;
        let next_fire_at_unix = schedule.run_at_unix;
        Self {
            id: id.into(),
            title: title.into(),
            status: ScheduleStatus::Active,
            schedule,
            target,
            policy: SchedulePolicy::default(),
            state: ScheduleState {
                next_fire_at_unix,
                ..ScheduleState::default()
            },
            tags: vec!["scheduled".to_owned()],
            notes: String::new(),
            relative_path: String::new(),
        }
    }

    fn from_document(frontmatter: ScheduledTaskFrontmatter, body: String) -> Result<Self> {
        if frontmatter.r#type != "scheduled_task" {
            bail!("unsupported scheduled task type: {}", frontmatter.r#type);
        }
        let task = Self {
            id: frontmatter.id,
            title: frontmatter.title,
            status: frontmatter.status,
            schedule: frontmatter.schedule,
            target: frontmatter.target,
            policy: frontmatter.policy,
            state: frontmatter.state,
            tags: frontmatter.tags,
            notes: body.trim().to_owned(),
            relative_path: String::new(),
        };
        validate_scheduled_task(&task)?;
        Ok(task)
    }
}

impl ScheduledTaskFrontmatter {
    fn from_task(task: &ScheduledTask, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: task.id.clone(),
            r#type: "scheduled_task".to_owned(),
            title: task.title.clone(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            status: task.status.clone(),
            schedule: task.schedule.clone(),
            target: task.target.clone(),
            policy: task.policy.clone(),
            state: task.state.clone(),
            tags: task.tags.clone(),
        }
    }
}

impl ScheduledRun {
    fn from_document(frontmatter: ScheduledRunFrontmatter) -> Result<Self> {
        if frontmatter.r#type != "scheduled_run" {
            bail!("unsupported scheduled run type: {}", frontmatter.r#type);
        }
        Ok(Self {
            id: frontmatter.id,
            schedule_id: frontmatter.schedule_id,
            status: frontmatter.status,
            scheduled_for_unix: frontmatter.scheduled_for_unix,
            started_at_unix: frontmatter.started_at_unix,
            finished_at_unix: frontmatter.finished_at_unix,
            target: frontmatter.target,
            result_ref: frontmatter.result_ref,
            error: frontmatter.error,
            relative_path: String::new(),
        })
    }
}

impl ScheduledRunFrontmatter {
    fn from_run(run: &ScheduledRun, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: run.id.clone(),
            r#type: "scheduled_run".to_owned(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            schedule_id: run.schedule_id.clone(),
            status: run.status.clone(),
            scheduled_for_unix: run.scheduled_for_unix,
            started_at_unix: run.started_at_unix,
            finished_at_unix: run.finished_at_unix,
            target: run.target.clone(),
            result_ref: run.result_ref.clone(),
            error: run.error.clone(),
        }
    }
}

pub fn is_due(task: &ScheduledTask, now_unix: u64) -> bool {
    task.status == ScheduleStatus::Active
        && task
            .state
            .next_fire_at_unix
            .or(task.schedule.run_at_unix)
            .is_some_and(|next| next <= now_unix)
}

pub fn next_fire_after(task: &ScheduledTask, now_unix: u64) -> Option<u64> {
    match task.schedule.kind {
        ScheduleKind::Once => None,
        ScheduleKind::Interval => {
            let interval = task.schedule.interval_seconds?;
            if interval == 0 {
                return None;
            }
            let mut next = task
                .state
                .next_fire_at_unix
                .or(task.schedule.run_at_unix)
                .unwrap_or(now_unix)
                .saturating_add(interval);
            while next <= now_unix {
                match task.policy.misfire {
                    MisfirePolicy::RunOnce => {
                        next = now_unix.saturating_add(interval);
                        break;
                    }
                    MisfirePolicy::Skip => {
                        next = next.saturating_add(interval);
                    }
                }
            }
            Some(next)
        }
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn parse_run_mode(value: &str) -> Result<RunMode> {
    match value {
        "process" => Ok(RunMode::Process),
        "pty" => Ok(RunMode::Pty),
        "auto" => Ok(RunMode::Auto),
        other => bail!("unsupported run mode: {other}"),
    }
}

fn validate_scheduled_task(task: &ScheduledTask) -> Result<()> {
    if task.id.trim().is_empty() {
        bail!("scheduled task id cannot be empty");
    }
    if task.title.trim().is_empty() {
        bail!("scheduled task title cannot be empty");
    }
    if task.target.r#ref.trim().is_empty() {
        bail!("scheduled task target ref cannot be empty");
    }
    match task.schedule.kind {
        ScheduleKind::Once => {
            if task.schedule.run_at_unix.is_none() {
                bail!("once schedule requires run_at_unix");
            }
        }
        ScheduleKind::Interval => {
            if task.schedule.interval_seconds.unwrap_or(0) == 0 {
                bail!("interval schedule requires interval_seconds > 0");
            }
        }
    }
    Ok(())
}

fn has_unfinished_run(runs: &[ScheduledRun], schedule_id: &str) -> bool {
    runs.iter().any(|run| {
        run.schedule_id == schedule_id
            && matches!(
                run.status,
                ScheduledRunStatus::Queued | ScheduledRunStatus::Running
            )
    })
}

fn collect_markdown_files(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, paths)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '.' | '-' | '_') {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hc_core::RuntimeCommand;

    fn target() -> ScheduledTarget {
        ScheduledTarget {
            kind: ScheduledTargetKind::Event,
            r#ref: "event.demo".to_owned(),
            action: Some("wake".to_owned()),
            args: Map::new(),
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hc-scheduler-{name}-{}", now_unix()))
    }

    #[test]
    fn once_schedule_is_due_and_then_cancelled() {
        let root = temp_root("once");
        let repo = ScheduleRepository::new(&root);
        let task = ScheduledTask::new(
            "schedule.demo.once",
            "Demo Once",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(10),
                interval_seconds: None,
            },
            target(),
        );
        repo.write_schedule(&task).unwrap();

        assert_eq!(repo.due_schedules(9).unwrap().len(), 0);
        assert_eq!(repo.due_schedules(10).unwrap().len(), 1);
        let runs = repo.queue_due_runs(10).unwrap();
        assert_eq!(runs.len(), 1);
        let stored_runs = repo.list_runs().unwrap();
        assert_eq!(stored_runs.len(), 1);
        assert_eq!(stored_runs[0].schedule_id, "schedule.demo.once");
        let stored = repo
            .read_schedule(ScheduleRepository::relative_path_for("schedule.demo.once"))
            .unwrap();
        assert_eq!(stored.status, ScheduleStatus::Cancelled);
        assert_eq!(stored.state.last_run_id, Some(runs[0].id.clone()));
    }

    #[test]
    fn interval_schedule_reschedules_after_due_run() {
        let root = temp_root("interval");
        let repo = ScheduleRepository::new(&root);
        let task = ScheduledTask::new(
            "schedule.demo.interval",
            "Demo Interval",
            ScheduleSpec {
                kind: ScheduleKind::Interval,
                run_at_unix: Some(10),
                interval_seconds: Some(60),
            },
            target(),
        );
        repo.write_schedule(&task).unwrap();

        let runs = repo.queue_due_runs(70).unwrap();
        assert_eq!(runs.len(), 1);
        let stored = repo
            .read_schedule(ScheduleRepository::relative_path_for(
                "schedule.demo.interval",
            ))
            .unwrap();
        assert_eq!(stored.status, ScheduleStatus::Active);
        assert_eq!(stored.state.next_fire_at_unix, Some(130));
    }

    #[test]
    fn overlap_skip_does_not_queue_again_while_run_is_unfinished() {
        let root = temp_root("overlap-skip");
        let repo = ScheduleRepository::new(&root);
        let task = ScheduledTask::new(
            "schedule.demo.overlap",
            "Demo Overlap",
            ScheduleSpec {
                kind: ScheduleKind::Interval,
                run_at_unix: Some(10),
                interval_seconds: Some(60),
            },
            target(),
        );
        repo.write_schedule(&task).unwrap();

        let first_runs = repo.queue_due_runs(10).unwrap();
        assert_eq!(first_runs.len(), 1);
        let second_runs = repo.queue_due_runs(70).unwrap();
        assert!(second_runs.is_empty());

        let stored = repo
            .read_schedule(ScheduleRepository::relative_path_for(
                "schedule.demo.overlap",
            ))
            .unwrap();
        assert_eq!(stored.state.next_fire_at_unix, Some(130));
        assert_eq!(repo.queued_runs().unwrap().len(), 1);
    }

    #[test]
    fn command_adapter_submits_runtime_job_without_running_it() {
        let mut args = Map::new();
        args.insert(
            "args".to_owned(),
            Value::Array(vec![Value::String("hello".to_owned())]),
        );
        let run = ScheduledRun {
            id: "scheduled-run.demo".to_owned(),
            schedule_id: "schedule.demo.command".to_owned(),
            status: ScheduledRunStatus::Queued,
            scheduled_for_unix: 10,
            started_at_unix: None,
            finished_at_unix: None,
            target: ScheduledTarget {
                kind: ScheduledTargetKind::Command,
                r#ref: "echo".to_owned(),
                action: None,
                args,
            },
            result_ref: None,
            error: None,
            relative_path: String::new(),
        };
        let mut runtime = RuntimeSupervisor::new();
        let session = match runtime
            .dispatch(RuntimeCommand::CreateSession {
                name: "scheduler-test".to_owned(),
                namespace: None,
            })
            .unwrap()
        {
            RuntimeCommandResult::Session(session) => session,
            other => panic!("unexpected result: {other:?}"),
        };
        let instance = match runtime
            .dispatch(RuntimeCommand::CreateInstance {
                session_id: session.id,
                name: "scheduler".to_owned(),
                parent_instance_id: None,
            })
            .unwrap()
        {
            RuntimeCommandResult::Instance(instance) => instance,
            other => panic!("unexpected result: {other:?}"),
        };
        let mut adapter = CommandDispatchAdapter::new(&mut runtime, instance.id);
        let (receipt, job) = adapter.dispatch_command_run(&run).unwrap();
        assert_eq!(receipt.result_ref, Some(job.id.clone()));
        assert_eq!(job.run_request.program, "echo");
        assert_eq!(job.run_request.args, vec!["hello"]);
        assert_eq!(job.state, hc_core::JobState::Queued);
    }
}
