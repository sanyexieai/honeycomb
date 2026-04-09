//! Core runtime model for Honeycomb.

pub mod runtime;
pub mod worker;

pub use runtime::{
    ChannelRecord, ChildInstanceDisposition, EventKind, EventRecord, InstanceRecord, JobKind,
    JobRecord, JobState, MessageKind, MessageRecord, MessageRoute, RunMode, RunRequest,
    RuntimeCommand, RuntimeCommandResult, RuntimeError, RuntimeState, RuntimeSupervisor, SessionRecord,
    WorkerKind, WorkerPlan, WorkerRecord, WorkerReport, classify_run_request,
};
pub use worker::{ProcessWorker, ProcessWorkerHandle, WorkerExecutionError};
