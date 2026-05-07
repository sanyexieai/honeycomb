//! 个性化模块 - 用户配置文件、偏好学习和自适应优化

use std::collections::HashMap;
use chrono::{DateTime, Utc, Duration};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::fs;

use crate::{TagVector, ContextAnalysisResult};

/// 个性化用户配置文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub user_id: String,
    pub created_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
    pub preferences: UserPreferences,
    pub learning_state: LearningState,
    pub usage_statistics: UsageStatistics,
    pub adaptation_history: Vec<AdaptationEvent>,
}

/// 用户偏好设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    // 维度偏好权重
    pub dimension_weights: HashMap<String, f32>,
    // 自定义关键词
    pub custom_keywords: HashMap<String, Vec<String>>,
    // 禁用的检测器
    pub disabled_detectors: Vec<String>,
    // 偏好的分析方法
    pub preferred_analysis_method: Option<String>,
    // 置信度阈值偏好
    pub confidence_threshold: f32,
    // 个性化设置
    pub personalization_level: PersonalizationLevel,
}

/// 个性化程度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PersonalizationLevel {
    Minimal,    // 最小个性化
    Moderate,   // 适度个性化
    Aggressive, // 积极个性化
}

/// 学习状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningState {
    // 学习进度
    pub learning_progress: f32,
    // 反馈样本数
    pub feedback_samples: u32,
    // 学习到的模式
    pub learned_patterns: Vec<LearnedPattern>,
    // 错误分类历史
    pub misclassification_history: Vec<MisclassificationRecord>,
    // 成功率趋势
    pub success_rate_trend: Vec<SuccessRatePoint>,
}

/// 学习到的模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedPattern {
    pub pattern_id: String,
    pub pattern_type: PatternType,
    pub confidence: f32,
    pub usage_count: u32,
    pub last_used: DateTime<Utc>,
    pub success_rate: f32,
}

/// 模式类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatternType {
    InputPattern(String),      // 输入模式（常用短语）
    DimensionCorrelation,      // 维度相关性
    ContextualPreference,      // 上下文偏好
    TemporalPattern,          // 时间模式
}

/// 错误分类记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MisclassificationRecord {
    pub timestamp: DateTime<Utc>,
    pub input: String,
    pub predicted_result: TagVector,
    pub actual_result: TagVector,
    pub user_feedback: UserFeedback,
    pub correction_applied: bool,
}

/// 用户反馈
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserFeedback {
    pub feedback_type: FeedbackType,
    pub rating: Option<f32>,           // 1-5分评级
    pub corrections: HashMap<String, f32>, // 维度修正
    pub comments: Option<String>,
}

/// 反馈类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FeedbackType {
    Positive,      // 正面反馈
    Negative,      // 负面反馈
    Correction,    // 纠正反馈
    Neutral,       // 中性反馈
}

/// 使用统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStatistics {
    pub total_analyses: u32,
    pub successful_analyses: u32,
    pub average_satisfaction: f32,
    pub preferred_dimensions: Vec<(String, u32)>, // 维度使用频次
    pub common_inputs: Vec<(String, u32)>,        // 常用输入模式
    pub peak_usage_hours: Vec<u32>,               // 使用高峰时段
}

/// 成功率时间点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessRatePoint {
    pub timestamp: DateTime<Utc>,
    pub success_rate: f32,
    pub sample_size: u32,
}

/// 适应事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptationEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: AdaptationType,
    pub description: String,
    pub impact_score: f32,
}

/// 适应类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdaptationType {
    WeightAdjustment,     // 权重调整
    KeywordLearning,      // 关键词学习
    PatternRecognition,   // 模式识别
    ThresholdOptimization, // 阈值优化
}

/// 个性化管理器
pub struct PersonalizationManager {
    workspace_root: PathBuf,
    user_profiles: HashMap<String, UserProfile>,
    learning_engine: LearningEngine,
    adaptation_engine: AdaptationEngine,
    config: PersonalizationConfig,
}

/// 个性化配置
#[derive(Debug, Clone)]
pub struct PersonalizationConfig {
    pub learning_rate: f32,
    pub adaptation_threshold: f32,
    pub max_patterns: usize,
    pub feedback_weight: f32,
    pub decay_factor: f32,
    pub min_samples_for_learning: u32,
}

impl Default for PersonalizationConfig {
    fn default() -> Self {
        Self {
            learning_rate: 0.1,
            adaptation_threshold: 0.3,
            max_patterns: 100,
            feedback_weight: 0.8,
            decay_factor: 0.95,
            min_samples_for_learning: 5,
        }
    }
}

impl PersonalizationManager {
    pub fn new(workspace_root: PathBuf, config: PersonalizationConfig) -> Self {
        Self {
            workspace_root: workspace_root.clone(),
            user_profiles: HashMap::new(),
            learning_engine: LearningEngine::new(),
            adaptation_engine: AdaptationEngine::new(),
            config,
        }
    }

    pub fn with_defaults(workspace_root: PathBuf) -> Self {
        Self::new(workspace_root, PersonalizationConfig::default())
    }

    /// 初始化，加载所有用户配置文件
    pub fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let profiles_dir = self.workspace_root.join("profiles");
        if !profiles_dir.exists() {
            fs::create_dir_all(&profiles_dir)?;
            return Ok(());
        }

        for entry in fs::read_dir(&profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(user_id) = path.file_stem().and_then(|s| s.to_str()) {
                    match self.load_user_profile(user_id) {
                        Ok(profile) => {
                            self.user_profiles.insert(user_id.to_string(), profile);
                        }
                        Err(e) => {
                            tracing::warn!(%user_id, error = %e, "failed to load user profile");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// 加载用户配置文件
    fn load_user_profile(&self, user_id: &str) -> Result<UserProfile, Box<dyn std::error::Error>> {
        let path = self.workspace_root.join("profiles").join(format!("{}.json", user_id));
        let content = fs::read_to_string(&path)?;
        let profile: UserProfile = serde_json::from_str(&content)?;
        Ok(profile)
    }

    /// 保存用户配置文件
    fn save_user_profile(&self, profile: &UserProfile) -> Result<(), Box<dyn std::error::Error>> {
        let profiles_dir = self.workspace_root.join("profiles");
        fs::create_dir_all(&profiles_dir)?;
        
        let path = profiles_dir.join(format!("{}.json", profile.user_id));
        let content = serde_json::to_string_pretty(profile)?;
        fs::write(&path, content)?;
        Ok(())
    }

    /// 获取或创建用户配置文件
    pub fn get_or_create_profile(&mut self, user_id: &str) -> &mut UserProfile {
        if !self.user_profiles.contains_key(user_id) {
            let new_profile = UserProfile {
                user_id: user_id.to_string(),
                created_at: Utc::now(),
                last_updated: Utc::now(),
                preferences: UserPreferences {
                    dimension_weights: HashMap::new(),
                    custom_keywords: HashMap::new(),
                    disabled_detectors: Vec::new(),
                    preferred_analysis_method: None,
                    confidence_threshold: 0.5,
                    personalization_level: PersonalizationLevel::Moderate,
                },
                learning_state: LearningState {
                    learning_progress: 0.0,
                    feedback_samples: 0,
                    learned_patterns: Vec::new(),
                    misclassification_history: Vec::new(),
                    success_rate_trend: Vec::new(),
                },
                usage_statistics: UsageStatistics {
                    total_analyses: 0,
                    successful_analyses: 0,
                    average_satisfaction: 0.0,
                    preferred_dimensions: Vec::new(),
                    common_inputs: Vec::new(),
                    peak_usage_hours: Vec::new(),
                },
                adaptation_history: Vec::new(),
            };
            
            self.user_profiles.insert(user_id.to_string(), new_profile);
        }
        
        self.user_profiles.get_mut(user_id).unwrap()
    }

    /// 个性化分析
    pub fn personalized_analysis(
        &mut self,
        user_id: &str,
        base_result: &TagVector,
        context: Option<&ContextAnalysisResult>
    ) -> PersonalizedResult {
        // 先获取需要的数据，避免借用冲突
        let (dimension_weights, learned_patterns, last_updated) = {
            let profile = self.get_or_create_profile(user_id);
            (
                profile.preferences.dimension_weights.clone(),
                profile.learning_state.learned_patterns.clone(),
                profile.last_updated,
            )
        };
        
        // 应用个性化权重
        let mut personalized_result = base_result.clone();
        for (dimension, weight) in &dimension_weights {
            if let Some(current_value) = personalized_result.dimensions.get_mut(dimension) {
                *current_value = (*current_value * weight).clamp(0.0, 1.0);
            }
        }

        // 应用学习到的模式
        let pattern_adjustment = self.learning_engine.apply_learned_patterns(
            &learned_patterns,
            base_result,
            context
        );
        
        personalized_result = personalized_result.weighted_merge(&pattern_adjustment, 0.2);

        // 计算个性化置信度
        let personalization_confidence = {
            let profile = self.get_or_create_profile(user_id);
            let feedback_factor = (profile.learning_state.feedback_samples as f32 / 50.0).min(1.0);
            let success_factor = if profile.usage_statistics.total_analyses > 0 {
                profile.usage_statistics.successful_analyses as f32 / profile.usage_statistics.total_analyses as f32
            } else {
                0.5
            };
            let learning_factor = profile.learning_state.learning_progress;
            (feedback_factor * 0.4 + success_factor * 0.4 + learning_factor * 0.2).clamp(0.0, 1.0)
        };

        PersonalizedResult {
            personalized_vector: personalized_result,
            personalization_confidence,
            applied_patterns: learned_patterns.iter()
                .filter(|p| p.last_used > Utc::now() - Duration::days(30))
                .map(|p| p.pattern_id.clone())
                .collect(),
            user_profile_version: last_updated,
        }
    }

    /// 处理用户反馈
    pub fn process_feedback(
        &mut self,
        user_id: &str,
        input: &str,
        predicted_result: &TagVector,
        feedback: UserFeedback
    ) -> FeedbackProcessResult {
        // 处理纠正反馈
        let corrected_result = if let FeedbackType::Correction = feedback.feedback_type {
            let mut corrected = predicted_result.clone();
            for (dimension, correction) in &feedback.corrections {
                corrected.set(dimension, *correction);
            }
            Some(corrected)
        } else {
            None
        };

        // 提取配置值避免借用冲突
        let learning_rate = self.config.learning_rate;
        let min_samples = self.config.min_samples_for_learning;
        
        // 更新用户状态并进行学习
        let learning_result = {
            let profile = self.get_or_create_profile(user_id);
            
            // 更新学习状态
            profile.learning_state.feedback_samples += 1;
            profile.last_updated = Utc::now();

            // 记录错误分类（如果是负面反馈）
            if matches!(feedback.feedback_type, FeedbackType::Negative | FeedbackType::Correction) {
                profile.learning_state.misclassification_history.push(MisclassificationRecord {
                    timestamp: Utc::now(),
                    input: input.to_string(),
                    predicted_result: predicted_result.clone(),
                    actual_result: corrected_result.clone().unwrap_or_else(|| predicted_result.clone()),
                    user_feedback: feedback.clone(),
                    correction_applied: false,
                });
            }

            // 简化版本的学习逻辑（避免借用检查问题）
            let mut patterns_updated = false;
            let mut should_adapt = false;
            let mut confidence_delta = 0.0f32;

            // 基于反馈类型调整学习进度
            match feedback.feedback_type {
                FeedbackType::Positive => {
                    profile.learning_state.learning_progress = (profile.learning_state.learning_progress + learning_rate * 0.5).min(1.0);
                    confidence_delta = 0.1;
                }
                FeedbackType::Negative | FeedbackType::Correction => {
                    profile.learning_state.learning_progress = (profile.learning_state.learning_progress + learning_rate).min(1.0);
                    confidence_delta = -0.05;
                    patterns_updated = true;
                    should_adapt = profile.learning_state.feedback_samples >= min_samples;
                }
                FeedbackType::Neutral => {
                    profile.learning_state.learning_progress = (profile.learning_state.learning_progress + learning_rate * 0.2).min(1.0);
                }
            }

            LearningResult {
                patterns_updated,
                should_adapt,
                confidence_delta,
            }
        };

        // 应用适应（分离操作）
        if learning_result.should_adapt {
            let profile = self.get_or_create_profile(user_id);
            
            // 简化适应逻辑
            if learning_result.patterns_updated {
                // 调整置信度阈值
                if learning_result.confidence_delta < 0.0 {
                    profile.preferences.confidence_threshold = (profile.preferences.confidence_threshold - 0.05).max(0.1);
                } else {
                    profile.preferences.confidence_threshold = (profile.preferences.confidence_threshold + 0.02).min(0.9);
                }

                profile.adaptation_history.push(AdaptationEvent {
                    timestamp: Utc::now(),
                    event_type: AdaptationType::ThresholdOptimization,
                    description: format!("调整置信度阈值到 {:.2}", profile.preferences.confidence_threshold),
                    impact_score: learning_result.confidence_delta.abs(),
                });
            }
        }

        // 保存配置文件
        if let Some(profile) = self.user_profiles.get(user_id) {
            if let Err(e) = self.save_user_profile(profile) {
                tracing::warn!(%user_id, error = %e, "failed to save user profile");
            }
        }

        FeedbackProcessResult {
            learning_applied: learning_result.patterns_updated,
            adaptation_applied: learning_result.should_adapt,
            confidence_change: learning_result.confidence_delta,
            corrected_result,
        }
    }

    /// 计算个性化置信度
    fn calculate_personalization_confidence(&self, profile: &UserProfile) -> f32 {
        let feedback_factor = (profile.learning_state.feedback_samples as f32 / 50.0).min(1.0);
        let success_factor = if profile.usage_statistics.total_analyses > 0 {
            profile.usage_statistics.successful_analyses as f32 / profile.usage_statistics.total_analyses as f32
        } else {
            0.5
        };
        let learning_factor = profile.learning_state.learning_progress;

        (feedback_factor * 0.4 + success_factor * 0.4 + learning_factor * 0.2).clamp(0.0, 1.0)
    }

    /// 获取用户统计信息
    pub fn get_user_statistics(&self, user_id: &str) -> Option<UserStatistics> {
        self.user_profiles.get(user_id).map(|profile| {
            UserStatistics {
                total_analyses: profile.usage_statistics.total_analyses,
                success_rate: if profile.usage_statistics.total_analyses > 0 {
                    profile.usage_statistics.successful_analyses as f32 / profile.usage_statistics.total_analyses as f32
                } else {
                    0.0
                },
                learning_progress: profile.learning_state.learning_progress,
                personalization_level: profile.preferences.personalization_level.clone(),
                adaptation_events: profile.adaptation_history.len() as u32,
                learned_patterns: profile.learning_state.learned_patterns.len() as u32,
            }
        })
    }
}

/// 个性化结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalizedResult {
    pub personalized_vector: TagVector,
    pub personalization_confidence: f32,
    pub applied_patterns: Vec<String>,
    pub user_profile_version: DateTime<Utc>,
}

/// 反馈处理结果
#[derive(Debug, Clone)]
pub struct FeedbackProcessResult {
    pub learning_applied: bool,
    pub adaptation_applied: bool,
    pub confidence_change: f32,
    pub corrected_result: Option<TagVector>,
}

/// 用户统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserStatistics {
    pub total_analyses: u32,
    pub success_rate: f32,
    pub learning_progress: f32,
    pub personalization_level: PersonalizationLevel,
    pub adaptation_events: u32,
    pub learned_patterns: u32,
}

/// 学习引擎
struct LearningEngine;

impl LearningEngine {
    fn new() -> Self {
        Self
    }

    fn apply_learned_patterns(
        &self,
        patterns: &[LearnedPattern],
        _base_result: &TagVector,
        _context: Option<&ContextAnalysisResult>
    ) -> TagVector {
        // 简化实现：基于模式应用调整
        let mut adjustment = TagVector::new();
        
        for pattern in patterns.iter().filter(|p| p.success_rate > 0.6) {
            match &pattern.pattern_type {
                PatternType::DimensionCorrelation => {
                    // 应用维度相关性调整
                    adjustment.set("creativity_level", pattern.confidence * 0.1);
                }
                _ => {
                    // 其他模式的默认调整
                }
            }
        }
        
        adjustment
    }

    fn learn_from_feedback(
        &self,
        learning_state: &mut LearningState,
        _input: &str,
        _predicted: &TagVector,
        feedback: &UserFeedback,
        config: &PersonalizationConfig,
    ) -> LearningResult {
        let mut patterns_updated = false;
        let mut should_adapt = false;
        let mut confidence_delta = 0.0f32;

        // 基于反馈类型调整学习进度
        match feedback.feedback_type {
            FeedbackType::Positive => {
                learning_state.learning_progress = (learning_state.learning_progress + config.learning_rate * 0.5).min(1.0);
                confidence_delta = 0.1;
            }
            FeedbackType::Negative | FeedbackType::Correction => {
                learning_state.learning_progress = (learning_state.learning_progress + config.learning_rate).min(1.0);
                confidence_delta = -0.05;
                patterns_updated = true;
                should_adapt = learning_state.feedback_samples >= config.min_samples_for_learning;
            }
            FeedbackType::Neutral => {
                learning_state.learning_progress = (learning_state.learning_progress + config.learning_rate * 0.2).min(1.0);
            }
        }

        LearningResult {
            patterns_updated,
            should_adapt,
            confidence_delta,
        }
    }
}

/// 学习结果
struct LearningResult {
    patterns_updated: bool,
    should_adapt: bool,
    confidence_delta: f32,
}

/// 适应引擎
struct AdaptationEngine;

impl AdaptationEngine {
    fn new() -> Self {
        Self
    }

    fn adapt_preferences(
        &self,
        preferences: &mut UserPreferences,
        learning_result: &LearningResult,
        _config: &PersonalizationConfig,
    ) -> AdaptationResult {
        // 简化实现：基于学习结果调整偏好
        if learning_result.patterns_updated {
            // 调整置信度阈值
            if learning_result.confidence_delta < 0.0 {
                preferences.confidence_threshold = (preferences.confidence_threshold - 0.05).max(0.1);
            } else {
                preferences.confidence_threshold = (preferences.confidence_threshold + 0.02).min(0.9);
            }

            AdaptationResult {
                adaptation_type: AdaptationType::ThresholdOptimization,
                description: format!("调整置信度阈值到 {:.2}", preferences.confidence_threshold),
                impact_score: learning_result.confidence_delta.abs(),
            }
        } else {
            AdaptationResult {
                adaptation_type: AdaptationType::WeightAdjustment,
                description: "无需调整".to_string(),
                impact_score: 0.0,
            }
        }
    }
}

/// 适应结果
struct AdaptationResult {
    adaptation_type: AdaptationType,
    description: String,
    impact_score: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_personalization_manager() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        
        let mut manager = PersonalizationManager::with_defaults(workspace);
        assert!(manager.initialize().is_ok());

        // 测试创建用户配置文件
        let profile = manager.get_or_create_profile("test_user");
        assert_eq!(profile.user_id, "test_user");
        assert_eq!(profile.learning_state.feedback_samples, 0);

        // 测试个性化分析
        let mut base_result = TagVector::new();
        base_result.set("creativity_level", 0.5);
        
        let personalized = manager.personalized_analysis("test_user", &base_result, None);
        assert!(personalized.personalization_confidence >= 0.0);
        assert!(personalized.personalization_confidence <= 1.0);
    }

    #[test]
    fn test_feedback_processing() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        
        let mut manager = PersonalizationManager::with_defaults(workspace);
        manager.initialize().unwrap();

        let mut predicted = TagVector::new();
        predicted.set("creativity_level", 0.3);

        let feedback = UserFeedback {
            feedback_type: FeedbackType::Correction,
            rating: Some(3.0),
            corrections: {
                let mut corrections = HashMap::new();
                corrections.insert("creativity_level".to_string(), 0.8);
                corrections
            },
            comments: Some("应该更有创造性".to_string()),
        };

        let result = manager.process_feedback(
            "test_user",
            "create innovative design",
            &predicted,
            feedback
        );

        assert!(result.corrected_result.is_some());
        let corrected = result.corrected_result.unwrap();
        assert_eq!(corrected.get("creativity_level"), 0.8);
    }

    #[test]
    fn test_user_statistics() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().to_path_buf();
        
        let mut manager = PersonalizationManager::with_defaults(workspace);
        manager.initialize().unwrap();

        // 创建用户并处理一些反馈
        let profile = manager.get_or_create_profile("test_user");
        profile.usage_statistics.total_analyses = 10;
        profile.usage_statistics.successful_analyses = 8;
        profile.learning_state.learning_progress = 0.6;

        let stats = manager.get_user_statistics("test_user").unwrap();
        assert_eq!(stats.total_analyses, 10);
        assert_eq!(stats.success_rate, 0.8);
        assert_eq!(stats.learning_progress, 0.6);
    }
}