//! Unified vocabulary for timed reminders, countdown sequences, and scheduler-backed follow-ups.
//!
//! Disk state lives in `hc-conversation` (`FollowUpStatus`) and `hc-scheduler` (`ScheduledRunStatus`).
//! This module adds a **single-lane** lifecycle enum for observability and alignment with
//! `docs/todo/timed-and-scheduler-unification.md`.

use serde::{Deserialize, Serialize};

/// One logical execution instance for timed / scheduled follow-up work.
///
/// Maps conceptually to:
/// - Scheduler: [`Queued`][Self::Queued] / [`Running`][Self::Running] ↔ `ScheduledRunStatus::{Queued,Running}`;
///   terminal success ↔ `Succeeded`; terminal failure ↔ `Failed`.
/// - Conversation follow-up: [`Queued`][Self::Queued] ↔ `Pending` before fire;
///   [`Fired`][Self::Fired] ↔ `Fired` after delivery; `Failed` / `Cancelled` ↔ same names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimedRunLifecycle {
    Queued,
    Running,
    /// Timed follow-up delivered to the user/session (conversation `Fired`).
    Fired,
    /// Terminal success without a timed “fire” semantics (e.g. cancelled follow-up, skipped run).
    Done,
    Failed,
}

/// Stable idempotency token for “one logical fire” of a timed run.
///
/// Inputs are **logical** (not the follow-up file id): the same triple must yield the same key
/// across processes and Rust toolchain versions — use this to detect duplicate enqueue on retries.
///
/// Format: `timed.idem.v1.` + 16 hex chars (FNV-1a 64 over canonical bytes).
pub fn timed_run_idempotency_key_v1(
    logical_task_id: &str,
    fire_at_unix: u64,
    sequence_index: u32,
) -> String {
    format!(
        "timed.idem.v1.{:016x}",
        fnv1a64_idem_payload(logical_task_id, fire_at_unix, sequence_index)
    )
}

fn fnv1a64_idem_payload(logical_task_id: &str, fire_at_unix: u64, sequence_index: u32) -> u64 {
    const OFFSET_BASIS: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;

    #[inline]
    fn mix(mut h: u64, b: u8) -> u64 {
        h ^= b as u64;
        h.wrapping_mul(PRIME)
    }

    let mut hash = OFFSET_BASIS;
    let ver = b"timed_idem_v1\0";
    for &b in ver {
        hash = mix(hash, b);
    }
    for &b in logical_task_id.as_bytes() {
        hash = mix(hash, b);
    }
    hash = mix(hash, 0);
    for b in fire_at_unix.to_be_bytes() {
        hash = mix(hash, b);
    }
    for b in sequence_index.to_be_bytes() {
        hash = mix(hash, b);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{TimedRunLifecycle, timed_run_idempotency_key_v1};

    #[test]
    fn idempotency_key_is_stable_deterministic() {
        assert_eq!(
            timed_run_idempotency_key_v1("reminder.builtin", 1_700_000_000, 0),
            timed_run_idempotency_key_v1("reminder.builtin", 1_700_000_000, 0)
        );
        assert_ne!(
            timed_run_idempotency_key_v1("reminder.builtin", 1_700_000_000, 0),
            timed_run_idempotency_key_v1("reminder.builtin", 1_700_000_001, 0)
        );
        assert_ne!(
            timed_run_idempotency_key_v1("reminder.builtin", 1_700_000_000, 0),
            timed_run_idempotency_key_v1("reminder.builtin", 1_700_000_000, 1)
        );
        assert!(
            timed_run_idempotency_key_v1("a", 2, 3).starts_with("timed.idem.v1."),
            "expected versioned prefix"
        );
    }

    #[test]
    fn lifecycle_serde_snake_case() {
        let v = serde_json::to_string(&TimedRunLifecycle::Fired).unwrap();
        assert_eq!(v, r#""fired""#);
    }
}
