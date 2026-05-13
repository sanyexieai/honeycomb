//! 交付层（HTTP/CLI 等）跨领域类型的统一再导出。
//!
//! 约定：应用入口 crate 应优先通过本模块访问下列符号，而不是直接依赖多个
//! `hc-*` 领域 crate，以降低交付层依赖扇出（见仓库根目录 `docs/architecture.md`）。

pub use hc_behavior::{
    BehaviorConfig, BehaviorContext, BehaviorEngine, BehaviorPattern, DecisionOption,
    DecisionRecord, DecisionType,
};
pub use hc_bootstrap::{
    default_tenant_id, default_user_id, init_console_tracing, load_local_env_file,
    tenant_id_from_env, unix_timestamp_secs, user_id_from_env, wall_clock_ms, workspace_root,
};
pub use hc_conversation::{
    AgentTurnProposal, ConversationEvent, ConversationRepository, FollowUpStatus, now_unix,
};
pub use hc_memory::{
    CapabilityRef, InheritanceType, MemoryLayer, MemoryNamespace, MemoryRoom, MemoryRoomRepository,
    ResolvedRoomCapabilities, RoomCapabilityResolver, RoomConfig, ScheduleRef, SkillRef, ToolRef,
};
pub use hc_responder::HumanInboxItem;
pub use hc_scheduler::{
    ScheduleKind, ScheduleRepository, ScheduleSpec, ScheduleStatus, ScheduledRun, ScheduledTarget,
    ScheduledTargetKind, ScheduledTask,
};
pub use hc_store::store::WorkspaceNamespace;

/// Markdown / 向量索引相关类型（CLI `index` 子命令等）。
pub mod workspace_markdown_index {
    pub use hc_store::index::{
        DEFAULT_LOCAL_EMBEDDING_DIMS, IndexHit, LocalJsonVectorIndex, RebuildableIndex, VectorIndex,
        VectorQuery, indexed_documents_from_markdown_index, local_hash_embedding,
        vector_documents_from_indexed_documents,
    };
    pub use hc_store::store::{MarkdownIndex, MarkdownQuery, WorkspaceNamespace, WorkspaceStore};
}
