//! 动态权重学习和反馈优化模块

use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc, Duration as ChronoDuration};
use std::path::PathBuf;
use std::fs;

use crate::{TagVector, HybridAnalysisResult};

/// 动态学习管理器
pub struct DynamicLearningManager {
    workspace_root: PathBuf,
    weight_optimizer: WeightOptimizer,
    feedback_processor: FeedbackProcessor,
    performance_tracker: PerformanceTracker,
    config: DynamicLearningConfig,
}

/// 动态学习配置
#[derive(Debug, Clone)]
pub struct DynamicLearningConfig {
    pub learning_rate: f32,
    pub momentum: f32,
    pub decay_factor: f32,
    pub min_samples_for_update: usize,
    pub performance_window_size: usize,
    pub weight_update_frequency: ChronoDuration,
    pub enable_adaptive_learning_rate: bool,
    pub convergence_threshold: f32,
}

impl Default for DynamicLearningConfig {
    fn default() -> Self {
        Self {
            learning_rate: 0.01,
            momentum: 0.9,
            decay_factor: 0.95,
            min_samples_for_update: 10,
            performance_window_size: 100,
            weight_update_frequency: ChronoDuration::hours(1),
            enable_adaptive_learning_rate: true,
            convergence_threshold: 0.001,
        }
    }
}

/// 权重优化器
pub struct WeightOptimizer {
    current_weights: ComponentWeights,
    weight_history: VecDeque<WeightSnapshot>,
    gradient_accumulator: ComponentWeights,
    velocity: ComponentWeights, // For momentum
    last_update: DateTime<Utc>,
}

/// 组件权重配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentWeights {
    pub legacy_weight: f32,
    pub enhanced_weight: f32,
    pub vector_weight: f32,
    pub multipath_weight: f32,
    pub personalized_weight: f32,
    pub intent_influence: f32,
    pub context_boost: f32,
}

pub const DEFAULT_LEGACY_WEIGHT: f32 = 0.15;
pub const DEFAULT_ENHANCED_WEIGHT: f32 = 0.25;
pub const DEFAULT_VECTOR_WEIGHT: f32 = 0.30;
pub const DEFAULT_MULTIPATH_WEIGHT: f32 = 0.20;
pub const DEFAULT_PERSONALIZED_WEIGHT: f32 = 0.10;
pub const DEFAULT_INTENT_INFLUENCE: f32 = 0.15;
pub const DEFAULT_CONTEXT_BOOST: f32 = 0.10;

impl Default for ComponentWeights {
    fn default() -> Self {
        Self {
            legacy_weight: DEFAULT_LEGACY_WEIGHT,
            enhanced_weight: DEFAULT_ENHANCED_WEIGHT,
            vector_weight: DEFAULT_VECTOR_WEIGHT,
            multipath_weight: DEFAULT_MULTIPATH_WEIGHT,
            personalized_weight: DEFAULT_PERSONALIZED_WEIGHT,
            intent_influence: DEFAULT_INTENT_INFLUENCE,
            context_boost: DEFAULT_CONTEXT_BOOST,
        }
    }
}

/// 权重快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightSnapshot {
    pub timestamp: DateTime<Utc>,
    pub weights: ComponentWeights,
    pub performance_metrics: PerformanceMetrics,
    pub trigger_reason: String,
}

/// 反馈处理器
pub struct FeedbackProcessor {
    feedback_queue: VecDeque<ProcessedFeedback>,
    aggregated_feedback: HashMap<String, FeedbackStats>,
    last_processing: DateTime<Utc>,
}

/// 处理后的反馈
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedFeedback {
    pub timestamp: DateTime<Utc>,
    pub input: String,
    pub predicted_tags: TagVector,
    pub expected_tags: TagVector,
    pub user_satisfaction: f32, // 0.0 - 1.0
    pub error_vector: TagVector, // predicted - expected
    pub component_contributions: ComponentContributions,
    pub feedback_type: FeedbackType,
}

/// 组件贡献度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentContributions {
    pub legacy_contribution: f32,
    pub enhanced_contribution: f32,
    pub vector_contribution: f32,
    pub multipath_contribution: f32,
    pub personalized_contribution: f32,
    pub intent_adjustment: f32,
}

/// 反馈类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FeedbackType {
    Explicit,    // 用户明确提供反馈
    Implicit,    // 从用户行为推断的反馈
    System,      // 系统内部自动评估
    Correction,  // 用户纠错
}

/// 反馈统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackStats {
    pub total_feedback_count: usize,
    pub average_satisfaction: f32,
    pub error_distribution: HashMap<String, f32>, // dimension -> avg error
    pub improvement_trend: f32,
    pub last_updated: DateTime<Utc>,
}

/// 性能跟踪器
pub struct PerformanceTracker {
    performance_history: VecDeque<PerformanceRecord>,
    current_metrics: PerformanceMetrics,
    baseline_metrics: Option<PerformanceMetrics>,
}

/// 性能记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceRecord {
    pub timestamp: DateTime<Utc>,
    pub metrics: PerformanceMetrics,
    pub sample_size: usize,
    pub weights_used: ComponentWeights,
}

/// 性能指标
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub accuracy: f32,           // 整体准确度
    pub precision: f32,          // 精确度
    pub recall: f32,             // 召回率
    pub f1_score: f32,           // F1分数
    pub user_satisfaction: f32,  // 用户满意度
    pub response_time: f32,      // 响应时间(ms)
    pub dimension_accuracy: HashMap<String, f32>, // 各维度准确度
}

impl DynamicLearningManager {
    /// 创建动态学习管理器
    pub fn new(workspace_root: PathBuf, config: DynamicLearningConfig) -> Self {
        Self {
            workspace_root: workspace_root.clone(),
            weight_optimizer: WeightOptimizer::new(),
            feedback_processor: FeedbackProcessor::new(),
            performance_tracker: PerformanceTracker::new(),
            config,
        }
    }

    /// 初始化学习管理器
    pub fn initialize(&mut self) -> Result<(), String> {
        // 创建学习数据目录
        let learning_dir = self.workspace_root.join("learning_data");
        fs::create_dir_all(&learning_dir).map_err(|e| format!("创建学习目录失败: {}", e))?;

        // 加载历史权重
        if let Ok(weights) = self.load_saved_weights() {
            self.weight_optimizer.current_weights = weights;
        }

        // 加载历史性能数据
        self.load_performance_history()?;

        // 设置基线性能指标
        if self.performance_tracker.baseline_metrics.is_none() {
            self.performance_tracker.baseline_metrics = Some(PerformanceMetrics::default());
        }

        Ok(())
    }

    /// 处理用户反馈并更新权重
    pub fn process_feedback_and_learn(
        &mut self,
        input: &str,
        predicted_result: &HybridAnalysisResult,
        expected_tags: Option<&TagVector>,
        user_satisfaction: Option<f32>,
        feedback_type: FeedbackType,
    ) -> Result<LearningResult, String> {
        let start_time = std::time::Instant::now();

        // 1. 处理反馈数据
        let processed_feedback = self.process_feedback(
            input,
            predicted_result,
            expected_tags,
            user_satisfaction,
            feedback_type,
        )?;

        // 2. 更新性能统计
        let performance_update = self.update_performance_metrics(&processed_feedback);

        // 3. 检查是否需要权重更新
        let weight_update = if self.should_update_weights() {
            Some(self.update_weights()?)
        } else {
            None
        };

        // 4. 记录学习结果
        let learning_duration = start_time.elapsed();
        
        let result = LearningResult {
            feedback_processed: true,
            performance_improved: performance_update.improvement > 0.0,
            weights_updated: weight_update.is_some(),
            current_weights: self.weight_optimizer.current_weights.clone(),
            performance_change: performance_update.improvement,
            learning_rate_adjusted: weight_update.map(|u| u.learning_rate_adjusted).unwrap_or(false),
            processing_time: learning_duration,
            recommendations: self.generate_learning_recommendations(),
        };

        // 5. 保存学习状态
        self.save_learning_state()?;

        Ok(result)
    }

    /// 处理单个反馈
    fn process_feedback(
        &mut self,
        input: &str,
        predicted_result: &HybridAnalysisResult,
        expected_tags: Option<&TagVector>,
        user_satisfaction: Option<f32>,
        feedback_type: FeedbackType,
    ) -> Result<ProcessedFeedback, String> {
        let predicted_tags = &predicted_result.final_result;
        
        let expected_tags = expected_tags.unwrap_or(predicted_tags).clone();
        let satisfaction = user_satisfaction.unwrap_or(0.5);

        // 计算误差向量
        let error_vector = self.calculate_error_vector(predicted_tags, &expected_tags);

        // 分析各组件贡献度
        let contributions = self.analyze_component_contributions(predicted_result);

        let processed_feedback = ProcessedFeedback {
            timestamp: Utc::now(),
            input: input.to_string(),
            predicted_tags: predicted_tags.clone(),
            expected_tags,
            user_satisfaction: satisfaction,
            error_vector,
            component_contributions: contributions,
            feedback_type,
        };

        // 添加到反馈队列
        self.feedback_processor.feedback_queue.push_back(processed_feedback.clone());

        // 保持队列大小
        if self.feedback_processor.feedback_queue.len() > self.config.performance_window_size {
            self.feedback_processor.feedback_queue.pop_front();
        }

        Ok(processed_feedback)
    }

    /// 计算误差向量
    fn calculate_error_vector(&self, predicted: &TagVector, expected: &TagVector) -> TagVector {
        let mut error_vector = TagVector::new();
        
        // 收集所有维度
        let mut all_dimensions = std::collections::HashSet::new();
        for dim in predicted.dimensions.keys() {
            all_dimensions.insert(dim.clone());
        }
        for dim in expected.dimensions.keys() {
            all_dimensions.insert(dim.clone());
        }

        // 计算各维度误差
        for dimension in all_dimensions {
            let predicted_value = predicted.get(&dimension);
            let expected_value = expected.get(&dimension);
            let error = predicted_value - expected_value;
            error_vector.set(&dimension, error);
        }

        error_vector
    }

    /// 分析组件贡献度
    fn analyze_component_contributions(&self, result: &HybridAnalysisResult) -> ComponentContributions {
        // 基于融合策略分析各组件的实际贡献
        let weights = &self.weight_optimizer.current_weights;
        
        ComponentContributions {
            legacy_contribution: self.calculate_component_influence(&result.legacy_result, &result.final_result) * weights.legacy_weight,
            enhanced_contribution: self.calculate_component_influence(&result.enhanced_result, &result.final_result) * weights.enhanced_weight,
            vector_contribution: result.vector_result.as_ref()
                .map(|v| self.calculate_component_influence(&v.tag_vector, &result.final_result) * weights.vector_weight)
                .unwrap_or(0.0),
            multipath_contribution: result.multipath_result.as_ref()
                .map(|m| self.calculate_component_influence(&m.final_tag_vector, &result.final_result) * weights.multipath_weight)
                .unwrap_or(0.0),
            personalized_contribution: result.personalized_result.as_ref()
                .map(|p| self.calculate_component_influence(&p.personalized_vector, &result.final_result) * weights.personalized_weight)
                .unwrap_or(0.0),
            intent_adjustment: weights.intent_influence,
        }
    }

    /// 计算组件影响度
    fn calculate_component_influence(&self, component_result: &TagVector, final_result: &TagVector) -> f32 {
        // 使用余弦相似度衡量组件对最终结果的影响
        component_result.cosine_similarity(final_result)
    }

    /// 更新性能指标
    fn update_performance_metrics(&mut self, feedback: &ProcessedFeedback) -> PerformanceUpdate {
        // 计算当前样本的指标
        let sample_accuracy = self.calculate_accuracy(&feedback.predicted_tags, &feedback.expected_tags);
        let sample_satisfaction = feedback.user_satisfaction;

        // 更新累积指标
        let old_accuracy = self.performance_tracker.current_metrics.accuracy;
        let old_satisfaction = self.performance_tracker.current_metrics.user_satisfaction;

        // 使用指数移动平均更新指标
        let alpha = 0.1; // 平滑因子
        self.performance_tracker.current_metrics.accuracy = 
            old_accuracy * (1.0 - alpha) + sample_accuracy * alpha;
        self.performance_tracker.current_metrics.user_satisfaction = 
            old_satisfaction * (1.0 - alpha) + sample_satisfaction * alpha;

        // 计算其他指标
        self.update_precision_recall(feedback);
        self.update_dimension_accuracy(feedback);

        // 计算改进程度
        let improvement = (self.performance_tracker.current_metrics.accuracy - old_accuracy) +
                         (self.performance_tracker.current_metrics.user_satisfaction - old_satisfaction);

        PerformanceUpdate {
            improvement,
            current_accuracy: self.performance_tracker.current_metrics.accuracy,
            current_satisfaction: self.performance_tracker.current_metrics.user_satisfaction,
        }
    }

    /// 计算准确度
    fn calculate_accuracy(&self, predicted: &TagVector, expected: &TagVector) -> f32 {
        if expected.dimensions.is_empty() {
            return 1.0;
        }

        let mut total_error = 0.0f32;
        let mut dimension_count = 0;

        for (dimension, expected_value) in &expected.dimensions {
            let predicted_value = predicted.get(dimension);
            let error = (predicted_value - expected_value).abs();
            total_error += error;
            dimension_count += 1;
        }

        if dimension_count > 0 {
            1.0 - (total_error / dimension_count as f32)
        } else {
            1.0
        }
    }

    /// 更新精确度和召回率
    fn update_precision_recall(&mut self, feedback: &ProcessedFeedback) {
        // 简化的精确度和召回率计算
        let threshold = 0.5;
        
        let mut true_positives = 0;
        let mut false_positives = 0;
        let mut false_negatives = 0;

        for (dimension, expected_value) in &feedback.expected_tags.dimensions {
            let predicted_value = feedback.predicted_tags.get(dimension);
            
            let predicted_positive = predicted_value > threshold;
            let actual_positive = *expected_value > threshold;

            match (predicted_positive, actual_positive) {
                (true, true) => true_positives += 1,
                (true, false) => false_positives += 1,
                (false, true) => false_negatives += 1,
                (false, false) => {} // true_negatives
            }
        }

        if true_positives + false_positives > 0 {
            self.performance_tracker.current_metrics.precision = 
                true_positives as f32 / (true_positives + false_positives) as f32;
        }

        if true_positives + false_negatives > 0 {
            self.performance_tracker.current_metrics.recall = 
                true_positives as f32 / (true_positives + false_negatives) as f32;
        }

        // 计算F1分数
        let precision = self.performance_tracker.current_metrics.precision;
        let recall = self.performance_tracker.current_metrics.recall;
        if precision + recall > 0.0 {
            self.performance_tracker.current_metrics.f1_score = 
                2.0 * (precision * recall) / (precision + recall);
        }
    }

    /// 更新维度准确度
    fn update_dimension_accuracy(&mut self, feedback: &ProcessedFeedback) {
        for (dimension, expected_value) in &feedback.expected_tags.dimensions {
            let predicted_value = feedback.predicted_tags.get(dimension);
            let accuracy = 1.0 - (predicted_value - expected_value).abs();
            
            let current_accuracy = self.performance_tracker.current_metrics.dimension_accuracy
                .get(dimension).unwrap_or(&0.5);
            
            let alpha = 0.1;
            let updated_accuracy = current_accuracy * (1.0 - alpha) + accuracy * alpha;
            
            self.performance_tracker.current_metrics.dimension_accuracy
                .insert(dimension.clone(), updated_accuracy);
        }
    }

    /// 检查是否应该更新权重
    fn should_update_weights(&self) -> bool {
        let enough_samples = self.feedback_processor.feedback_queue.len() >= self.config.min_samples_for_update;
        let time_elapsed = Utc::now().signed_duration_since(self.weight_optimizer.last_update) >= self.config.weight_update_frequency;
        
        enough_samples && time_elapsed
    }

    /// 更新权重
    fn update_weights(&mut self) -> Result<WeightUpdateResult, String> {
        let start_time = std::time::Instant::now();

        // 计算权重梯度
        let gradients = self.calculate_weight_gradients()?;

        // 应用梯度下降更新
        let old_weights = self.weight_optimizer.current_weights.clone();
        self.apply_weight_updates(&gradients);

        // 检查收敛性
        let convergence_check = self.check_convergence(&old_weights);

        // 自适应学习率调整
        let learning_rate_adjusted = if self.config.enable_adaptive_learning_rate {
            self.adjust_learning_rate(convergence_check.converged)
        } else {
            false
        };

        // 创建权重快照
        let snapshot = WeightSnapshot {
            timestamp: Utc::now(),
            weights: self.weight_optimizer.current_weights.clone(),
            performance_metrics: self.performance_tracker.current_metrics.clone(),
            trigger_reason: "定期权重更新".to_string(),
        };

        self.weight_optimizer.weight_history.push_back(snapshot);
        if self.weight_optimizer.weight_history.len() > 50 {
            self.weight_optimizer.weight_history.pop_front();
        }

        self.weight_optimizer.last_update = Utc::now();

        Ok(WeightUpdateResult {
            weights_changed: !convergence_check.converged,
            convergence_achieved: convergence_check.converged,
            learning_rate_adjusted,
            gradient_magnitude: convergence_check.gradient_magnitude,
            update_time: start_time.elapsed(),
        })
    }

    /// 计算权重梯度
    fn calculate_weight_gradients(&self) -> Result<ComponentWeights, String> {
        let mut gradients = ComponentWeights::default();
        let mut gradient_count = 0;

        // 遍历最近的反馈计算梯度
        for feedback in &self.feedback_processor.feedback_queue {
            if feedback.user_satisfaction < 0.5 {
                // 对于低满意度的反馈，调整权重梯度
                let error_magnitude = self.calculate_error_magnitude(&feedback.error_vector);
                let contributions = &feedback.component_contributions;

                // 基于组件贡献度和误差计算梯度
                gradients.legacy_weight += -error_magnitude * contributions.legacy_contribution;
                gradients.enhanced_weight += -error_magnitude * contributions.enhanced_contribution;
                gradients.vector_weight += -error_magnitude * contributions.vector_contribution;
                gradients.multipath_weight += -error_magnitude * contributions.multipath_contribution;
                gradients.personalized_weight += -error_magnitude * contributions.personalized_contribution;

                gradient_count += 1;
            }
        }

        if gradient_count > 0 {
            // 归一化梯度
            let scale = 1.0 / gradient_count as f32;
            gradients.legacy_weight *= scale;
            gradients.enhanced_weight *= scale;
            gradients.vector_weight *= scale;
            gradients.multipath_weight *= scale;
            gradients.personalized_weight *= scale;
        }

        Ok(gradients)
    }

    /// 计算误差幅度
    fn calculate_error_magnitude(&self, error_vector: &TagVector) -> f32 {
        let sum_squares: f32 = error_vector.dimensions.values()
            .map(|error| error * error)
            .sum();
        
        if error_vector.dimensions.is_empty() {
            0.0
        } else {
            (sum_squares / error_vector.dimensions.len() as f32).sqrt()
        }
    }

    /// 应用权重更新
    fn apply_weight_updates(&mut self, gradients: &ComponentWeights) {
        let lr = self.config.learning_rate;
        let momentum = self.config.momentum;
        let decay = self.config.decay_factor;

        // 更新速度（动量）
        self.weight_optimizer.velocity.legacy_weight = 
            momentum * self.weight_optimizer.velocity.legacy_weight + lr * gradients.legacy_weight;
        self.weight_optimizer.velocity.enhanced_weight = 
            momentum * self.weight_optimizer.velocity.enhanced_weight + lr * gradients.enhanced_weight;
        self.weight_optimizer.velocity.vector_weight = 
            momentum * self.weight_optimizer.velocity.vector_weight + lr * gradients.vector_weight;
        self.weight_optimizer.velocity.multipath_weight = 
            momentum * self.weight_optimizer.velocity.multipath_weight + lr * gradients.multipath_weight;
        self.weight_optimizer.velocity.personalized_weight = 
            momentum * self.weight_optimizer.velocity.personalized_weight + lr * gradients.personalized_weight;

        // 应用权重更新
        self.weight_optimizer.current_weights.legacy_weight = 
            (self.weight_optimizer.current_weights.legacy_weight + self.weight_optimizer.velocity.legacy_weight) * decay;
        self.weight_optimizer.current_weights.enhanced_weight = 
            (self.weight_optimizer.current_weights.enhanced_weight + self.weight_optimizer.velocity.enhanced_weight) * decay;
        self.weight_optimizer.current_weights.vector_weight = 
            (self.weight_optimizer.current_weights.vector_weight + self.weight_optimizer.velocity.vector_weight) * decay;
        self.weight_optimizer.current_weights.multipath_weight = 
            (self.weight_optimizer.current_weights.multipath_weight + self.weight_optimizer.velocity.multipath_weight) * decay;
        self.weight_optimizer.current_weights.personalized_weight = 
            (self.weight_optimizer.current_weights.personalized_weight + self.weight_optimizer.velocity.personalized_weight) * decay;

        // 确保权重为正值并进行归一化
        self.normalize_weights();
    }

    /// 归一化权重
    fn normalize_weights(&mut self) {
        let weights = &mut self.weight_optimizer.current_weights;
        
        // 确保所有权重为正值
        weights.legacy_weight = weights.legacy_weight.max(0.01);
        weights.enhanced_weight = weights.enhanced_weight.max(0.01);
        weights.vector_weight = weights.vector_weight.max(0.01);
        weights.multipath_weight = weights.multipath_weight.max(0.01);
        weights.personalized_weight = weights.personalized_weight.max(0.01);

        // 归一化主要权重（确保总和为1.0）
        let total = weights.legacy_weight + weights.enhanced_weight + weights.vector_weight + 
                   weights.multipath_weight + weights.personalized_weight;
        
        if total > 0.0 {
            weights.legacy_weight /= total;
            weights.enhanced_weight /= total;
            weights.vector_weight /= total;
            weights.multipath_weight /= total;
            weights.personalized_weight /= total;
        }

        // 辅助权重限制在合理范围内
        weights.intent_influence = weights.intent_influence.clamp(0.0, 0.5);
        weights.context_boost = weights.context_boost.clamp(0.0, 0.3);
    }

    /// 检查收敛性
    fn check_convergence(&self, old_weights: &ComponentWeights) -> ConvergenceResult {
        let current_weights = &self.weight_optimizer.current_weights;
        
        let weight_changes = [
            (current_weights.legacy_weight - old_weights.legacy_weight).abs(),
            (current_weights.enhanced_weight - old_weights.enhanced_weight).abs(),
            (current_weights.vector_weight - old_weights.vector_weight).abs(),
            (current_weights.multipath_weight - old_weights.multipath_weight).abs(),
            (current_weights.personalized_weight - old_weights.personalized_weight).abs(),
        ];

        let gradient_magnitude: f32 = weight_changes.iter().map(|x| x * x).sum::<f32>().sqrt();
        let converged = gradient_magnitude < self.config.convergence_threshold;

        ConvergenceResult {
            converged,
            gradient_magnitude,
        }
    }

    /// 调整学习率
    fn adjust_learning_rate(&mut self, converged: bool) -> bool {
        // 如果收敛，降低学习率；如果发散，提高学习率
        if converged {
            self.config.learning_rate *= 0.95;
            self.config.learning_rate = self.config.learning_rate.max(0.001);
        } else {
            self.config.learning_rate *= 1.05;
            self.config.learning_rate = self.config.learning_rate.min(0.1);
        }
        true
    }

    /// 生成学习建议
    fn generate_learning_recommendations(&self) -> Vec<String> {
        let mut recommendations = Vec::new();
        let metrics = &self.performance_tracker.current_metrics;

        if metrics.accuracy < 0.7 {
            recommendations.push("系统准确度偏低，建议收集更多高质量反馈数据".to_string());
        }

        if metrics.user_satisfaction < 0.6 {
            recommendations.push("用户满意度需要提升，建议分析用户痛点".to_string());
        }

        if self.feedback_processor.feedback_queue.len() < self.config.min_samples_for_update {
            recommendations.push(format!("反馈样本不足，建议收集至少{}个反馈样本", self.config.min_samples_for_update));
        }

        if self.config.learning_rate < 0.005 {
            recommendations.push("学习率过低，权重更新可能过于缓慢".to_string());
        }

        if recommendations.is_empty() {
            recommendations.push("系统运行良好，继续收集反馈以优化性能".to_string());
        }

        recommendations
    }

    /// 获取当前权重
    pub fn get_current_weights(&self) -> &ComponentWeights {
        &self.weight_optimizer.current_weights
    }

    /// 获取性能指标
    pub fn get_performance_metrics(&self) -> &PerformanceMetrics {
        &self.performance_tracker.current_metrics
    }

    /// 获取学习统计
    pub fn get_learning_statistics(&self) -> LearningStatistics {
        LearningStatistics {
            total_feedback_processed: self.feedback_processor.feedback_queue.len(),
            current_weights: self.weight_optimizer.current_weights.clone(),
            performance_metrics: self.performance_tracker.current_metrics.clone(),
            weight_updates_count: self.weight_optimizer.weight_history.len(),
            last_weight_update: self.weight_optimizer.last_update,
            learning_rate: self.config.learning_rate,
        }
    }

    /// 加载保存的权重
    fn load_saved_weights(&self) -> Result<ComponentWeights, String> {
        let weights_file = self.workspace_root.join("learning_data/weights.json");
        if weights_file.exists() {
            let content = fs::read_to_string(&weights_file)
                .map_err(|e| format!("读取权重文件失败: {}", e))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("解析权重数据失败: {}", e))
        } else {
            Err("权重文件不存在".to_string())
        }
    }

    /// 加载性能历史
    fn load_performance_history(&mut self) -> Result<(), String> {
        let history_file = self.workspace_root.join("learning_data/performance_history.json");
        if history_file.exists() {
            let content = fs::read_to_string(&history_file)
                .map_err(|e| format!("读取性能历史失败: {}", e))?;
            let history: Vec<PerformanceRecord> = serde_json::from_str(&content)
                .map_err(|e| format!("解析性能历史失败: {}", e))?;
            
            for record in history {
                self.performance_tracker.performance_history.push_back(record);
            }
        }
        Ok(())
    }

    /// 保存学习状态
    fn save_learning_state(&self) -> Result<(), String> {
        let learning_dir = self.workspace_root.join("learning_data");
        
        // 保存权重
        let weights_file = learning_dir.join("weights.json");
        let weights_content = serde_json::to_string_pretty(&self.weight_optimizer.current_weights)
            .map_err(|e| format!("序列化权重失败: {}", e))?;
        fs::write(&weights_file, weights_content)
            .map_err(|e| format!("保存权重失败: {}", e))?;

        // 保存性能历史
        let history_file = learning_dir.join("performance_history.json");
        let history: Vec<_> = self.performance_tracker.performance_history.iter().collect();
        let history_content = serde_json::to_string_pretty(&history)
            .map_err(|e| format!("序列化性能历史失败: {}", e))?;
        fs::write(&history_file, history_content)
            .map_err(|e| format!("保存性能历史失败: {}", e))?;

        Ok(())
    }
}

impl WeightOptimizer {
    fn new() -> Self {
        Self {
            current_weights: ComponentWeights::default(),
            weight_history: VecDeque::new(),
            gradient_accumulator: ComponentWeights::default(),
            velocity: ComponentWeights::default(),
            last_update: Utc::now(),
        }
    }
}

impl FeedbackProcessor {
    fn new() -> Self {
        Self {
            feedback_queue: VecDeque::new(),
            aggregated_feedback: HashMap::new(),
            last_processing: Utc::now(),
        }
    }
}

impl PerformanceTracker {
    fn new() -> Self {
        Self {
            performance_history: VecDeque::new(),
            current_metrics: PerformanceMetrics::default(),
            baseline_metrics: None,
        }
    }
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        Self {
            accuracy: 0.5,
            precision: 0.5,
            recall: 0.5,
            f1_score: 0.5,
            user_satisfaction: 0.5,
            response_time: 100.0,
            dimension_accuracy: HashMap::new(),
        }
    }
}

impl ComponentWeights {
    /// 重置为默认值
    pub fn reset_to_defaults(&mut self) {
        *self = Self::default();
    }

    /// 应用权重衰减
    pub fn apply_decay(&mut self, decay_factor: f32) {
        self.legacy_weight *= decay_factor;
        self.enhanced_weight *= decay_factor;
        self.vector_weight *= decay_factor;
        self.multipath_weight *= decay_factor;
        self.personalized_weight *= decay_factor;
    }
}

/// 学习结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningResult {
    pub feedback_processed: bool,
    pub performance_improved: bool,
    pub weights_updated: bool,
    pub current_weights: ComponentWeights,
    pub performance_change: f32,
    pub learning_rate_adjusted: bool,
    pub processing_time: std::time::Duration,
    pub recommendations: Vec<String>,
}

/// 性能更新结果
#[derive(Debug, Clone)]
struct PerformanceUpdate {
    pub improvement: f32,
    pub current_accuracy: f32,
    pub current_satisfaction: f32,
}

/// 权重更新结果
#[derive(Debug, Clone)]
struct WeightUpdateResult {
    pub weights_changed: bool,
    pub convergence_achieved: bool,
    pub learning_rate_adjusted: bool,
    pub gradient_magnitude: f32,
    pub update_time: std::time::Duration,
}

/// 收敛结果
#[derive(Debug, Clone)]
struct ConvergenceResult {
    pub converged: bool,
    pub gradient_magnitude: f32,
}

/// 学习统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningStatistics {
    pub total_feedback_processed: usize,
    pub current_weights: ComponentWeights,
    pub performance_metrics: PerformanceMetrics,
    pub weight_updates_count: usize,
    pub last_weight_update: DateTime<Utc>,
    pub learning_rate: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_hybrid_result() -> HybridAnalysisResult {
        use crate::{HybridAnalysisResult, TagVector};
        
        let mut final_result = TagVector::new();
        final_result.set("creativity_level", 0.8);
        final_result.set("urgency", 0.6);

        let mut legacy_result = TagVector::new();
        legacy_result.set("creativity_level", 0.7);

        let mut enhanced_result = TagVector::new();
        enhanced_result.set("creativity_level", 0.85);

        HybridAnalysisResult {
            input: "test input".to_string(),
            final_result,
            legacy_result,
            enhanced_result,
            vector_result: None,
            multipath_result: None,
            personalized_result: None,
            fusion_strategy: "test".to_string(),
            confidence_score: 0.8,
            analysis_duration: std::time::Duration::from_millis(100),
        }
    }

    #[test]
    fn test_dynamic_learning_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = DynamicLearningConfig::default();
        let mut manager = DynamicLearningManager::new(temp_dir.path().to_path_buf(), config);
        
        assert!(manager.initialize().is_ok());
    }

    #[test]
    fn test_feedback_processing() {
        let temp_dir = TempDir::new().unwrap();
        let config = DynamicLearningConfig::default();
        let mut manager = DynamicLearningManager::new(temp_dir.path().to_path_buf(), config);
        
        manager.initialize().unwrap();
        
        let hybrid_result = create_test_hybrid_result();
        let mut expected_tags = TagVector::new();
        expected_tags.set("creativity_level", 0.9);
        
        let result = manager.process_feedback_and_learn(
            "test input",
            &hybrid_result,
            Some(&expected_tags),
            Some(0.8),
            FeedbackType::Explicit,
        );
        
        assert!(result.is_ok());
        let learning_result = result.unwrap();
        assert!(learning_result.feedback_processed);
        assert!(!learning_result.recommendations.is_empty());
    }

    #[test]
    fn test_weight_normalization() {
        let temp_dir = TempDir::new().unwrap();
        let config = DynamicLearningConfig::default();
        let mut manager = DynamicLearningManager::new(temp_dir.path().to_path_buf(), config);
        
        // 设置不正常的权重
        manager.weight_optimizer.current_weights.legacy_weight = -0.1;
        manager.weight_optimizer.current_weights.enhanced_weight = 2.0;
        
        manager.normalize_weights();
        
        let weights = &manager.weight_optimizer.current_weights;
        assert!(weights.legacy_weight >= 0.01);
        assert!(weights.enhanced_weight > 0.0);
        
        // 检查主要权重是否归一化
        let total = weights.legacy_weight + weights.enhanced_weight + weights.vector_weight + 
                   weights.multipath_weight + weights.personalized_weight;
        assert!((total - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_performance_metrics_update() {
        let temp_dir = TempDir::new().unwrap();
        let config = DynamicLearningConfig::default();
        let mut manager = DynamicLearningManager::new(temp_dir.path().to_path_buf(), config);
        manager.initialize().unwrap();
        
        let mut predicted = TagVector::new();
        predicted.set("creativity_level", 0.8);
        
        let mut expected = TagVector::new();
        expected.set("creativity_level", 0.9);
        
        let accuracy = manager.calculate_accuracy(&predicted, &expected);
        assert!(accuracy > 0.8); // 应该有较高的准确度，因为差异不大
        
        let error_vector = manager.calculate_error_vector(&predicted, &expected);
        assert_eq!(error_vector.get("creativity_level"), -0.1);
    }

    #[test]
    fn test_learning_statistics() {
        let temp_dir = TempDir::new().unwrap();
        let config = DynamicLearningConfig::default();
        let manager = DynamicLearningManager::new(temp_dir.path().to_path_buf(), config);
        
        let stats = manager.get_learning_statistics();
        assert_eq!(stats.total_feedback_processed, 0);
        assert!(stats.learning_rate > 0.0);
        assert_eq!(stats.weight_updates_count, 0);
    }
}