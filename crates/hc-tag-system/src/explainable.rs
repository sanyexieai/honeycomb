//! 可解释性模块 - 提供决策路径和置信度分解

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{DimensionAnalysisResult, MatchType};

/// 分类解释结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationExplanation {
    pub input: String,
    pub primary_reason: String,
    pub contributing_factors: Vec<ContributingFactor>,
    pub confidence_breakdown: ConfidenceBreakdown,
    pub alternative_possibilities: Vec<AlternativePossibility>,
    pub decision_path: DecisionPath,
    pub summary: String,
}

/// 贡献因子
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributingFactor {
    pub factor_type: FactorType,
    pub description: String,
    pub impact_score: f32,
    pub evidence: Vec<String>,
}

/// 因子类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FactorType {
    ExactKeywordMatch, // 精确关键词匹配
    FuzzyMatch,        // 模糊匹配
    SynonymMatch,      // 同义词匹配
    ContextualHint,    // 上下文提示
    DefaultValue,      // 默认值影响
    UserHistory,       // 用户历史影响
}

/// 置信度分解
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceBreakdown {
    pub overall_confidence: f32,
    pub dimension_confidences: BTreeMap<String, DimensionConfidence>,
    pub uncertainty_sources: Vec<UncertaintySource>,
}

/// 维度置信度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionConfidence {
    pub dimension_id: String,
    pub confidence: f32,
    pub evidence_strength: f32,
    pub ambiguity_score: f32,
}

/// 不确定性来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncertaintySource {
    pub source: String,
    pub impact: f32,
    pub description: String,
}

/// 可选可能性
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlternativePossibility {
    pub scenario: String,
    pub probability: f32,
    pub required_changes: Vec<String>,
}

/// 决策路径
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionPath {
    pub steps: Vec<DecisionStep>,
    pub final_decision: String,
    pub critical_points: Vec<CriticalPoint>,
}

/// 决策步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionStep {
    pub step_number: u32,
    pub description: String,
    pub input_state: String,
    pub output_state: String,
    pub reasoning: String,
}

/// 关键决策点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticalPoint {
    pub location: String,
    pub decision: String,
    pub alternatives: Vec<String>,
    pub impact: f32,
}

/// 可解释性分析器
pub struct ExplainableAnalyzer;

impl ExplainableAnalyzer {
    /// 从维度分析结果生成完整解释
    pub fn generate_explanation(
        input: &str,
        dimension_details: &BTreeMap<String, DimensionAnalysisResult>,
    ) -> ClassificationExplanation {
        let contributing_factors = Self::extract_contributing_factors(dimension_details);
        let confidence_breakdown = Self::compute_confidence_breakdown(dimension_details);
        let alternative_possibilities = Self::generate_alternatives(dimension_details);
        let decision_path = Self::trace_decision_path(input, dimension_details);
        let primary_reason = Self::determine_primary_reason(&contributing_factors);
        let summary = Self::generate_summary(input, &primary_reason, &confidence_breakdown);

        ClassificationExplanation {
            input: input.to_string(),
            primary_reason,
            contributing_factors,
            confidence_breakdown,
            alternative_possibilities,
            decision_path,
            summary,
        }
    }

    /// 提取贡献因子
    fn extract_contributing_factors(
        dimension_details: &BTreeMap<String, DimensionAnalysisResult>,
    ) -> Vec<ContributingFactor> {
        let mut factors = Vec::new();

        for (dimension_id, details) in dimension_details {
            // 分析高权重匹配
            for match_result in &details.high_matches {
                let factor = ContributingFactor {
                    factor_type: Self::match_type_to_factor_type(&match_result.match_type),
                    description: format!(
                        "在维度 '{}' 中发现高权重匹配：'{}' (置信度: {:.2})",
                        dimension_id, match_result.keyword, match_result.score
                    ),
                    impact_score: match_result.score * 0.25, // 高权重因子
                    evidence: vec![
                        format!("匹配关键词: '{}'", match_result.keyword),
                        format!("匹配类型: {:?}", match_result.match_type),
                        format!("原始输入: '{}'", match_result.original_input),
                    ],
                };
                factors.push(factor);
            }

            // 分析中等权重匹配
            for match_result in &details.medium_matches {
                let factor = ContributingFactor {
                    factor_type: Self::match_type_to_factor_type(&match_result.match_type),
                    description: format!(
                        "在维度 '{}' 中发现中等权重匹配：'{}' (置信度: {:.2})",
                        dimension_id, match_result.keyword, match_result.score
                    ),
                    impact_score: match_result.score * 0.15, // 中等权重因子
                    evidence: vec![
                        format!("匹配关键词: '{}'", match_result.keyword),
                        format!("匹配类型: {:?}", match_result.match_type),
                    ],
                };
                factors.push(factor);
            }

            // 分析低权重匹配（降权）
            for match_result in &details.low_matches {
                let factor = ContributingFactor {
                    factor_type: Self::match_type_to_factor_type(&match_result.match_type),
                    description: format!(
                        "在维度 '{}' 中发现低权重匹配（降权）：'{}' (影响: -{:.2})",
                        dimension_id,
                        match_result.keyword,
                        match_result.score * 0.15
                    ),
                    impact_score: -(match_result.score * 0.15), // 负影响
                    evidence: vec![
                        format!("降权关键词: '{}'", match_result.keyword),
                        format!("匹配类型: {:?}", match_result.match_type),
                    ],
                };
                factors.push(factor);
            }
        }

        // 按影响分数排序
        factors.sort_by(|a, b| {
            b.impact_score
                .abs()
                .partial_cmp(&a.impact_score.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        factors
    }

    /// 将匹配类型转换为因子类型
    fn match_type_to_factor_type(match_type: &MatchType) -> FactorType {
        match match_type {
            MatchType::Exact => FactorType::ExactKeywordMatch,
            MatchType::Fuzzy => FactorType::FuzzyMatch,
            MatchType::Synonym => FactorType::SynonymMatch,
            MatchType::Stemmed => FactorType::FuzzyMatch,
            MatchType::Phonetic => FactorType::FuzzyMatch,
        }
    }

    /// 计算置信度分解
    fn compute_confidence_breakdown(
        dimension_details: &BTreeMap<String, DimensionAnalysisResult>,
    ) -> ConfidenceBreakdown {
        let mut dimension_confidences = BTreeMap::new();
        let mut overall_confidence = 0.0f32;
        let mut uncertainty_sources = Vec::new();

        for (dimension_id, details) in dimension_details {
            let evidence_strength = Self::calculate_evidence_strength(details);
            let ambiguity_score = Self::calculate_ambiguity_score(details);
            let confidence = evidence_strength * (1.0 - ambiguity_score);

            dimension_confidences.insert(
                dimension_id.clone(),
                DimensionConfidence {
                    dimension_id: dimension_id.clone(),
                    confidence,
                    evidence_strength,
                    ambiguity_score,
                },
            );

            overall_confidence += confidence;

            // 识别不确定性来源
            if ambiguity_score > 0.3 {
                uncertainty_sources.push(UncertaintySource {
                    source: format!("维度 '{}'", dimension_id),
                    impact: ambiguity_score,
                    description: format!("该维度存在多个可能的解释，模糊度较高"),
                });
            }
        }

        overall_confidence /= dimension_details.len() as f32;

        ConfidenceBreakdown {
            overall_confidence,
            dimension_confidences,
            uncertainty_sources,
        }
    }

    /// 计算证据强度
    fn calculate_evidence_strength(details: &DimensionAnalysisResult) -> f32 {
        let total_matches =
            details.high_matches.len() + details.medium_matches.len() + details.low_matches.len();
        let high_weight = details.high_matches.len() as f32 * 0.4;
        let medium_weight = details.medium_matches.len() as f32 * 0.3;
        let low_weight = details.low_matches.len() as f32 * 0.1;

        let evidence_score = high_weight + medium_weight + low_weight;
        (evidence_score / (total_matches.max(1) as f32)).min(1.0)
    }

    /// 计算模糊度分数
    fn calculate_ambiguity_score(details: &DimensionAnalysisResult) -> f32 {
        // 如果同时有高权重和低权重匹配，说明存在矛盾
        let contradiction_score =
            if !details.high_matches.is_empty() && !details.low_matches.is_empty() {
                0.3
            } else {
                0.0
            };

        // 模糊匹配增加不确定性
        let fuzzy_matches = details
            .high_matches
            .iter()
            .chain(details.medium_matches.iter())
            .filter(|m| matches!(m.match_type, MatchType::Fuzzy | MatchType::Phonetic))
            .count() as f32;

        let total_matches =
            (details.high_matches.len() + details.medium_matches.len()).max(1) as f32;
        let fuzzy_ratio = fuzzy_matches / total_matches;

        (contradiction_score + fuzzy_ratio * 0.2).min(1.0)
    }

    /// 生成替代可能性
    fn generate_alternatives(
        dimension_details: &BTreeMap<String, DimensionAnalysisResult>,
    ) -> Vec<AlternativePossibility> {
        let mut alternatives = Vec::new();

        for (dimension_id, details) in dimension_details {
            if details.final_score > 0.3 && details.final_score < 0.7 {
                // 中等分数的维度可能有替代解释
                alternatives.push(AlternativePossibility {
                    scenario: format!("维度 '{}' 可能被高估", dimension_id),
                    probability: 1.0 - details.final_score,
                    required_changes: vec![
                        "需要更多低权重关键词证据".to_string(),
                        "或者移除部分高权重匹配".to_string(),
                    ],
                });

                alternatives.push(AlternativePossibility {
                    scenario: format!("维度 '{}' 可能被低估", dimension_id),
                    probability: details.final_score,
                    required_changes: vec![
                        "需要更多高权重关键词证据".to_string(),
                        "或者更强的上下文支持".to_string(),
                    ],
                });
            }
        }

        alternatives.sort_by(|a, b| {
            b.probability
                .partial_cmp(&a.probability)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        alternatives.into_iter().take(3).collect() // 只保留前3个
    }

    /// 追踪决策路径
    fn trace_decision_path(
        input: &str,
        dimension_details: &BTreeMap<String, DimensionAnalysisResult>,
    ) -> DecisionPath {
        let mut steps = Vec::new();
        let mut critical_points = Vec::new();

        // 步骤1：输入解析
        steps.push(DecisionStep {
            step_number: 1,
            description: "解析用户输入".to_string(),
            input_state: "原始文本输入".to_string(),
            output_state: format!("标准化输入: '{}'", input),
            reasoning: "将输入转换为标准化格式，便于关键词匹配".to_string(),
        });

        // 步骤2：维度分析
        for (i, (dimension_id, details)) in dimension_details.iter().enumerate() {
            steps.push(DecisionStep {
                step_number: (i + 2) as u32,
                description: format!("分析维度 '{}'", dimension_id),
                input_state: format!("维度默认值: {:.2}", 0.5), // 假设默认值
                output_state: format!("最终得分: {:.2}", details.final_score),
                reasoning: format!(
                    "基于 {} 个匹配关键词的分析结果",
                    details.high_matches.len()
                        + details.medium_matches.len()
                        + details.low_matches.len()
                ),
            });

            // 识别关键决策点
            if details.final_score > 0.7 || details.final_score < 0.3 {
                critical_points.push(CriticalPoint {
                    location: format!("维度 '{}'", dimension_id),
                    decision: if details.final_score > 0.7 {
                        "高分评级"
                    } else {
                        "低分评级"
                    }
                    .to_string(),
                    alternatives: vec!["中等评级".to_string(), "需要更多证据".to_string()],
                    impact: (details.final_score - 0.5).abs(),
                });
            }
        }

        DecisionPath {
            steps,
            final_decision: "生成多维度标签向量".to_string(),
            critical_points,
        }
    }

    /// 确定主要原因
    fn determine_primary_reason(factors: &[ContributingFactor]) -> String {
        if let Some(primary_factor) = factors.first() {
            format!(
                "主要由{}决定：{}",
                match primary_factor.factor_type {
                    FactorType::ExactKeywordMatch => "精确关键词匹配",
                    FactorType::FuzzyMatch => "模糊匹配",
                    FactorType::SynonymMatch => "同义词匹配",
                    _ => "其他因素",
                },
                primary_factor.description
            )
        } else {
            "基于默认值，没有发现明确的匹配模式".to_string()
        }
    }

    /// 生成总结
    fn generate_summary(
        input: &str,
        primary_reason: &str,
        confidence: &ConfidenceBreakdown,
    ) -> String {
        format!(
            "对输入 '{}' 的分析结果：{}。整体置信度为 {:.1}%，基于 {} 个维度的综合分析。{}",
            input,
            primary_reason,
            confidence.overall_confidence * 100.0,
            confidence.dimension_confidences.len(),
            if confidence.overall_confidence > 0.7 {
                "分析结果具有较高可信度。"
            } else if confidence.overall_confidence > 0.4 {
                "分析结果具有中等可信度，建议结合更多上下文信息。"
            } else {
                "分析结果存在较大不确定性，建议提供更多明确的关键词。"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionAnalysisResult, MatchResult, MatchType};

    #[test]
    fn test_explanation_generation() {
        let mut dimension_details = BTreeMap::new();

        let creativity_result = DimensionAnalysisResult {
            dimension_id: "creativity_level".to_string(),
            final_score: 0.8,
            high_matches: vec![MatchResult {
                keyword: "create".to_string(),
                score: 1.0,
                match_type: MatchType::Exact,
                original_input: "create".to_string(),
            }],
            medium_matches: vec![],
            low_matches: vec![],
            explanation: "Test explanation".to_string(),
        };

        dimension_details.insert("creativity_level".to_string(), creativity_result);

        let explanation = ExplainableAnalyzer::generate_explanation(
            "I want to create something",
            &dimension_details,
        );

        assert!(!explanation.summary.is_empty());
        assert!(!explanation.contributing_factors.is_empty());
        assert!(explanation.confidence_breakdown.overall_confidence > 0.0);
        assert!(!explanation.decision_path.steps.is_empty());
    }

    #[test]
    fn test_evidence_strength_calculation() {
        let details = DimensionAnalysisResult {
            dimension_id: "test".to_string(),
            final_score: 0.5,
            high_matches: vec![MatchResult {
                keyword: "high1".to_string(),
                score: 1.0,
                match_type: MatchType::Exact,
                original_input: "input".to_string(),
            }],
            medium_matches: vec![],
            low_matches: vec![],
            explanation: "".to_string(),
        };

        let strength = ExplainableAnalyzer::calculate_evidence_strength(&details);
        assert!(strength > 0.0 && strength <= 1.0);
    }
}
