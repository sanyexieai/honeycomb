//! 上下文感知模块 - 考虑对话历史、任务序列和时间模式

use std::collections::HashMap;
use chrono::{DateTime, Utc, Duration, Timelike, Datelike};
use serde::{Deserialize, Serialize};

use crate::TagVector;

/// 上下文感知分析器
pub struct ContextAwareAnalyzer {
    // 对话历史分析器
    conversation_analyzer: ConversationAnalyzer,
    // 任务序列分析器
    task_sequence_analyzer: TaskSequenceAnalyzer,
    // 时间模式分析器
    temporal_analyzer: TemporalAnalyzer,
    // 配置
    config: ContextConfig,
}

/// 上下文分析配置
#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub max_history_length: usize,
    pub conversation_weight: f32,
    pub sequence_weight: f32,
    pub temporal_weight: f32,
    pub decay_factor: f32, // 历史影响衰减因子
    pub similarity_threshold: f32,
    pub enable_temporal_patterns: bool,
}

pub const DEFAULT_MAX_HISTORY_LENGTH: usize = 10;
pub const DEFAULT_CONVERSATION_WEIGHT: f32 = 0.3;
pub const DEFAULT_SEQUENCE_WEIGHT: f32 = 0.4;
pub const DEFAULT_TEMPORAL_WEIGHT: f32 = 0.3;
pub const DEFAULT_DECAY_FACTOR: f32 = 0.8;
pub const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.6;

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_history_length: DEFAULT_MAX_HISTORY_LENGTH,
            conversation_weight: DEFAULT_CONVERSATION_WEIGHT,
            sequence_weight: DEFAULT_SEQUENCE_WEIGHT,
            temporal_weight: DEFAULT_TEMPORAL_WEIGHT,
            decay_factor: DEFAULT_DECAY_FACTOR,
            similarity_threshold: DEFAULT_SIMILARITY_THRESHOLD,
            enable_temporal_patterns: true,
        }
    }
}

/// 上下文分析结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextAnalysisResult {
    pub context_boost: TagVector,
    pub conversation_influence: ConversationInfluence,
    pub sequence_pattern: TaskSequencePattern,
    pub temporal_pattern: TemporalPattern,
    pub overall_confidence: f32,
}

/// 对话影响分析
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationInfluence {
    pub recurring_themes: Vec<String>,
    pub topic_consistency: f32,
    pub sentiment_trend: String,
    pub key_concepts: HashMap<String, f32>,
}

/// 任务序列模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSequencePattern {
    pub pattern_type: SequencePatternType,
    pub confidence: f32,
    pub next_likely_dimensions: Vec<(String, f32)>,
    pub workflow_stage: String,
}

/// 序列模式类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SequencePatternType {
    Progressive,    // 递进式（复杂度递增）
    Cyclical,      // 循环式（重复模式）
    Exploratory,   // 探索式（随机性高）
    Focused,       // 专注式（单一领域）
}

/// 时间模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalPattern {
    pub time_of_day_bias: HashMap<String, f32>,
    pub urgency_trend: f32,
    pub session_duration_impact: f32,
    pub day_of_week_pattern: Option<String>,
}

impl ContextAwareAnalyzer {
    pub fn new(config: ContextConfig) -> Self {
        Self {
            conversation_analyzer: ConversationAnalyzer::new(),
            task_sequence_analyzer: TaskSequenceAnalyzer::new(),
            temporal_analyzer: TemporalAnalyzer::new(),
            config,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(ContextConfig::default())
    }

    /// 基于上下文分析输入
    pub fn analyze_with_context(
        &mut self, 
        input: &str, 
        context: &ExtendedContext
    ) -> ContextAnalysisResult {
        // 分析对话历史影响
        let conversation_influence = self.conversation_analyzer.analyze_conversation(
            input, 
            &context.conversation_history
        );

        // 分析任务序列模式
        let sequence_pattern = self.task_sequence_analyzer.analyze_sequence(
            input,
            &context.task_history
        );

        // 分析时间模式（如果启用）
        let temporal_pattern = if self.config.enable_temporal_patterns {
            self.temporal_analyzer.analyze_temporal_context(
                input,
                context.current_time,
                &context.session_info
            )
        } else {
            TemporalPattern {
                time_of_day_bias: HashMap::new(),
                urgency_trend: 0.0,
                session_duration_impact: 0.0,
                day_of_week_pattern: None,
            }
        };

        // 生成上下文增强
        let context_boost = self.generate_context_boost(
            input,
            &conversation_influence,
            &sequence_pattern,
            &temporal_pattern
        );

        // 计算整体置信度
        let overall_confidence = self.calculate_context_confidence(
            &conversation_influence,
            &sequence_pattern,
            &temporal_pattern
        );

        ContextAnalysisResult {
            context_boost,
            conversation_influence,
            sequence_pattern,
            temporal_pattern,
            overall_confidence,
        }
    }

    /// 生成上下文增强的标签向量
    fn generate_context_boost(
        &self,
        input: &str,
        conversation: &ConversationInfluence,
        sequence: &TaskSequencePattern,
        temporal: &TemporalPattern
    ) -> TagVector {
        let mut boost_vector = TagVector::new();

        // 基于对话主题的增强
        for (concept, weight) in &conversation.key_concepts {
            if input.to_lowercase().contains(&concept.to_lowercase()) {
                // 假设概念到维度的映射
                let dimension = self.concept_to_dimension(concept);
                let boost = weight * self.config.conversation_weight;
                boost_vector.set(&dimension, boost);
            }
        }

        // 基于序列模式的增强
        for (dimension, likelihood) in &sequence.next_likely_dimensions {
            let existing_boost = boost_vector.get(dimension);
            let sequence_boost = likelihood * self.config.sequence_weight;
            boost_vector.set(dimension, existing_boost + sequence_boost);
        }

        // 基于时间模式的增强
        for (dimension, bias) in &temporal.time_of_day_bias {
            let existing_boost = boost_vector.get(dimension);
            let temporal_boost = bias * self.config.temporal_weight;
            boost_vector.set(dimension, existing_boost + temporal_boost);
        }

        boost_vector
    }

    /// 概念到维度的映射（简化版）
    fn concept_to_dimension(&self, concept: &str) -> String {
        match concept.to_lowercase().as_str() {
            "create" | "design" | "invent" | "innovative" => "creativity_level".to_string(),
            "complex" | "difficult" | "advanced" | "sophisticated" => "technical_complexity".to_string(),
            "urgent" | "asap" | "quickly" | "immediate" => "urgency".to_string(),
            _ => "general".to_string(),
        }
    }

    /// 计算上下文置信度
    fn calculate_context_confidence(
        &self,
        conversation: &ConversationInfluence,
        sequence: &TaskSequencePattern,
        temporal: &TemporalPattern
    ) -> f32 {
        let conversation_confidence = conversation.topic_consistency;
        let sequence_confidence = sequence.confidence;
        let temporal_confidence = temporal.urgency_trend.abs().min(1.0);

        conversation_confidence * self.config.conversation_weight +
         sequence_confidence * self.config.sequence_weight +
         temporal_confidence * self.config.temporal_weight
    }
}

/// 对话历史分析器
pub struct ConversationAnalyzer {
    theme_extractor: ThemeExtractor,
    sentiment_analyzer: SentimentAnalyzer,
}

impl ConversationAnalyzer {
    pub fn new() -> Self {
        Self {
            theme_extractor: ThemeExtractor::new(),
            sentiment_analyzer: SentimentAnalyzer::new(),
        }
    }

    pub fn analyze_conversation(
        &mut self,
        current_input: &str,
        history: &[ConversationTurn]
    ) -> ConversationInfluence {
        // 提取历史主题
        let recurring_themes = self.theme_extractor.extract_themes(history);

        // 计算主题一致性
        let topic_consistency = self.calculate_topic_consistency(current_input, history);

        // 分析情感趋势
        let sentiment_trend = self.sentiment_analyzer.analyze_trend(history);

        // 提取关键概念及其权重
        let key_concepts = self.extract_key_concepts(current_input, history);

        ConversationInfluence {
            recurring_themes,
            topic_consistency,
            sentiment_trend,
            key_concepts,
        }
    }

    fn calculate_topic_consistency(&self, current_input: &str, history: &[ConversationTurn]) -> f32 {
        if history.is_empty() {
            return 0.0;
        }

        let current_words: Vec<&str> = current_input.split_whitespace().collect();
        let mut total_similarity = 0.0f32;

        for turn in history.iter().take(5) { // 只考虑最近5轮对话
            let turn_words: Vec<&str> = turn.user_input.split_whitespace().collect();
            let similarity = self.calculate_word_overlap(&current_words, &turn_words);
            total_similarity += similarity;
        }

        (total_similarity / history.len().min(5) as f32).min(1.0)
    }

    fn calculate_word_overlap(&self, words1: &[&str], words2: &[&str]) -> f32 {
        let set1: std::collections::HashSet<&str> = words1.iter().cloned().collect();
        let set2: std::collections::HashSet<&str> = words2.iter().cloned().collect();
        
        let intersection = set1.intersection(&set2).count();
        let union = set1.union(&set2).count();
        
        if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        }
    }

    fn extract_key_concepts(&self, current_input: &str, history: &[ConversationTurn]) -> HashMap<String, f32> {
        let mut concepts = HashMap::new();
        let mut word_counts = HashMap::new();

        // 统计当前输入的词汇
        for word in current_input.split_whitespace() {
            let clean_word = word.to_lowercase();
            if clean_word.len() > 3 { // 过滤短词
                *word_counts.entry(clean_word).or_insert(0) += 2; // 当前输入权重更高
            }
        }

        // 统计历史对话的词汇
        for (i, turn) in history.iter().rev().enumerate().take(5) {
            let decay = 0.8_f32.powi(i as i32); // 历史衰减
            for word in turn.user_input.split_whitespace() {
                let clean_word = word.to_lowercase();
                if clean_word.len() > 3 {
                    *word_counts.entry(clean_word).or_insert(0) += (decay * 1.0) as i32;
                }
            }
        }

        // 转换为概念权重
        let total_count: i32 = word_counts.values().sum();
        for (word, count) in word_counts {
            if count > 1 { // 只保留出现多次的概念
                let weight = count as f32 / total_count as f32;
                concepts.insert(word, weight.min(1.0));
            }
        }

        concepts
    }
}

/// 任务序列分析器
pub struct TaskSequenceAnalyzer {
    pattern_detector: PatternDetector,
}

impl TaskSequenceAnalyzer {
    pub fn new() -> Self {
        Self {
            pattern_detector: PatternDetector::new(),
        }
    }

    pub fn analyze_sequence(
        &mut self,
        current_input: &str,
        task_history: &[TaskRecord]
    ) -> TaskSequencePattern {
        // 检测序列模式
        let pattern_type = self.pattern_detector.detect_pattern(task_history);

        // 计算模式置信度
        let confidence = self.calculate_pattern_confidence(task_history, &pattern_type);

        // 预测下一个可能的维度
        let next_likely_dimensions = self.predict_next_dimensions(current_input, task_history);

        // 确定工作流阶段
        let workflow_stage = self.determine_workflow_stage(task_history);

        TaskSequencePattern {
            pattern_type,
            confidence,
            next_likely_dimensions,
            workflow_stage,
        }
    }

    fn calculate_pattern_confidence(&self, history: &[TaskRecord], pattern: &SequencePatternType) -> f32 {
        if history.len() < 3 {
            return 0.3; // 历史不足，置信度较低
        }

        // 简化的置信度计算
        match pattern {
            SequencePatternType::Progressive => {
                // 检查复杂度是否递增
                let mut increases = 0;
                for window in history.windows(2) {
                    if let (Some(prev_complexity), Some(curr_complexity)) = (
                        window[0].tag_vector.dimensions.get("technical_complexity"),
                        window[1].tag_vector.dimensions.get("technical_complexity")
                    ) {
                        if curr_complexity > prev_complexity {
                            increases += 1;
                        }
                    }
                }
                increases as f32 / (history.len() - 1) as f32
            }
            SequencePatternType::Focused => {
                // 检查是否专注于某个维度
                if let Some(first_record) = history.first() {
                    let mut consistency_sum = 0.0f32;
                    let mut count = 0;

                    for dimension in first_record.tag_vector.dimensions.keys() {
                        let values: Vec<f32> = history.iter()
                            .map(|r| r.tag_vector.get(dimension))
                            .collect();
                        
                        let avg = values.iter().sum::<f32>() / values.len() as f32;
                        let variance = values.iter()
                            .map(|v| (v - avg).powi(2))
                            .sum::<f32>() / values.len() as f32;
                        
                        consistency_sum += 1.0 / (1.0 + variance);
                        count += 1;
                    }

                    if count > 0 {
                        consistency_sum / count as f32
                    } else {
                        0.5
                    }
                } else {
                    0.5
                }
            }
            _ => 0.5, // 其他模式的默认置信度
        }
    }

    fn predict_next_dimensions(&self, _current_input: &str, history: &[TaskRecord]) -> Vec<(String, f32)> {
        if history.is_empty() {
            return Vec::new();
        }

        // 基于历史频率预测
        let mut dimension_frequencies = HashMap::new();
        
        for record in history.iter().rev().take(5) { // 最近5条记录
            for (dimension, value) in &record.tag_vector.dimensions {
                if *value > 0.5 { // 只考虑高分维度
                    *dimension_frequencies.entry(dimension.clone()).or_insert(0) += 1;
                }
            }
        }

        let total_records = history.len().min(5);
        let mut predictions: Vec<(String, f32)> = dimension_frequencies
            .into_iter()
            .map(|(dim, freq)| (dim, freq as f32 / total_records as f32))
            .collect();

        predictions.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        predictions.truncate(3); // 只返回前3个预测

        predictions
    }

    fn determine_workflow_stage(&self, history: &[TaskRecord]) -> String {
        if history.is_empty() {
            return "初始阶段".to_string();
        }

        let recent_records = history.iter().rev().take(3).collect::<Vec<_>>();
        
        // 简化的工作流阶段判断
        let avg_creativity: f32 = recent_records.iter()
            .map(|r| r.tag_vector.get("creativity_level"))
            .sum::<f32>() / recent_records.len() as f32;
            
        let avg_complexity: f32 = recent_records.iter()
            .map(|r| r.tag_vector.get("technical_complexity"))
            .sum::<f32>() / recent_records.len() as f32;

        if avg_creativity > 0.7 {
            "创意构思阶段".to_string()
        } else if avg_complexity > 0.7 {
            "技术实现阶段".to_string()
        } else {
            "执行优化阶段".to_string()
        }
    }
}

/// 时间模式分析器
pub struct TemporalAnalyzer;

impl TemporalAnalyzer {
    pub fn new() -> Self {
        Self
    }

    pub fn analyze_temporal_context(
        &self,
        _input: &str,
        current_time: DateTime<Utc>,
        session_info: &SessionInfo
    ) -> TemporalPattern {
        let mut time_of_day_bias = HashMap::new();
        
        // 基于时间的偏好分析
        let hour = current_time.hour();
        match hour {
            6..=11 => {
                // 早上偏向创造性任务
                time_of_day_bias.insert("creativity_level".to_string(), 0.2);
            }
            12..=17 => {
                // 下午偏向技术性任务
                time_of_day_bias.insert("technical_complexity".to_string(), 0.2);
            }
            18..=23 => {
                // 晚上偏向紧急任务处理
                time_of_day_bias.insert("urgency".to_string(), 0.1);
            }
            _ => {
                // 深夜，一般不建议复杂任务
                time_of_day_bias.insert("technical_complexity".to_string(), -0.1);
            }
        }

        // 会话持续时间影响
        let session_duration_impact = if let Some(start_time) = session_info.start_time {
            let duration = current_time.signed_duration_since(start_time);
            if duration > Duration::hours(2) {
                -0.1 // 长时间会话可能导致疲劳
            } else {
                0.05 // 适中时间保持高效
            }
        } else {
            0.0
        };

        // 紧急度趋势（基于时间压力）
        let urgency_trend = if current_time.hour() >= 17 && current_time.weekday().number_from_monday() <= 5 {
            0.2 // 工作日下班前
        } else {
            0.0
        };

        TemporalPattern {
            time_of_day_bias,
            urgency_trend,
            session_duration_impact,
            day_of_week_pattern: Some(format!("{:?}", current_time.weekday())),
        }
    }
}

/// 支持数据结构

#[derive(Debug, Clone)]
pub struct ExtendedContext {
    pub conversation_history: Vec<ConversationTurn>,
    pub task_history: Vec<TaskRecord>,
    pub current_time: DateTime<Utc>,
    pub session_info: SessionInfo,
    pub user_preferences: HashMap<String, f32>,
}

#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub timestamp: DateTime<Utc>,
    pub user_input: String,
    pub assistant_response: String,
    pub turn_id: u64,
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub timestamp: DateTime<Utc>,
    pub input: String,
    pub tag_vector: TagVector,
    pub task_type: String,
    pub success: bool,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub start_time: Option<DateTime<Utc>>,
    pub total_turns: u32,
    pub user_id: Option<String>,
}

// 辅助分析器
struct ThemeExtractor;
impl ThemeExtractor {
    fn new() -> Self { Self }
    fn extract_themes(&self, _history: &[ConversationTurn]) -> Vec<String> {
        // 简化实现
        vec!["创意设计".to_string(), "技术开发".to_string()]
    }
}

struct SentimentAnalyzer;
impl SentimentAnalyzer {
    fn new() -> Self { Self }
    fn analyze_trend(&self, _history: &[ConversationTurn]) -> String {
        // 简化实现
        "积极".to_string()
    }
}

struct PatternDetector;
impl PatternDetector {
    fn new() -> Self { Self }
    fn detect_pattern(&self, history: &[TaskRecord]) -> SequencePatternType {
        if history.len() < 3 {
            return SequencePatternType::Exploratory;
        }

        // 检查是否有递增模式
        let mut progressive_count = 0;
        for window in history.windows(2) {
            let prev_complexity = window[0].tag_vector.get("technical_complexity");
            let curr_complexity = window[1].tag_vector.get("technical_complexity");
            if curr_complexity > prev_complexity + 0.1 {
                progressive_count += 1;
            }
        }

        if progressive_count > history.len() / 2 {
            SequencePatternType::Progressive
        } else {
            SequencePatternType::Focused
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_analyzer() {
        let mut analyzer = ContextAwareAnalyzer::with_defaults();
        
        let context = ExtendedContext {
            conversation_history: vec![
                ConversationTurn {
                    timestamp: Utc::now(),
                    user_input: "I want to create something innovative".to_string(),
                    assistant_response: "Great idea!".to_string(),
                    turn_id: 1,
                }
            ],
            task_history: vec![],
            current_time: Utc::now(),
            session_info: SessionInfo {
                session_id: "test".to_string(),
                start_time: Some(Utc::now()),
                total_turns: 1,
                user_id: None,
            },
            user_preferences: HashMap::new(),
        };

        let result = analyzer.analyze_with_context("design a new product", &context);
        
        assert!(result.overall_confidence >= 0.0);
        assert!(!result.conversation_influence.key_concepts.is_empty());
    }

    #[test]
    fn test_conversation_analyzer() {
        let mut analyzer = ConversationAnalyzer::new();
        
        let history = vec![
            ConversationTurn {
                timestamp: Utc::now(),
                user_input: "create design innovative".to_string(),
                assistant_response: "Ok".to_string(),
                turn_id: 1,
            }
        ];

        let result = analyzer.analyze_conversation("design something creative", &history);
        assert!(result.topic_consistency > 0.0);
    }
}