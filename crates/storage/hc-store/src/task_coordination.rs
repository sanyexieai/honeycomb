//! Relative paths for task-bound coordination artifacts (ADR-003 / rollout Phase 1).
//!
//! Resolved under [`WorkspaceNamespace::scoped_prefix`](crate::store::WorkspaceNamespace::scoped_prefix).
//!
//! - **Markdown**: [`task_plan_markdown_relative`], assignments, optional per–work-item snapshots live in
//!   **`coordination/{task_slug}/`** (one subdirectory per logical task).
//! - **Append-only JSONL** (routing / implicit-intent): **`coordination/{task_slug}.{kind}.jsonl`** at the
//!   **`coordination/`** root next to task subdirectories — preserves existing tooling layout.
//! - **Per-task JSONL** under **`coordination/{task_slug}/`** (e.g. work item journals, [`materialization_notices_journal_relative`]).

use std::path::PathBuf;

/// Slug for path segments (`task_id`, `assignment_id`, …); matches historical `hc-agent` persistence.
#[must_use]
pub fn coordination_segment_slug(raw: &str) -> String {
    let mut slug = String::new();
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('.') && !slug.ends_with('-') {
            slug.push('.');
        }
    }
    slug.trim_matches(&['.', '-'][..]).to_owned()
}

/// `coordination/{task_slug}/`
#[must_use]
pub fn coordination_task_markdown_root(task_id: &str) -> PathBuf {
    PathBuf::from("coordination").join(coordination_segment_slug(task_id))
}

#[must_use]
pub fn task_plan_markdown_relative(task_id: &str) -> PathBuf {
    coordination_task_markdown_root(task_id).join("task_plan.md")
}

#[must_use]
pub fn assignment_decision_markdown_relative(task_id: &str, assignment_id: &str) -> PathBuf {
    coordination_task_markdown_root(task_id).join(format!(
        "assignment_decision.{}.md",
        coordination_segment_slug(assignment_id)
    ))
}

/// `coordination/{task_slug}.routing.jsonl`
#[must_use]
pub fn routing_binding_journal_relative(task_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "coordination/{}.routing.jsonl",
        coordination_segment_slug(task_id)
    ))
}

/// `coordination/{task_slug}.implicit-intent.jsonl`
#[must_use]
pub fn implicit_intent_journal_relative(task_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "coordination/{}.implicit-intent.jsonl",
        coordination_segment_slug(task_id)
    ))
}

/// `coordination/{task_slug}/materialization_notices.jsonl`
#[must_use]
pub fn materialization_notices_journal_relative(task_id: &str) -> PathBuf {
    coordination_task_markdown_root(task_id).join("materialization_notices.jsonl")
}

#[must_use]
pub fn work_item_claims_journal_relative(task_id: &str) -> PathBuf {
    coordination_task_markdown_root(task_id).join("work_item_claims.jsonl")
}

#[must_use]
pub fn work_item_assignments_journal_relative(task_id: &str) -> PathBuf {
    coordination_task_markdown_root(task_id).join("work_item_assignments.jsonl")
}

#[must_use]
pub fn work_item_markdown_relative(task_id: &str, work_item_id: &str) -> PathBuf {
    coordination_task_markdown_root(task_id)
        .join("work_items")
        .join(format!("{}.md", coordination_segment_slug(work_item_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_paths_use_per_task_subdirectory() {
        assert_eq!(
            task_plan_markdown_relative("task.demo"),
            PathBuf::from("coordination/task.demo/task_plan.md")
        );
        assert_eq!(
            assignment_decision_markdown_relative("task.demo", "assign-001"),
            PathBuf::from("coordination/task.demo/assignment_decision.assign.001.md")
        );
        assert_eq!(
            work_item_markdown_relative("task.demo", "wi.001"),
            PathBuf::from("coordination/task.demo/work_items/wi.001.md")
        );
    }

    #[test]
    fn jsonl_journals_remain_flat_under_coordination() {
        assert_eq!(
            routing_binding_journal_relative("task.demo"),
            PathBuf::from("coordination/task.demo.routing.jsonl")
        );
        assert_eq!(
            implicit_intent_journal_relative("Room.Task.Alpha"),
            PathBuf::from("coordination/room.task.alpha.implicit-intent.jsonl")
        );
    }

    #[test]
    fn jsonl_splits_for_claims_under_task_subdirectory() {
        assert_eq!(
            work_item_claims_journal_relative("t1"),
            PathBuf::from("coordination/t1/work_item_claims.jsonl")
        );
        assert_eq!(
            work_item_assignments_journal_relative("t1"),
            PathBuf::from("coordination/t1/work_item_assignments.jsonl")
        );
        assert_eq!(
            materialization_notices_journal_relative("t1"),
            PathBuf::from("coordination/t1/materialization_notices.jsonl")
        );
    }
}
