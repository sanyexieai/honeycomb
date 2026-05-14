//! Swarm/task collaboration types aligned with ADR-001 — ADR-005.
//!
//! See `docs/adr/` for routing tiers, persistence, bindings, outward speaker,
//! artifacts.

/// First routing rules revision label; bump when heuristic tables change materially.
pub const ROUTING_RULE_VERSION_V1: &str = "routing_rules_v1";

pub const INTENT_HASH_VERSION_V1: &str = "intent_hash_v1";

pub const ARTIFACT_SCHEMA_V1: &str = "artifact_schema_v1";

// --- ADR-001 routing ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingTier {
    /// Direct conversational reply
    L1,
    /// Implicit micro-task behind conversation
    L2,
    /// Explicit planning / multi work-item collaboration
    L3,
}

impl RoutingTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::L2 => "l2",
            Self::L3 => "l3",
        }
    }
}

impl std::fmt::Display for RoutingTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoutingDecisionRecord {
    pub routing_tier: RoutingTier,
    pub routing_reason: String,
    #[serde(default)]
    pub routing_signals: Vec<String>,
    #[serde(default)]
    pub routing_forced_by_user: bool,
    pub routing_rule_version: String,
}

impl RoutingDecisionRecord {
    pub fn l1_simple(reason: impl Into<String>) -> Self {
        Self {
            routing_tier: RoutingTier::L1,
            routing_reason: reason.into(),
            routing_signals: Vec::new(),
            routing_forced_by_user: false,
            routing_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
        }
    }
}

/// Single inbound message: ADR-001 routing + ADR-004 task binding (trace / JSONL / fixtures).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwarmRoutingBindingSnapshot {
    pub routing: RoutingDecisionRecord,
    pub task_binding: TaskBindingDecisionRecord,
}

impl SwarmRoutingBindingSnapshot {
    #[must_use]
    pub fn new(routing: RoutingDecisionRecord, task_binding: TaskBindingDecisionRecord) -> Self {
        Self {
            routing,
            task_binding,
        }
    }
}

// --- ADR-004 task binding observability ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskBindingAction {
    /// Keep using the conversation's active task id
    ReuseActiveTask,
    CreateImplicitTask,
    ClearBinding,
    /// Conversation-only turn; task scope unchanged or N/A
    NoChange,
}

impl TaskBindingAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReuseActiveTask => "reuse_active_task",
            Self::CreateImplicitTask => "create_implicit_task",
            Self::ClearBinding => "clear_binding",
            Self::NoChange => "no_change",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskBindingDecisionRecord {
    /// None when no task is bound after the decision
    pub active_task_id: Option<String>,
    pub task_binding_action: TaskBindingAction,
    pub task_binding_reason: String,
    #[serde(default)]
    pub task_binding_signals: Vec<String>,
    pub task_binding_rule_version: String,
}

impl TaskBindingDecisionRecord {
    pub fn conversation_only(reason: impl Into<String>) -> Self {
        Self {
            active_task_id: None,
            task_binding_action: TaskBindingAction::NoChange,
            task_binding_reason: reason.into(),
            task_binding_signals: Vec::new(),
            task_binding_rule_version: ROUTING_RULE_VERSION_V1.to_owned(),
        }
    }
}

// --- ADR-003 work-item lifecycle ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemLifecycleState {
    Planned,
    Claiming,
    Assigned,
    Blocked,
    Done,
    Cancelled,
}

impl WorkItemLifecycleState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Claiming => "claiming",
            Self::Assigned => "assigned",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

impl std::fmt::Display for WorkItemLifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedExitKind {
    UserResume,
    PlannerReplan,
    /// Time out while blocked; item returns to `claiming` for another claim round.
    TimeoutRequeue,
    /// User or operator cancels from `blocked`.
    ManualCancel,
    /// Time out with **no retry** (abandon) — terminal per ADR-003 (distinct from `manual_cancel` in audits).
    TimeoutAbandon,
}

impl BlockedExitKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserResume => "user_resume",
            Self::PlannerReplan => "planner_replan",
            Self::TimeoutRequeue => "timeout_requeue",
            Self::ManualCancel => "manual_cancel",
            Self::TimeoutAbandon => "timeout_abandon",
        }
    }
}

impl std::fmt::Display for BlockedExitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Next work-item state after exiting `blocked` (ADR-003 § “P0 transition targets”).
///
/// Product note: **`UserResume`** and **`PlannerReplan`** may move to **`claiming`** or **`assigned`**;
/// P0 default is **`claiming`** so reassignment stays explicit in traces.
#[must_use]
pub const fn blocked_exit_next_state_v1(exit: BlockedExitKind) -> WorkItemLifecycleState {
    match exit {
        BlockedExitKind::TimeoutRequeue => WorkItemLifecycleState::Claiming,
        BlockedExitKind::ManualCancel | BlockedExitKind::TimeoutAbandon => {
            WorkItemLifecycleState::Cancelled
        }
        BlockedExitKind::UserResume | BlockedExitKind::PlannerReplan => {
            WorkItemLifecycleState::Claiming
        }
    }
}

/// P0 command applying a single lifecycle step (ADR-003 nominal graph). Orchestration layers should
/// use this for deterministic tests and as a checklist against persisted state updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemLifecycleCommandV1 {
    /// Move from `planned` toward claim collection.
    SubmitForClaiming,
    /// Winner selected while in `claiming`; results in exactly one assignment row in persisted models.
    AssignWinner,
    MarkDone,
    EnterBlocked,
    ExitBlocked(BlockedExitKind),
    Cancel,
}

/// Invalid `(state, command)` pair for [`apply_work_item_lifecycle_command_v1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItemLifecycleTransitionError {
    AlreadyTerminal,
    InvalidCommandForState,
}

impl std::fmt::Display for WorkItemLifecycleTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyTerminal => f.write_str("work item already in terminal state"),
            Self::InvalidCommandForState => {
                f.write_str("command is invalid for current lifecycle state")
            }
        }
    }
}

impl std::error::Error for WorkItemLifecycleTransitionError {}

/// Apply one P0 lifecycle command. Terminal states (`done`, `cancelled`) reject all commands.
///
/// **Assignment rule (ADR-003)**: [`WorkItemLifecycleCommandV1::AssignWinner`] is only valid in
/// `claiming`; a second assign without returning to `claiming` is rejected (at most one active
/// assignment episode per work item in P0).
#[must_use]
pub fn apply_work_item_lifecycle_command_v1(
    from: WorkItemLifecycleState,
    command: WorkItemLifecycleCommandV1,
) -> Result<WorkItemLifecycleState, WorkItemLifecycleTransitionError> {
    use WorkItemLifecycleCommandV1::*;
    use WorkItemLifecycleState::*;

    if from.is_terminal() {
        return Err(WorkItemLifecycleTransitionError::AlreadyTerminal);
    }

    let next = match (from, command) {
        (Planned, SubmitForClaiming) => Claiming,
        (Planned, Cancel) => Cancelled,
        (Claiming, AssignWinner) => Assigned,
        (Claiming, Cancel) => Cancelled,
        (Assigned, MarkDone) => Done,
        (Assigned, EnterBlocked) => Blocked,
        (Assigned, Cancel) => Cancelled,
        (Blocked, ExitBlocked(exit)) => blocked_exit_next_state_v1(exit),
        _ => {
            return Err(WorkItemLifecycleTransitionError::InvalidCommandForState);
        }
    };

    Ok(next)
}

/// Returns true when a new **assignment** may be recorded for this work item (in `claiming` only).
///
/// ADR-003: at most one active assignment at a time — callers must not emit `AssignWinner` while
/// already `assigned` without an intervening path back to `claiming` (e.g. `blocked` → `timeout_requeue`).
#[must_use]
pub const fn work_item_may_accept_new_assignment_v1(state: WorkItemLifecycleState) -> bool {
    matches!(state, WorkItemLifecycleState::Claiming)
}

// --- ADR-003 P0 minimal assign winner (ordering only) ---

/// Eligibility cutoff for `capability_score`: claims **must satisfy** strictly greater (`>`), not ≥.
///
/// Equivalent to ADR-003 “no claim clears the eligibility threshold” when all normalized scores are `0`.
pub const P0_ASSIGN_CAPABILITY_EXCLUSIVE_FLOOR: f32 = 0.0;

#[must_use]
pub fn claim_capability_eligible_for_p0_assign_v1(capability_score: f32) -> bool {
    capability_score > P0_ASSIGN_CAPABILITY_EXCLUSIVE_FLOOR
}

/// Winner among **eligible** claims for one work-item assign round (ADR-003 “Minimal Assign Algorithm”).
///
/// Each row is `(claim_vector_index, capability_score, current_workload)` where `claim_vector_index`
/// is a stable ordinal for tie-breaking (typically index in the task’s persisted claim vector:
/// lower = earlier submitted in that storage order).
///
/// Deterministic ordering: highest `capability_score` → lowest `current_workload` → lowest
/// `claim_vector_index`.
///
/// Returns the winning **`claim_vector_index`**, or `None` when `rows` is empty.
#[must_use]
pub fn select_assign_winner_claim_index_v1(rows: &[(usize, f32, u32)]) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&i, &j| {
        let (ixi, si, wi) = rows[i];
        let (ixj, sj, wj) = rows[j];
        sj.total_cmp(&si)
            .then_with(|| wi.cmp(&wj))
            .then_with(|| ixi.cmp(&ixj))
    });
    Some(rows[order[0]].0)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkItemBlockedMeta {
    pub blocked_reason: String,
    pub blocked_at: String,
}

// --- ADR-005 minimal artifacts ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmArtifactKind {
    PlanNote,
    ExecutionResult,
    ReviewNote,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwarmArtifactHeader {
    pub id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub artifact_kind: SwarmArtifactKind,
    pub schema_version: String,
    pub created_at: String,
    pub producer: String,
}

// --- ADR-003 intent hash v1 ---

/// Normalize per ADR-003 `intent_hash_v1`.
#[must_use]
pub fn normalize_intent_text_v1(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let trimmed = text.trim();
    let nfc: String = trimmed.nfc().collect();
    let lower: String = nfc
        .chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect();
    let mut out = String::with_capacity(lower.len());
    let mut prev_space = false;
    for ch in lower.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        prev_space = false;
        out.push(ch);
    }
    let out = out.trim_matches(|c: char| c.is_whitespace()).to_owned();
    trim_edge_punctuation_noise(&out)
}

fn trim_edge_punctuation_noise(s: &str) -> String {
    let mut start = None::<usize>;
    let mut end = None::<usize>;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() {
            start = Some(i);
            break;
        }
    }
    for (i, c) in s.char_indices().rev() {
        if c.is_alphanumeric() {
            end = Some(i + c.len_utf8());
            break;
        }
    }
    match (start, end) {
        (Some(a), Some(b)) if a <= b => s.get(a..b).unwrap_or("").trim().to_owned(),
        _ => String::new(),
    }
}

/// Stable 64-bit fingerprint (FNV-1a) over UTF-8 of normalized text; hex-encoded.
/// Not cryptographic; only for idempotency keys.
#[must_use]
pub fn intent_fingerprint_v1_hex(text: &str) -> String {
    let normalized = normalize_intent_text_v1(text);
    let hash = fnv1a64(normalized.as_bytes());
    format!("{hash:016x}")
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut h = OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

// --- ADR-003 implicit work idempotency ---

pub const IMPLICIT_INTENT_RECORD_SCHEMA_V1: &str = "implicit_intent_record_v1";

/// Deduplication key for implicit work-item triggers (ADR-003).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ImplicitIntentDedupeKey {
    pub conversation_id: String,
    pub triggering_message_id: String,
    pub normalized_intent_hash_hex: String,
    pub intent_hash_version: String,
}

impl ImplicitIntentDedupeKey {
    /// P0: `conversation_id` is usually the runtime `session_id` until a dedicated conversation id exists.
    #[must_use]
    pub fn from_trigger(
        conversation_id: impl Into<String>,
        triggering_message_id: impl Into<String>,
        message_body: &str,
    ) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            triggering_message_id: triggering_message_id.into(),
            normalized_intent_hash_hex: intent_fingerprint_v1_hex(message_body),
            intent_hash_version: INTENT_HASH_VERSION_V1.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImplicitIntentDedupeRecord {
    pub schema: String,
    pub recorded_at_ms: u64,
    pub conversation_id: String,
    pub triggering_message_id: String,
    pub normalized_intent_hash_hex: String,
    pub intent_hash_version: String,
}

impl ImplicitIntentDedupeRecord {
    #[must_use]
    pub fn from_key(key: &ImplicitIntentDedupeKey, recorded_at_ms: u64) -> Self {
        Self {
            schema: IMPLICIT_INTENT_RECORD_SCHEMA_V1.to_owned(),
            recorded_at_ms,
            conversation_id: key.conversation_id.clone(),
            triggering_message_id: key.triggering_message_id.clone(),
            normalized_intent_hash_hex: key.normalized_intent_hash_hex.clone(),
            intent_hash_version: key.intent_hash_version.clone(),
        }
    }

    #[must_use]
    pub fn dedupe_key(&self) -> ImplicitIntentDedupeKey {
        ImplicitIntentDedupeKey {
            conversation_id: self.conversation_id.clone(),
            triggering_message_id: self.triggering_message_id.clone(),
            normalized_intent_hash_hex: self.normalized_intent_hash_hex.clone(),
            intent_hash_version: self.intent_hash_version.clone(),
        }
    }
}

// --- ADR-005 task-room artifacts (`artifact_schema_v1`) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKindV1 {
    PlanNote,
    ExecutionResult,
    ReviewNote,
}

impl ArtifactKindV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlanNote => "plan_note",
            Self::ExecutionResult => "execution_result",
            Self::ReviewNote => "review_note",
        }
    }
}

/// Shared header fields for **`artifact_schema_v1`** (ADR-005).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactHeaderV1 {
    pub id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    pub artifact_kind: ArtifactKindV1,
    pub schema_version: String,
    pub created_at_ms: u64,
    /// Producer identity, e.g. `agent:<instance_id>` or `http_chat:agent:<instance_id>`.
    pub producer: String,
}

/// P0 **`execution_result`** payload + header (single JSON document in task room).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionResultArtifactV1 {
    #[serde(flatten)]
    pub header: ArtifactHeaderV1,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// P0 **`plan_note`** payload + header (single JSON document in task room).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanNoteArtifactV1 {
    #[serde(flatten)]
    pub header: ArtifactHeaderV1,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactValidationError {
    WrongSchemaVersion,
    WrongArtifactKind,
    MissingWorkItemId,
}

impl ExecutionResultArtifactV1 {
    pub fn validate(&self) -> Result<(), ArtifactValidationError> {
        if self.header.schema_version != ARTIFACT_SCHEMA_V1 {
            return Err(ArtifactValidationError::WrongSchemaVersion);
        }
        if !matches!(self.header.artifact_kind, ArtifactKindV1::ExecutionResult) {
            return Err(ArtifactValidationError::WrongArtifactKind);
        }
        if self
            .header
            .work_item_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            return Err(ArtifactValidationError::MissingWorkItemId);
        }
        Ok(())
    }
}

impl PlanNoteArtifactV1 {
    pub fn validate(&self) -> Result<(), ArtifactValidationError> {
        if self.header.schema_version != ARTIFACT_SCHEMA_V1 {
            return Err(ArtifactValidationError::WrongSchemaVersion);
        }
        if !matches!(self.header.artifact_kind, ArtifactKindV1::PlanNote) {
            return Err(ArtifactValidationError::WrongArtifactKind);
        }
        Ok(())
    }
}

/// P0 **`review_note`** payload + header (single JSON document in task room).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewNoteArtifactV1 {
    #[serde(flatten)]
    pub header: ArtifactHeaderV1,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl ReviewNoteArtifactV1 {
    pub fn validate(&self) -> Result<(), ArtifactValidationError> {
        if self.header.schema_version != ARTIFACT_SCHEMA_V1 {
            return Err(ArtifactValidationError::WrongSchemaVersion);
        }
        if !matches!(self.header.artifact_kind, ArtifactKindV1::ReviewNote) {
            return Err(ArtifactValidationError::WrongArtifactKind);
        }
        if self
            .header
            .work_item_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            return Err(ArtifactValidationError::MissingWorkItemId);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_intent_collapses_space_and_lower_ascii() {
        assert_eq!(normalize_intent_text_v1("  Hello   WORLD  "), "hello world");
    }

    #[test]
    fn intent_fingerprint_stable() {
        let a = intent_fingerprint_v1_hex("  Plan   THIS  ");
        let b = intent_fingerprint_v1_hex("plan this");
        assert_eq!(a, b);
    }

    #[test]
    fn intent_trigger_key_stable_for_same_normalized_body() {
        let k1 = ImplicitIntentDedupeKey::from_trigger("sess", "mid1", "  PLAN  ThIs  ");
        let k2 = ImplicitIntentDedupeKey::from_trigger("sess", "mid2", "plan this");
        assert_ne!(k1.triggering_message_id, k2.triggering_message_id);
        assert_eq!(k1.normalized_intent_hash_hex, k2.normalized_intent_hash_hex);

        let rec = ImplicitIntentDedupeRecord::from_key(&k1, 1);
        assert_eq!(rec.dedupe_key(), k1);
    }

    /// Same ADR-003 quadruple (incl. normalization) ⇒ one logical implicit-WI trigger identity.
    #[test]
    fn implicit_intent_duplicate_replay_same_message_id_collapses_dedupe_key() {
        let k1 = ImplicitIntentDedupeKey::from_trigger("conv", "mid", "  Do  THE  thing  ");
        let k2 = ImplicitIntentDedupeKey::from_trigger("conv", "mid", "do the thing");
        assert_eq!(k1, k2);
    }

    #[test]
    fn blocked_exit_p0_targets_match_adr003() {
        assert_eq!(
            blocked_exit_next_state_v1(BlockedExitKind::TimeoutRequeue),
            WorkItemLifecycleState::Claiming
        );
        assert_eq!(
            blocked_exit_next_state_v1(BlockedExitKind::ManualCancel),
            WorkItemLifecycleState::Cancelled
        );
        assert_eq!(
            blocked_exit_next_state_v1(BlockedExitKind::TimeoutAbandon),
            WorkItemLifecycleState::Cancelled
        );
        assert_eq!(
            blocked_exit_next_state_v1(BlockedExitKind::UserResume),
            WorkItemLifecycleState::Claiming
        );
        assert_eq!(
            blocked_exit_next_state_v1(BlockedExitKind::PlannerReplan),
            WorkItemLifecycleState::Claiming
        );
        assert_eq!(BlockedExitKind::ManualCancel.as_str(), "manual_cancel");
        assert_eq!(BlockedExitKind::TimeoutAbandon.as_str(), "timeout_abandon");
    }

    #[test]
    fn p0_happy_path_planned_to_done() {
        use WorkItemLifecycleCommandV1::*;

        let mut s = WorkItemLifecycleState::Planned;
        assert!(!work_item_may_accept_new_assignment_v1(s));
        assert_eq!(
            apply_work_item_lifecycle_command_v1(s, SubmitForClaiming).unwrap(),
            WorkItemLifecycleState::Claiming
        );
        s = WorkItemLifecycleState::Claiming;
        assert!(work_item_may_accept_new_assignment_v1(s));
        s = apply_work_item_lifecycle_command_v1(s, AssignWinner).unwrap();
        assert_eq!(s, WorkItemLifecycleState::Assigned);
        assert!(!work_item_may_accept_new_assignment_v1(s));
        assert!(apply_work_item_lifecycle_command_v1(s, AssignWinner).is_err());
        s = apply_work_item_lifecycle_command_v1(s, MarkDone).unwrap();
        assert_eq!(s, WorkItemLifecycleState::Done);
        assert!(apply_work_item_lifecycle_command_v1(s, MarkDone).is_err());
        assert_eq!(
            apply_work_item_lifecycle_command_v1(s, Cancel).unwrap_err(),
            WorkItemLifecycleTransitionError::AlreadyTerminal
        );
    }

    #[test]
    fn p0_blocked_timeout_requeue_returns_to_claiming_then_can_assign_again() {
        use WorkItemLifecycleCommandV1::*;
        let mut s = WorkItemLifecycleState::Assigned;
        s = apply_work_item_lifecycle_command_v1(s, EnterBlocked).unwrap();
        assert_eq!(s, WorkItemLifecycleState::Blocked);
        assert!(!work_item_may_accept_new_assignment_v1(s));

        s = apply_work_item_lifecycle_command_v1(s, ExitBlocked(BlockedExitKind::TimeoutRequeue))
            .unwrap();
        assert_eq!(s, WorkItemLifecycleState::Claiming);
        assert!(work_item_may_accept_new_assignment_v1(s));

        s = apply_work_item_lifecycle_command_v1(s, AssignWinner).unwrap();
        assert_eq!(s, WorkItemLifecycleState::Assigned);
    }

    #[test]
    fn p0_blocked_manual_cancel_to_cancelled() {
        use WorkItemLifecycleCommandV1::*;
        let s = apply_work_item_lifecycle_command_v1(
            WorkItemLifecycleState::Blocked,
            ExitBlocked(BlockedExitKind::ManualCancel),
        )
        .unwrap();
        assert_eq!(s, WorkItemLifecycleState::Cancelled);
        assert!(s.is_terminal());
    }

    #[test]
    fn p0_blocked_timeout_abandon_targets_cancelled() {
        use WorkItemLifecycleCommandV1::*;
        let s = apply_work_item_lifecycle_command_v1(
            WorkItemLifecycleState::Blocked,
            ExitBlocked(BlockedExitKind::TimeoutAbandon),
        )
        .unwrap();
        assert_eq!(s, WorkItemLifecycleState::Cancelled);
    }

    #[test]
    fn p0_assign_prefers_capability_score_then_workload_then_earlier_claim_index() {
        assert_eq!(
            select_assign_winner_claim_index_v1(&[(5, 0.92, 1)]),
            Some(5)
        );
        assert_eq!(
            select_assign_winner_claim_index_v1(&[(0, 0.50, 0), (1, 0.90, 0)]),
            Some(1)
        );
        assert_eq!(
            select_assign_winner_claim_index_v1(&[(0, 0.81, 2), (1, 0.81, 0)]),
            Some(1)
        );
        assert_eq!(
            select_assign_winner_claim_index_v1(&[(10, 0.81, 1), (20, 0.81, 1)]),
            Some(10)
        );
        assert!(select_assign_winner_claim_index_v1(&[]).is_none());
    }

    #[test]
    fn p0_assign_eligibility_requires_score_above_zero() {
        assert!(claim_capability_eligible_for_p0_assign_v1(0.001));
        assert!(!claim_capability_eligible_for_p0_assign_v1(0.0));
        assert!(!claim_capability_eligible_for_p0_assign_v1(-1.0));
    }

    #[test]
    fn planned_cannot_assign_without_claiming_phase() {
        use WorkItemLifecycleCommandV1::*;
        assert_eq!(
            apply_work_item_lifecycle_command_v1(WorkItemLifecycleState::Planned, AssignWinner)
                .unwrap_err(),
            WorkItemLifecycleTransitionError::InvalidCommandForState
        );
    }

    #[test]
    fn execution_result_artifact_v1_serializes_flat_header_fields() {
        let artifact = ExecutionResultArtifactV1 {
            header: ArtifactHeaderV1 {
                id: "e1".into(),
                task_id: "t1".into(),
                work_item_id: Some("wi1".into()),
                artifact_kind: ArtifactKindV1::ExecutionResult,
                schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
                created_at_ms: 42,
                producer: "producer".into(),
            },
            summary: "done".into(),
            details: None,
        };
        artifact.validate().expect("fixture should validate");
        let value = serde_json::to_value(&artifact).expect("serde");
        assert_eq!(value["artifact_kind"], "execution_result");
        assert_eq!(value["schema_version"], ARTIFACT_SCHEMA_V1);
        assert_eq!(value["summary"], "done");
        let back: ExecutionResultArtifactV1 = serde_json::from_value(value).expect("roundtrip");
        assert_eq!(back, artifact);
    }

    #[test]
    fn execution_result_artifact_v1_validate_requires_work_item_id() {
        let artifact = ExecutionResultArtifactV1 {
            header: ArtifactHeaderV1 {
                id: "e1".into(),
                task_id: "t1".into(),
                work_item_id: None,
                artifact_kind: ArtifactKindV1::ExecutionResult,
                schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
                created_at_ms: 42,
                producer: "producer".into(),
            },
            summary: "s".into(),
            details: None,
        };
        assert_eq!(
            artifact.validate(),
            Err(ArtifactValidationError::MissingWorkItemId)
        );
    }

    #[test]
    fn review_note_artifact_v1_roundtrips() {
        let artifact = ReviewNoteArtifactV1 {
            header: ArtifactHeaderV1 {
                id: "r1".into(),
                task_id: "t1".into(),
                work_item_id: Some("wi1".into()),
                artifact_kind: ArtifactKindV1::ReviewNote,
                schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
                created_at_ms: 7,
                producer: "reviewer".into(),
            },
            summary: "lgtm with nits".into(),
            verdict: Some("approve".into()),
            details: None,
        };
        artifact.validate().unwrap();
        let v = serde_json::to_value(&artifact).unwrap();
        let back: ReviewNoteArtifactV1 = serde_json::from_value(v).unwrap();
        assert_eq!(back, artifact);
    }

    #[test]
    fn plan_note_artifact_v1_roundtrips_without_work_item_id() {
        let artifact = PlanNoteArtifactV1 {
            header: ArtifactHeaderV1 {
                id: "p1".into(),
                task_id: "t1".into(),
                work_item_id: None,
                artifact_kind: ArtifactKindV1::PlanNote,
                schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
                created_at_ms: 9,
                producer: "planner".into(),
            },
            summary: "plan updated".into(),
            details: Some("split into two stages".into()),
        };
        artifact.validate().unwrap();
        let v = serde_json::to_value(&artifact).unwrap();
        let back: PlanNoteArtifactV1 = serde_json::from_value(v).unwrap();
        assert_eq!(back, artifact);
    }
}
