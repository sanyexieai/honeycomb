//! Extensible intent detection for Honeycomb.
//!
//! Design: small core (`IntentRouter` + merge) and pluggable [`IntentDetector`] implementations.
//! New product intents add a detector (or extend config) without changing the merger.

use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Stable intent ids (stringly-typed for forward compatibility).
pub mod ids {
    pub const CHAT_GENERAL: &str = "chat.general";
    pub const INTERACTION_TIMED_EMIT: &str = "interaction.timed_emit";
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IntentInput<'a> {
    pub user_text: &'a str,
}

#[derive(Debug, Clone)]
pub struct IntentCandidate {
    pub intent_id: String,
    pub score: f32,
    pub detector_id: &'static str,
    pub signals: Value,
}

#[derive(Debug, Clone)]
pub struct IntentResolution {
    pub primary_intent: String,
    pub confidence: f32,
    pub merged_by_intent: BTreeMap<String, IntentCandidate>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct IntentMergerConfig {
    pub threshold: f32,
    pub fallback_intent: String,
}

impl Default for IntentMergerConfig {
    fn default() -> Self {
        Self {
            threshold: 0.45,
            fallback_intent: ids::CHAT_GENERAL.to_owned(),
        }
    }
}

pub trait IntentDetector: Send + Sync {
    fn detector_id(&self) -> &'static str;
    fn detect(&self, input: &IntentInput<'_>) -> Vec<IntentCandidate>;
}

pub struct IntentRouter {
    detectors: Vec<Box<dyn IntentDetector>>,
    merger: IntentMergerConfig,
}

impl IntentRouter {
    pub fn new(merger: IntentMergerConfig) -> Self {
        Self {
            detectors: Vec::new(),
            merger,
        }
    }

    pub fn with_builtin_defaults() -> Self {
        let mut router = Self::new(IntentMergerConfig::default());
        router.register(Box::new(HeuristicTimedEmitDetector));
        router.register(Box::new(ChatFallbackDetector));
        router
    }

    pub fn register(&mut self, detector: Box<dyn IntentDetector>) {
        self.detectors.push(detector);
    }

    pub fn resolve(&self, input: &IntentInput<'_>) -> IntentResolution {
        let mut all = Vec::new();
        for detector in &self.detectors {
            all.extend(detector.detect(input));
        }

        let mut merged: BTreeMap<String, IntentCandidate> = BTreeMap::new();
        for candidate in all {
            merged
                .entry(candidate.intent_id.clone())
                .and_modify(|existing| {
                    if candidate.score > existing.score {
                        *existing = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }

        let mut ranked = merged.values().cloned().collect::<Vec<_>>();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let (primary, confidence, reason) = match ranked.first() {
            Some(top) if top.score >= self.merger.threshold => (
                top.intent_id.clone(),
                top.score,
                format!(
                    "selected {} via detector {}",
                    top.intent_id, top.detector_id
                ),
            ),
            Some(top) => (
                self.merger.fallback_intent.clone(),
                top.score,
                format!(
                    "top intent {} score {:.2} below threshold {:.2}; fallback to {}",
                    top.intent_id, top.score, self.merger.threshold, self.merger.fallback_intent
                ),
            ),
            None => (
                self.merger.fallback_intent.clone(),
                0.0,
                "no candidates; fallback".to_owned(),
            ),
        };

        IntentResolution {
            primary_intent: primary,
            confidence,
            merged_by_intent: merged,
            reason,
        }
    }
}

/// Heuristic detector for countdown / per-second emission (no ML).
pub struct HeuristicTimedEmitDetector;

impl IntentDetector for HeuristicTimedEmitDetector {
    fn detector_id(&self) -> &'static str {
        "heuristic.timed_emit"
    }

    fn detect(&self, input: &IntentInput<'_>) -> Vec<IntentCandidate> {
        let t = input.user_text;
        let mut score = 0.0f32;
        let mut signals = serde_json::Map::new();

        const COUNTDOWN: &[&str] = &["倒数", "倒计时", "countdown", "count down"];
        const PER_TICK: &[&str] = &[
            "每秒",
            "每隔一秒",
            "一秒一个",
            "one per second",
            "every second",
            "per second",
        ];
        const COUNT_CTX: &[&str] = &["个数", "个数", "数到", "数一个", "报数"];

        if COUNTDOWN.iter().any(|k| t.contains(k)) {
            score += 0.45;
            signals.insert("hint".into(), json!("countdown"));
        }
        if PER_TICK.iter().any(|k| t.contains(k)) {
            score += 0.4;
            signals.insert("hint".into(), json!("per_tick"));
        }
        if COUNT_CTX.iter().any(|k| t.contains(k)) {
            score += 0.15;
        }
        if contains_ascii_digit(t) || contains_chinese_numeral_token(t) {
            score += 0.15;
        }

        score = score.min(0.95);

        if score < 0.35 {
            return Vec::new();
        }

        signals.insert("family".into(), json!("timed_emit"));
        vec![IntentCandidate {
            intent_id: ids::INTERACTION_TIMED_EMIT.to_owned(),
            score,
            detector_id: self.detector_id(),
            signals: Value::Object(signals),
        }]
    }
}

fn contains_ascii_digit(text: &str) -> bool {
    text.chars().any(|ch| ch.is_ascii_digit())
}

fn contains_chinese_numeral_token(text: &str) -> bool {
    const TOKENS: &[&str] = &[
        "十", "百", "零", "一", "二", "三", "四", "五", "六", "七", "八", "九",
    ];
    TOKENS.iter().any(|tok| text.contains(tok))
}

/// Low-score fallback so merge always has a chat path.
pub struct ChatFallbackDetector;

impl IntentDetector for ChatFallbackDetector {
    fn detector_id(&self) -> &'static str {
        "fallback.chat_general"
    }

    fn detect(&self, _input: &IntentInput<'_>) -> Vec<IntentCandidate> {
        vec![IntentCandidate {
            intent_id: ids::CHAT_GENERAL.to_owned(),
            score: 0.01,
            detector_id: self.detector_id(),
            signals: json!({}),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_emit_beats_fallback() {
        let router = IntentRouter::with_builtin_defaults();
        let r = router.resolve(&IntentInput {
            user_text: "倒数10个数",
        });
        assert_eq!(r.primary_intent, ids::INTERACTION_TIMED_EMIT);
        assert!(r.confidence >= 0.45);
    }

    #[test]
    fn general_chat_falls_back() {
        let router = IntentRouter::with_builtin_defaults();
        let r = router.resolve(&IntentInput {
            user_text: "今天天气怎么样",
        });
        assert_eq!(r.primary_intent, ids::CHAT_GENERAL);
    }
}
