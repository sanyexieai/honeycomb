//! Configurable swarm routing phrases (ADR-001: not hard-coded only in orchestration).
//!
//! Default file: **`{workspace_root}/swarm_routing_phrases.json`** — override absolute path via
//! **`HC_SWARM_ROUTING_PHRASES_FILE`**.
//!
//! If the file is missing or invalid JSON, **builtin** phrases are used (matching historical P0 defaults).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SwarmRoutingPhrasesJson {
    #[serde(default = "extends_builtin_true")]
    extends_builtin: bool,
    #[serde(default)]
    force_l1: Vec<String>,
    #[serde(default)]
    force_l3: Vec<String>,
    #[serde(default)]
    l3_collaboration: Vec<String>,
    #[serde(default)]
    l2_implicit_keywords: Vec<String>,
}

const fn extends_builtin_true() -> bool {
    true
}

/// Phrase lists consulted by [`crate::swarm_routing::decide_routing_tier`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmRoutingPhraseTable {
    pub force_l1: Vec<String>,
    pub force_l3: Vec<String>,
    pub l3_collaboration: Vec<String>,
    pub l2_implicit_keywords: Vec<String>,
}

impl SwarmRoutingPhraseTable {
    #[must_use]
    pub fn builtins() -> Self {
        Self {
            force_l1: str_slice_to_owned(&[
                "do not split",
                "don't split",
                "just answer directly",
                "just answer",
                "别拆任务",
                "不要拆",
                "直接回答",
            ]),
            force_l3: str_slice_to_owned(&[
                "plan this",
                "plan with steps",
                "break this down",
                "handle as a task",
                "turn this into a task",
                "分步骤",
                "一起协作",
                "开个任务",
            ]),
            l3_collaboration: str_slice_to_owned(&["collaborate", "协作", "parallel work items"]),
            l2_implicit_keywords: str_slice_to_owned(&[
                "refactor",
                "migrate",
                "patch",
                "pull request",
                "pr ",
                "open a pr",
                "write tests",
                "add tests",
                "集成",
                "重构",
                "实现功能",
            ]),
        }
    }

    #[must_use]
    pub fn load_from_workspace(workspace_root: &Path) -> Self {
        let path = phrase_file_path(
            workspace_root,
            std::env::var("HC_SWARM_ROUTING_PHRASES_FILE").ok(),
        );

        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<SwarmRoutingPhrasesJson>(&raw) {
                Ok(overlay) => Self::merge_builtins_optional(overlay),
                Err(_) => Self::builtins(),
            },
            Err(_) => Self::builtins(),
        }
    }

    fn merge_builtins_optional(overlay: SwarmRoutingPhrasesJson) -> Self {
        let sanitized = sanitize_overlay(overlay);
        let base = Self::builtins();
        if !sanitized.extends_builtin {
            return SwarmRoutingPhraseTable {
                force_l1: sanitized.force_l1,
                force_l3: sanitized.force_l3,
                l3_collaboration: sanitized.l3_collaboration,
                l2_implicit_keywords: sanitized.l2_implicit_keywords,
            };
        }
        SwarmRoutingPhraseTable {
            force_l1: merge_unique_ordered(base.force_l1, sanitized.force_l1),
            force_l3: merge_unique_ordered(base.force_l3, sanitized.force_l3),
            l3_collaboration: merge_unique_ordered(
                base.l3_collaboration,
                sanitized.l3_collaboration,
            ),
            l2_implicit_keywords: merge_unique_ordered(
                base.l2_implicit_keywords,
                sanitized.l2_implicit_keywords,
            ),
        }
    }
}

#[must_use]
fn phrase_file_path(workspace_root: &Path, env_path: Option<String>) -> PathBuf {
    if let Some(p) = env_path {
        let pb = PathBuf::from(p.trim());
        if pb.is_absolute() {
            return pb;
        }
        return workspace_root.join(pb);
    }
    workspace_root.join("swarm_routing_phrases.json")
}

#[must_use]
fn str_slice_to_owned(src: &[&str]) -> Vec<String> {
    src.iter().map(|s| (*s).to_owned()).collect()
}

fn sanitize_overlay(mut o: SwarmRoutingPhrasesJson) -> SwarmRoutingPhrasesJson {
    o.force_l1 = trim_nonempty(o.force_l1);
    o.force_l3 = trim_nonempty(o.force_l3);
    o.l3_collaboration = trim_nonempty(o.l3_collaboration);
    o.l2_implicit_keywords = trim_nonempty(o.l2_implicit_keywords);
    o
}

fn trim_nonempty(v: Vec<String>) -> Vec<String> {
    v.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn merge_unique_ordered(first: Vec<String>, second: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::<String>::with_capacity(first.len() + second.len());
    let mut out = Vec::with_capacity(first.len() + second.len());
    for s in first.into_iter().chain(second) {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

impl Default for SwarmRoutingPhrasesJson {
    fn default() -> Self {
        Self {
            extends_builtin: true,
            force_l1: Vec::new(),
            force_l3: Vec::new(),
            l3_collaboration: Vec::new(),
            l2_implicit_keywords: Vec::new(),
        }
    }
}

static PHRASE_TABLE: OnceLock<SwarmRoutingPhraseTable> = OnceLock::new();

#[must_use]
pub(crate) fn global_phrase_table() -> &'static SwarmRoutingPhraseTable {
    PHRASE_TABLE.get_or_init(|| {
        SwarmRoutingPhraseTable::load_from_workspace(&hc_bootstrap::workspace_root())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_from_workspace_merges_json_file() {
        let dir =
            std::env::temp_dir().join(format!("hc-swarm-phrases-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let json = r#"{
            "extends_builtin": false,
            "force_l3": ["only_magic_l3"],
            "force_l1": [],
            "l3_collaboration": [],
            "l2_implicit_keywords": []
        }"#;
        std::fs::write(dir.join("swarm_routing_phrases.json"), json).expect("write");
        let t = SwarmRoutingPhraseTable::load_from_workspace(&dir);
        assert_eq!(t.force_l3, vec!["only_magic_l3"]);
        assert!(t.force_l1.is_empty());
        assert!(t.l2_implicit_keywords.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_extends_builtin_appends_without_duplicates() {
        let overlay = SwarmRoutingPhrasesJson {
            extends_builtin: true,
            force_l1: vec!["  custom escape  ".to_owned(), "don't split".to_owned()],
            ..Default::default()
        };
        let t = SwarmRoutingPhraseTable::merge_builtins_optional(overlay);
        assert_eq!(
            t.force_l1
                .iter()
                .filter(|s| s.as_str() == "don't split")
                .count(),
            1
        );
        assert!(t.force_l1.iter().any(|s| s == "custom escape"));
        assert!(t.force_l1.iter().any(|s| s == "don't split"));
    }

    #[test]
    fn replace_mode_can_clear_l2_keywords() {
        let overlay = SwarmRoutingPhrasesJson {
            extends_builtin: false,
            force_l1: vec!["only-this".into()],
            ..Default::default()
        };
        let t = SwarmRoutingPhraseTable::merge_builtins_optional(overlay);
        assert_eq!(t.force_l1, vec!["only-this"]);
        assert!(t.force_l3.is_empty());
        assert!(t.l2_implicit_keywords.is_empty());
    }
}
