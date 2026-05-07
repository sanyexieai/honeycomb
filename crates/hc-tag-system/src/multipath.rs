//! 多路匹配器模块 - 整合规则、语义、统计等多种匹配方法

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{Dimension, FuzzyMatcher, MatchResult, TagVector};

/// 匹配路径类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatchPathType {
    RuleBased,   // 规则匹配
    Semantic,    // 语义匹配
    Statistical, // 统计匹配
    Contextual,  // 上下文匹配
    Hybrid,      // 混合匹配
}

/// 单个路径的匹配结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathMatchResult {
    pub path_type: MatchPathType,
    pub tag_vector: TagVector,
    pub confidence: f32,
    pub match_details: Vec<MatchResult>,
    pub processing_time_ms: u64,
    pub metadata: HashMap<String, String>,
}

/// 多路匹配的最终结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiPathResult {
    pub input: String,
    pub final_tag_vector: TagVector,
    pub path_results: Vec<PathMatchResult>,
    pub fusion_strategy: FusionStrategy,
    pub overall_confidence: f32,
    pub consensus_score: f32, // 各路径间的一致性分数
}

/// 融合策略
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FusionStrategy {
    WeightedAverage, // 加权平均
    MaxConfidence,   // 最高置信度
    Voting,          // 投票机制
    Adaptive,        // 自适应融合
}

/// 多路匹配器配置
#[derive(Debug, Clone)]
pub struct MultiPathConfig {
    pub enabled_paths: Vec<MatchPathType>,
    pub fusion_strategy: FusionStrategy,
    pub weights: HashMap<MatchPathType, f32>,
    pub confidence_threshold: f32,
    pub consensus_threshold: f32,
    pub timeout_ms: u64,
}

pub const DEFAULT_RULE_BASED_WEIGHT: f32 = 0.4;
pub const DEFAULT_SEMANTIC_WEIGHT: f32 = 0.3;
pub const DEFAULT_STATISTICAL_WEIGHT: f32 = 0.2;
pub const DEFAULT_CONTEXTUAL_WEIGHT: f32 = 0.1;
pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 0.3;
pub const DEFAULT_CONSENSUS_THRESHOLD: f32 = 0.6;
pub const DEFAULT_PATH_TIMEOUT_MS: u64 = 1000;

impl Default for MultiPathConfig {
    fn default() -> Self {
        let mut weights = HashMap::new();
        weights.insert(MatchPathType::RuleBased, DEFAULT_RULE_BASED_WEIGHT);
        weights.insert(MatchPathType::Semantic, DEFAULT_SEMANTIC_WEIGHT);
        weights.insert(MatchPathType::Statistical, DEFAULT_STATISTICAL_WEIGHT);
        weights.insert(MatchPathType::Contextual, DEFAULT_CONTEXTUAL_WEIGHT);

        Self {
            enabled_paths: vec![
                MatchPathType::RuleBased,
                MatchPathType::Semantic,
                MatchPathType::Statistical,
            ],
            fusion_strategy: FusionStrategy::WeightedAverage,
            weights,
            confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
            consensus_threshold: DEFAULT_CONSENSUS_THRESHOLD,
            timeout_ms: DEFAULT_PATH_TIMEOUT_MS,
        }
    }
}

/// 多路匹配器
pub struct MultiPathMatcher {
    config: MultiPathConfig,
    rule_based_matcher: RuleBasedMatcher,
    semantic_matcher: SemanticMatcher,
    statistical_matcher: StatisticalMatcher,
    contextual_matcher: Option<ContextualMatcher>,
}

impl MultiPathMatcher {
    pub fn new(config: MultiPathConfig, dimensions: &HashMap<String, Dimension>) -> Self {
        Self {
            rule_based_matcher: RuleBasedMatcher::new(dimensions),
            semantic_matcher: SemanticMatcher::new(dimensions),
            statistical_matcher: StatisticalMatcher::new(dimensions),
            contextual_matcher: if config.enabled_paths.contains(&MatchPathType::Contextual) {
                Some(ContextualMatcher::new(dimensions))
            } else {
                None
            },
            config,
        }
    }

    /// 执行多路匹配
    pub fn match_input(&self, input: &str, context: Option<&MatchContext>) -> MultiPathResult {
        let _start_time = std::time::Instant::now();
        let mut path_results = Vec::new();

        // 并行执行各个匹配路径
        for path_type in &self.config.enabled_paths {
            let path_start = std::time::Instant::now();

            let result = match path_type {
                MatchPathType::RuleBased => self.rule_based_matcher.match_input(input),
                MatchPathType::Semantic => self.semantic_matcher.match_input(input),
                MatchPathType::Statistical => self.statistical_matcher.match_input(input),
                MatchPathType::Contextual => {
                    if let Some(contextual) = &self.contextual_matcher {
                        contextual.match_input(input, context)
                    } else {
                        continue;
                    }
                }
                MatchPathType::Hybrid => {
                    // 混合匹配将在后续实现
                    continue;
                }
            };

            let processing_time = path_start.elapsed().as_millis() as u64;

            let path_result = PathMatchResult {
                path_type: path_type.clone(),
                tag_vector: result.tag_vector,
                confidence: result.confidence,
                match_details: result.match_details,
                processing_time_ms: processing_time,
                metadata: result.metadata,
            };

            path_results.push(path_result);
        }

        // 融合结果
        let (final_tag_vector, overall_confidence) = self.fuse_results(&path_results);
        let consensus_score = self.calculate_consensus(&path_results);

        MultiPathResult {
            input: input.to_string(),
            final_tag_vector,
            path_results,
            fusion_strategy: self.config.fusion_strategy.clone(),
            overall_confidence,
            consensus_score,
        }
    }

    /// 融合多个路径的结果
    fn fuse_results(&self, path_results: &[PathMatchResult]) -> (TagVector, f32) {
        if path_results.is_empty() {
            return (TagVector::new(), 0.0);
        }

        match self.config.fusion_strategy {
            FusionStrategy::WeightedAverage => self.weighted_average_fusion(path_results),
            FusionStrategy::MaxConfidence => self.max_confidence_fusion(path_results),
            FusionStrategy::Voting => self.voting_fusion(path_results),
            FusionStrategy::Adaptive => self.adaptive_fusion(path_results),
        }
    }

    /// 加权平均融合
    fn weighted_average_fusion(&self, path_results: &[PathMatchResult]) -> (TagVector, f32) {
        let mut fused_vector = TagVector::new();
        let mut total_weight = 0.0f32;
        let mut weighted_confidence = 0.0f32;

        // 收集所有维度
        let mut all_dimensions = std::collections::HashSet::new();
        for result in path_results {
            for dimension in result.tag_vector.dimensions.keys() {
                all_dimensions.insert(dimension.clone());
            }
        }

        // 对每个维度进行加权平均
        for dimension in all_dimensions {
            let mut weighted_sum = 0.0f32;
            let mut dimension_weight = 0.0f32;

            for result in path_results {
                if let Some(weight) = self.config.weights.get(&result.path_type) {
                    let value = result.tag_vector.get(&dimension);
                    weighted_sum += value * weight * result.confidence;
                    dimension_weight += weight * result.confidence;
                }
            }

            if dimension_weight > 0.0 {
                fused_vector.set(&dimension, weighted_sum / dimension_weight);
            }
        }

        // 计算整体置信度
        for result in path_results {
            if let Some(weight) = self.config.weights.get(&result.path_type) {
                weighted_confidence += result.confidence * weight;
                total_weight += weight;
            }
        }

        let overall_confidence = if total_weight > 0.0 {
            weighted_confidence / total_weight
        } else {
            0.0
        };

        (fused_vector, overall_confidence)
    }

    /// 最高置信度融合
    fn max_confidence_fusion(&self, path_results: &[PathMatchResult]) -> (TagVector, f32) {
        if let Some(best_result) = path_results.iter().max_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            (best_result.tag_vector.clone(), best_result.confidence)
        } else {
            (TagVector::new(), 0.0)
        }
    }

    /// 投票融合
    fn voting_fusion(&self, path_results: &[PathMatchResult]) -> (TagVector, f32) {
        // 简化版投票：对每个维度，选择多数路径认为的值
        let mut fused_vector = TagVector::new();

        // 收集所有维度
        let mut all_dimensions = std::collections::HashSet::new();
        for result in path_results {
            for dimension in result.tag_vector.dimensions.keys() {
                all_dimensions.insert(dimension.clone());
            }
        }

        for dimension in all_dimensions {
            let values: Vec<f32> = path_results
                .iter()
                .map(|r| r.tag_vector.get(&dimension))
                .collect();

            // 简单投票：取中位数
            let mut sorted_values = values;
            sorted_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            let median = if sorted_values.len() % 2 == 0 {
                let mid = sorted_values.len() / 2;
                (sorted_values[mid - 1] + sorted_values[mid]) / 2.0
            } else {
                sorted_values[sorted_values.len() / 2]
            };

            fused_vector.set(&dimension, median);
        }

        // 投票融合的置信度基于一致性
        let consensus = self.calculate_consensus(path_results);
        (fused_vector, consensus)
    }

    /// 自适应融合
    fn adaptive_fusion(&self, path_results: &[PathMatchResult]) -> (TagVector, f32) {
        // 根据各路径的历史表现动态调整权重
        // 这里简化为基于置信度的动态权重
        let mut adjusted_weights = HashMap::new();
        let total_confidence: f32 = path_results.iter().map(|r| r.confidence).sum();

        for result in path_results {
            let adaptive_weight = if total_confidence > 0.0 {
                result.confidence / total_confidence
            } else {
                1.0 / path_results.len() as f32
            };
            adjusted_weights.insert(result.path_type.clone(), adaptive_weight);
        }

        // 使用调整后的权重进行加权平均
        let original_weights = self.config.weights.clone();
        let mut modified_config = self.config.clone();
        modified_config.weights = adjusted_weights;

        // 临时创建一个新的匹配器来使用调整后的权重
        let temp_matcher = MultiPathMatcher {
            config: modified_config,
            rule_based_matcher: RuleBasedMatcher::new(&HashMap::new()),
            semantic_matcher: SemanticMatcher::new(&HashMap::new()),
            statistical_matcher: StatisticalMatcher::new(&HashMap::new()),
            contextual_matcher: None,
        };

        temp_matcher.weighted_average_fusion(path_results)
    }

    /// 计算各路径间的一致性分数
    fn calculate_consensus(&self, path_results: &[PathMatchResult]) -> f32 {
        if path_results.len() < 2 {
            return 1.0;
        }

        let mut total_similarity = 0.0f32;
        let mut comparisons = 0;

        for i in 0..path_results.len() {
            for j in (i + 1)..path_results.len() {
                let similarity = path_results[i]
                    .tag_vector
                    .cosine_similarity(&path_results[j].tag_vector);
                total_similarity += similarity;
                comparisons += 1;
            }
        }

        if comparisons > 0 {
            total_similarity / comparisons as f32
        } else {
            0.0
        }
    }

    /// 获取最佳路径建议
    pub fn get_best_path_recommendation(
        &self,
        results: &[PathMatchResult],
    ) -> Option<MatchPathType> {
        results
            .iter()
            .max_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|r| r.path_type.clone())
    }
}

/// 匹配上下文
#[derive(Debug, Clone)]
pub struct MatchContext {
    pub conversation_history: Vec<String>,
    pub user_preferences: HashMap<String, f32>,
    pub temporal_context: Option<chrono::DateTime<chrono::Utc>>,
}

/// 通用匹配结果
#[derive(Debug, Clone)]
pub struct GenericMatchResult {
    pub tag_vector: TagVector,
    pub confidence: f32,
    pub match_details: Vec<MatchResult>,
    pub metadata: HashMap<String, String>,
}

/// 规则匹配器
pub struct RuleBasedMatcher {
    fuzzy_matcher: FuzzyMatcher,
    dimensions: HashMap<String, Dimension>,
}

impl RuleBasedMatcher {
    pub fn new(dimensions: &HashMap<String, Dimension>) -> Self {
        Self {
            fuzzy_matcher: FuzzyMatcher::with_defaults(),
            dimensions: dimensions.clone(),
        }
    }

    pub fn match_input(&self, input: &str) -> GenericMatchResult {
        let mut tag_vector = TagVector::new();
        let mut all_matches = Vec::new();
        let mut metadata = HashMap::new();

        for (dimension_id, dimension) in &self.dimensions {
            let high_matches = self
                .fuzzy_matcher
                .fuzzy_match_keywords(input, &dimension.keywords.high);
            let medium_matches = self
                .fuzzy_matcher
                .fuzzy_match_keywords(input, &dimension.keywords.medium);
            let low_matches = self
                .fuzzy_matcher
                .fuzzy_match_keywords(input, &dimension.keywords.low);

            let mut score = dimension.default_value;
            let mut total_boost = 0.0f32;

            for match_result in &high_matches {
                total_boost += 0.25 * match_result.score;
                all_matches.push(match_result.clone());
            }

            for match_result in &medium_matches {
                total_boost += 0.15 * match_result.score;
                all_matches.push(match_result.clone());
            }

            for match_result in &low_matches {
                total_boost -= 0.15 * match_result.score;
                all_matches.push(match_result.clone());
            }

            score = (score + total_boost).clamp(0.0, 1.0);
            tag_vector.set(dimension_id, score);
        }

        let confidence = if all_matches.is_empty() { 0.1 } else { 0.8 };
        metadata.insert("matcher_type".to_string(), "rule_based".to_string());
        metadata.insert("total_matches".to_string(), all_matches.len().to_string());

        GenericMatchResult {
            tag_vector,
            confidence,
            match_details: all_matches,
            metadata,
        }
    }
}

/// 语义匹配器
pub struct SemanticMatcher {
    dimensions: HashMap<String, Dimension>,
}

impl SemanticMatcher {
    pub fn new(dimensions: &HashMap<String, Dimension>) -> Self {
        Self {
            dimensions: dimensions.clone(),
        }
    }

    pub fn match_input(&self, input: &str) -> GenericMatchResult {
        // 简化版语义匹配 - 基于词共现和语义距离
        let mut tag_vector = TagVector::new();
        let mut metadata = HashMap::new();

        let input_words: Vec<&str> = input.split_whitespace().collect();

        for (dimension_id, dimension) in &self.dimensions {
            let mut semantic_score = dimension.default_value;

            // 简单的语义分析：检查词汇的语义场
            let creativity_indicators = ["new", "novel", "fresh", "unique", "breakthrough"];
            let complexity_indicators = ["complex", "intricate", "sophisticated", "advanced"];
            let urgency_indicators = ["now", "immediately", "urgent", "asap", "quickly"];

            match dimension_id.as_str() {
                "creativity_level" => {
                    let creativity_words = input_words
                        .iter()
                        .filter(|&&word| {
                            creativity_indicators.contains(&word.to_lowercase().as_str())
                        })
                        .count();
                    semantic_score += creativity_words as f32 * 0.2;
                }
                "technical_complexity" => {
                    let complexity_words = input_words
                        .iter()
                        .filter(|&&word| {
                            complexity_indicators.contains(&word.to_lowercase().as_str())
                        })
                        .count();
                    semantic_score += complexity_words as f32 * 0.2;
                }
                "urgency" => {
                    let urgency_words = input_words
                        .iter()
                        .filter(|&&word| urgency_indicators.contains(&word.to_lowercase().as_str()))
                        .count();
                    semantic_score += urgency_words as f32 * 0.2;
                }
                _ => {
                    // 默认处理
                }
            }

            semantic_score = semantic_score.clamp(0.0, 1.0);
            tag_vector.set(dimension_id, semantic_score);
        }

        metadata.insert("matcher_type".to_string(), "semantic".to_string());
        metadata.insert(
            "analysis_method".to_string(),
            "word_cooccurrence".to_string(),
        );

        GenericMatchResult {
            tag_vector,
            confidence: 0.6,
            match_details: Vec::new(),
            metadata,
        }
    }
}

/// 统计匹配器
pub struct StatisticalMatcher {
    dimensions: HashMap<String, Dimension>,
}

impl StatisticalMatcher {
    pub fn new(dimensions: &HashMap<String, Dimension>) -> Self {
        Self {
            dimensions: dimensions.clone(),
        }
    }

    pub fn match_input(&self, input: &str) -> GenericMatchResult {
        // 基于统计特征的匹配
        let mut tag_vector = TagVector::new();
        let mut metadata = HashMap::new();

        let input_length = input.len();
        let word_count = input.split_whitespace().count();
        let avg_word_length = if word_count > 0 {
            input_length as f32 / word_count as f32
        } else {
            0.0
        };

        for (dimension_id, dimension) in &self.dimensions {
            let mut statistical_score = dimension.default_value;

            // 基于统计特征推断
            match dimension_id.as_str() {
                "technical_complexity" => {
                    // 长词和长句子通常表示更高的技术复杂度
                    if avg_word_length > 6.0 {
                        statistical_score += 0.2;
                    }
                    if word_count > 15 {
                        statistical_score += 0.1;
                    }
                }
                "creativity_level" => {
                    // 词汇多样性可能表示创造性
                    let unique_words: std::collections::HashSet<&str> =
                        input.split_whitespace().collect();
                    let diversity_ratio = unique_words.len() as f32 / word_count.max(1) as f32;
                    statistical_score += diversity_ratio * 0.3;
                }
                "urgency" => {
                    // 短句和感叹号可能表示紧急程度
                    if word_count < 8 {
                        statistical_score += 0.1;
                    }
                    if input.contains('!') {
                        statistical_score += 0.2;
                    }
                }
                _ => {
                    // 默认处理
                }
            }

            statistical_score = statistical_score.clamp(0.0, 1.0);
            tag_vector.set(dimension_id, statistical_score);
        }

        metadata.insert("matcher_type".to_string(), "statistical".to_string());
        metadata.insert("word_count".to_string(), word_count.to_string());
        metadata.insert("avg_word_length".to_string(), avg_word_length.to_string());

        GenericMatchResult {
            tag_vector,
            confidence: 0.4,
            match_details: Vec::new(),
            metadata,
        }
    }
}

/// 上下文匹配器
pub struct ContextualMatcher {
    dimensions: HashMap<String, Dimension>,
}

impl ContextualMatcher {
    pub fn new(dimensions: &HashMap<String, Dimension>) -> Self {
        Self {
            dimensions: dimensions.clone(),
        }
    }

    pub fn match_input(&self, input: &str, context: Option<&MatchContext>) -> GenericMatchResult {
        let mut tag_vector = TagVector::new();
        let mut metadata = HashMap::new();

        for (dimension_id, dimension) in &self.dimensions {
            let mut contextual_score = dimension.default_value;

            if let Some(ctx) = context {
                // 基于用户偏好调整
                if let Some(preference) = ctx.user_preferences.get(dimension_id) {
                    contextual_score = contextual_score * (1.0 - 0.3) + preference * 0.3;
                }

                // 基于对话历史调整
                if !ctx.conversation_history.is_empty() {
                    // 简化：如果历史中有相似主题，增加相关维度的权重
                    let history_text = ctx.conversation_history.join(" ");
                    if history_text
                        .to_lowercase()
                        .contains(&input.to_lowercase()[..input.len().min(10)])
                    {
                        contextual_score += 0.1;
                    }
                }
            }

            contextual_score = contextual_score.clamp(0.0, 1.0);
            tag_vector.set(dimension_id, contextual_score);
        }

        metadata.insert("matcher_type".to_string(), "contextual".to_string());
        metadata.insert("has_context".to_string(), context.is_some().to_string());

        GenericMatchResult {
            tag_vector,
            confidence: if context.is_some() { 0.7 } else { 0.2 },
            match_details: Vec::new(),
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dimension, DimensionKeywords};

    fn create_test_dimensions() -> HashMap<String, Dimension> {
        let mut dimensions = HashMap::new();

        dimensions.insert(
            "creativity_level".to_string(),
            Dimension {
                id: "creativity_level".to_string(),
                name: "Creativity Level".to_string(),
                description: "Test dimension".to_string(),
                scale_min: 0.0,
                scale_max: 1.0,
                default_value: 0.3,
                keywords: DimensionKeywords {
                    low: vec!["copy".to_string(), "duplicate".to_string()],
                    medium: vec!["modify".to_string(), "improve".to_string()],
                    high: vec![
                        "create".to_string(),
                        "invent".to_string(),
                        "design".to_string(),
                    ],
                },
            },
        );

        dimensions
    }

    #[test]
    fn test_multipath_matching() {
        let dimensions = create_test_dimensions();
        let config = MultiPathConfig::default();
        let matcher = MultiPathMatcher::new(config, &dimensions);

        let result = matcher.match_input("I want to create something innovative", None);

        assert_eq!(result.input, "I want to create something innovative");
        assert!(!result.path_results.is_empty());
        assert!(result.overall_confidence > 0.0);
        assert!(result.consensus_score >= 0.0 && result.consensus_score <= 1.0);

        // 检查是否包含规则匹配结果
        let has_rule_based = result
            .path_results
            .iter()
            .any(|r| r.path_type == MatchPathType::RuleBased);
        assert!(has_rule_based);
    }

    #[test]
    fn test_fusion_strategies() {
        let dimensions = create_test_dimensions();

        // 测试不同的融合策略
        for strategy in [
            FusionStrategy::WeightedAverage,
            FusionStrategy::MaxConfidence,
            FusionStrategy::Voting,
        ] {
            let mut config = MultiPathConfig::default();
            config.fusion_strategy = strategy;

            let matcher = MultiPathMatcher::new(config, &dimensions);
            let result = matcher.match_input("create innovative design", None);

            assert!(result.final_tag_vector.get("creativity_level") > 0.0);
        }
    }

    #[test]
    fn test_contextual_matching() {
        let dimensions = create_test_dimensions();
        let mut config = MultiPathConfig::default();
        config.enabled_paths.push(MatchPathType::Contextual);

        let matcher = MultiPathMatcher::new(config, &dimensions);

        let mut context = MatchContext {
            conversation_history: vec!["I love creative projects".to_string()],
            user_preferences: HashMap::new(),
            temporal_context: None,
        };
        context
            .user_preferences
            .insert("creativity_level".to_string(), 0.8);

        let result = matcher.match_input("design something", Some(&context));

        assert!(!result.path_results.is_empty());
        let has_contextual = result
            .path_results
            .iter()
            .any(|r| r.path_type == MatchPathType::Contextual);
        assert!(has_contextual);
    }
}
