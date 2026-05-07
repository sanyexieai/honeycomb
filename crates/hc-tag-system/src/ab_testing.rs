//! A/B测试框架模块

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{AnalysisType, HybridAnalysisResult, MultimodalTagAnalysisResult, TagVector};

/// A/B测试管理器
pub struct ABTestingManager {
    experiments: HashMap<String, Experiment>,
    config: ABTestingConfig,
    results_store: ResultsStore,
    statistical_analyzer: StatisticalAnalyzer,
}

/// A/B测试配置
#[derive(Debug, Clone)]
pub struct ABTestingConfig {
    pub default_sample_size: usize,
    pub significance_level: f64, // α值，默认0.05
    pub power: f64,              // 统计功效，默认0.8
    pub min_effect_size: f64,    // 最小检测效应量
    pub max_experiment_duration: Duration,
    pub traffic_allocation_strategy: TrafficAllocationStrategy,
    pub auto_stop_on_significance: bool,
}

pub const DEFAULT_SAMPLE_SIZE: usize = 1000;
pub const DEFAULT_SIGNIFICANCE_LEVEL: f64 = 0.05;
pub const DEFAULT_STATISTICAL_POWER: f64 = 0.8;
pub const DEFAULT_MIN_EFFECT_SIZE: f64 = 0.1;
pub const DEFAULT_EXPERIMENT_DURATION_SECS: u64 = 30 * 24 * 3600;

impl Default for ABTestingConfig {
    fn default() -> Self {
        Self {
            default_sample_size: DEFAULT_SAMPLE_SIZE,
            significance_level: DEFAULT_SIGNIFICANCE_LEVEL,
            power: DEFAULT_STATISTICAL_POWER,
            min_effect_size: DEFAULT_MIN_EFFECT_SIZE,
            max_experiment_duration: Duration::from_secs(DEFAULT_EXPERIMENT_DURATION_SECS),
            traffic_allocation_strategy: TrafficAllocationStrategy::EqualSplit,
            auto_stop_on_significance: true,
        }
    }
}

/// 流量分配策略
#[derive(Debug, Clone)]
pub enum TrafficAllocationStrategy {
    EqualSplit,              // 均等分配
    WeightedSplit(Vec<f64>), // 权重分配
    AdaptiveSplit(f64),      // 自适应分配(基于性能)
    ThompsonSampling,        // 汤普森采样
}

/// 实验定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub name: String,
    pub description: String,
    pub variants: Vec<ExperimentVariant>,
    pub metrics: Vec<MetricDefinition>,
    pub status: ExperimentStatus,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub sample_size_per_variant: usize,
    pub traffic_allocation: Vec<f64>,
    pub hypothesis: String,
    pub success_criteria: SuccessCriteria,
}

/// 实验变体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentVariant {
    pub id: String,
    pub name: String,
    pub description: String,
    pub algorithm_config: AlgorithmConfig,
    pub is_control: bool,
}

/// 算法配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlgorithmConfig {
    Baseline,                                   // 基线算法
    FuzzyMatching(FuzzyConfig),                 // 模糊匹配配置
    VectorMatching(VectorConfig),               // 向量匹配配置
    HybridAnalysis(HybridConfig),               // 混合分析配置
    MultimodalAnalysis(MultimodalConfig),       // 多模态分析配置
    ReinforcementLearning(RLConfig),            // 强化学习配置
    Custom(HashMap<String, serde_json::Value>), // 自定义配置
}

/// 简化的配置结构（避免循环依赖）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuzzyConfig {
    pub enable_synonyms: bool,
    pub similarity_threshold: f32,
    pub enable_stemming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorConfig {
    pub embedding_model: String,
    pub similarity_threshold: f32,
    pub cache_embeddings: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridConfig {
    pub fuzzy_weight: f32,
    pub vector_weight: f32,
    pub context_weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultimodalConfig {
    pub enable_image_analysis: bool,
    pub enable_audio_analysis: bool,
    pub enable_document_analysis: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RLConfig {
    pub algorithm: String,
    pub learning_rate: f32,
    pub exploration_rate: f32,
}

/// 指标定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDefinition {
    pub name: String,
    pub metric_type: MetricType,
    pub aggregation: AggregationType,
    pub is_primary: bool,
    pub direction: MetricDirection, // 越高越好 or 越低越好
}

/// 指标类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetricType {
    Accuracy,         // 准确性
    Precision,        // 精确率
    Recall,           // 召回率
    F1Score,          // F1分数
    ResponseTime,     // 响应时间
    UserSatisfaction, // 用户满意度
    ClickThroughRate, // 点击率
    ConversionRate,   // 转换率
    Custom(String),   // 自定义指标
}

/// 聚合类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AggregationType {
    Mean,
    Median,
    Percentile(u8),
    Count,
    Rate,
}

/// 指标方向
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetricDirection {
    Higher, // 越高越好
    Lower,  // 越低越好
}

/// 成功标准
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessCriteria {
    pub primary_metric: String,
    pub min_improvement: f64,
    pub confidence_level: f64,
    pub min_sample_size: usize,
}

/// 实验状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExperimentStatus {
    Draft,     // 草稿
    Active,    // 进行中
    Paused,    // 暂停
    Completed, // 完成
    Cancelled, // 取消
}

/// 实验数据点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentDataPoint {
    pub experiment_id: String,
    pub variant_id: String,
    pub user_id: Option<String>,
    pub input: String,
    pub timestamp: DateTime<Utc>,
    pub metrics: HashMap<String, f64>,
    pub analysis_result: AnalysisResultSummary,
    pub execution_time: Duration,
}

/// 分析结果摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResultSummary {
    pub tag_vector: TagVector,
    pub confidence_score: f64,
    pub analysis_type: String,
    pub additional_data: HashMap<String, serde_json::Value>,
}

/// 结果存储
pub struct ResultsStore {
    data_points: VecDeque<ExperimentDataPoint>,
    max_storage: usize,
    aggregated_results: HashMap<String, VariantResults>,
}

/// 变体结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantResults {
    pub variant_id: String,
    pub sample_count: usize,
    pub metrics: HashMap<String, MetricStatistics>,
    pub last_updated: DateTime<Utc>,
}

/// 指标统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricStatistics {
    pub mean: f64,
    pub median: f64,
    pub std_dev: f64,
    pub min: f64,
    pub max: f64,
    pub percentiles: HashMap<u8, f64>, // P25, P50, P75, P90, P95, P99
    pub count: usize,
}

/// 统计分析器
pub struct StatisticalAnalyzer {
    significance_tests: HashMap<String, Box<dyn SignificanceTest>>,
}

/// 显著性检验接口
pub trait SignificanceTest: Send + Sync {
    fn test(&self, control: &[f64], treatment: &[f64]) -> SignificanceTestResult;
    fn name(&self) -> &str;
}

/// 显著性检验结果
#[derive(Debug, Clone)]
pub struct SignificanceTestResult {
    pub test_name: String,
    pub p_value: f64,
    pub is_significant: bool,
    pub confidence_interval: (f64, f64),
    pub effect_size: f64,
    pub power: Option<f64>,
}

/// t检验实现
pub struct TTest;

impl SignificanceTest for TTest {
    fn test(&self, control: &[f64], treatment: &[f64]) -> SignificanceTestResult {
        let (mean_control, var_control) = Self::calculate_stats(control);
        let (mean_treatment, var_treatment) = Self::calculate_stats(treatment);

        let n1 = control.len() as f64;
        let n2 = treatment.len() as f64;

        // 池化方差
        let pooled_var = ((n1 - 1.0) * var_control + (n2 - 1.0) * var_treatment) / (n1 + n2 - 2.0);
        let se = (pooled_var * (1.0 / n1 + 1.0 / n2)).sqrt();

        let t_stat = (mean_treatment - mean_control) / se;
        let df = n1 + n2 - 2.0;

        // 简化的p值计算（实际应该使用t分布）
        let p_value = 2.0 * (1.0 - Self::normal_cdf(t_stat.abs()));

        let effect_size = (mean_treatment - mean_control) / (pooled_var.sqrt());

        // 95%置信区间
        let t_critical = 1.96; // 简化，实际应该根据df查表
        let margin_error = t_critical * se;
        let ci_lower = (mean_treatment - mean_control) - margin_error;
        let ci_upper = (mean_treatment - mean_control) + margin_error;

        SignificanceTestResult {
            test_name: "t-test".to_string(),
            p_value,
            is_significant: p_value < 0.05,
            confidence_interval: (ci_lower, ci_upper),
            effect_size,
            power: None,
        }
    }

    fn name(&self) -> &str {
        "t-test"
    }
}

impl TTest {
    fn calculate_stats(data: &[f64]) -> (f64, f64) {
        let n = data.len() as f64;
        let mean = data.iter().sum::<f64>() / n;
        let variance = data.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (mean, variance)
    }

    fn normal_cdf(x: f64) -> f64 {
        // 简化的正态分布CDF近似
        0.5 * (1.0 + Self::erf(x / 2.0_f64.sqrt()))
    }

    fn erf(x: f64) -> f64 {
        // 误差函数的近似计算
        let a1 = 0.254829592;
        let a2 = -0.284496736;
        let a3 = 1.421413741;
        let a4 = -1.453152027;
        let a5 = 1.061405429;
        let p = 0.3275911;

        let sign = if x < 0.0 { -1.0 } else { 1.0 };
        let x = x.abs();

        let t = 1.0 / (1.0 + p * x);
        let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();

        sign * y
    }
}

/// Mann-Whitney U检验（非参数检验）
pub struct MannWhitneyTest;

impl SignificanceTest for MannWhitneyTest {
    fn test(&self, control: &[f64], treatment: &[f64]) -> SignificanceTestResult {
        let mut combined: Vec<(f64, usize)> = Vec::new();

        // 合并数据并标记组别
        for &val in control {
            combined.push((val, 0)); // 0 for control
        }
        for &val in treatment {
            combined.push((val, 1)); // 1 for treatment
        }

        // 排序
        combined.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        // 计算秩
        let mut ranks = vec![0.0; combined.len()];
        let mut i = 0;
        while i < combined.len() {
            let mut j = i;
            while j < combined.len() && combined[j].0 == combined[i].0 {
                j += 1;
            }
            let avg_rank = ((i + j + 1) as f64) / 2.0;
            for k in i..j {
                ranks[k] = avg_rank;
            }
            i = j;
        }

        // 计算U统计量
        let mut r1 = 0.0; // control组秩和
        for (idx, &(_, group)) in combined.iter().enumerate() {
            if group == 0 {
                r1 += ranks[idx];
            }
        }

        let n1 = control.len() as f64;
        let n2 = treatment.len() as f64;
        let u1 = r1 - n1 * (n1 + 1.0) / 2.0;
        let u2 = n1 * n2 - u1;
        let u = u1.min(u2);

        // 正态近似
        let mean_u = n1 * n2 / 2.0;
        let var_u = n1 * n2 * (n1 + n2 + 1.0) / 12.0;
        let z = (u - mean_u) / var_u.sqrt();

        let p_value = 2.0 * (1.0 - TTest::normal_cdf(z.abs()));

        // 效应量 (r = Z / sqrt(N))
        let effect_size = z.abs() / (n1 + n2).sqrt();

        SignificanceTestResult {
            test_name: "Mann-Whitney U".to_string(),
            p_value,
            is_significant: p_value < 0.05,
            confidence_interval: (0.0, 0.0), // 简化
            effect_size,
            power: None,
        }
    }

    fn name(&self) -> &str {
        "Mann-Whitney U"
    }
}

/// 实验报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentReport {
    pub experiment: Experiment,
    pub results_summary: ResultsSummary,
    pub statistical_tests: Vec<StatisticalTestResult>,
    pub recommendations: Vec<String>,
    pub confidence_level: f64,
    pub generated_at: DateTime<Utc>,
}

/// 结果摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultsSummary {
    pub total_samples: usize,
    pub variant_performance: HashMap<String, VariantPerformance>,
    pub winning_variant: Option<String>,
    pub performance_improvement: Option<f64>,
}

/// 变体性能
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPerformance {
    pub variant_id: String,
    pub sample_size: usize,
    pub primary_metric_value: f64,
    pub all_metrics: HashMap<String, f64>,
    pub confidence_intervals: HashMap<String, (f64, f64)>,
}

/// 统计检验结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatisticalTestResult {
    pub test_name: String,
    pub metric_name: String,
    pub p_value: f64,
    pub is_significant: bool,
    pub effect_size: f64,
    pub confidence_interval: (f64, f64),
}

impl ABTestingManager {
    /// 创建A/B测试管理器
    pub fn new(config: ABTestingConfig) -> Self {
        let mut statistical_analyzer = StatisticalAnalyzer {
            significance_tests: HashMap::new(),
        };

        // 注册统计检验方法
        statistical_analyzer
            .significance_tests
            .insert("t-test".to_string(), Box::new(TTest));
        statistical_analyzer
            .significance_tests
            .insert("mann-whitney".to_string(), Box::new(MannWhitneyTest));

        Self {
            experiments: HashMap::new(),
            config,
            results_store: ResultsStore {
                data_points: VecDeque::new(),
                max_storage: 100000,
                aggregated_results: HashMap::new(),
            },
            statistical_analyzer,
        }
    }

    /// 创建实验
    pub fn create_experiment(&mut self, mut experiment: Experiment) -> Result<String, String> {
        // 验证实验配置
        self.validate_experiment(&experiment)?;

        // 设置默认值
        if experiment.sample_size_per_variant == 0 {
            experiment.sample_size_per_variant = self.config.default_sample_size;
        }

        if experiment.traffic_allocation.is_empty() {
            experiment.traffic_allocation =
                vec![1.0 / experiment.variants.len() as f64; experiment.variants.len()];
        }

        experiment.status = ExperimentStatus::Draft;
        experiment.start_time = Utc::now();

        let experiment_id = experiment.id.clone();
        self.experiments.insert(experiment_id.clone(), experiment);

        Ok(experiment_id)
    }

    /// 开始实验
    pub fn start_experiment(&mut self, experiment_id: &str) -> Result<(), String> {
        let experiment = self
            .experiments
            .get_mut(experiment_id)
            .ok_or_else(|| format!("实验不存在: {}", experiment_id))?;

        if !matches!(
            experiment.status,
            ExperimentStatus::Draft | ExperimentStatus::Paused
        ) {
            return Err("只能启动草稿或暂停的实验".to_string());
        }

        experiment.status = ExperimentStatus::Active;
        experiment.start_time = Utc::now();

        Ok(())
    }

    /// 分配用户到实验变体
    pub fn assign_variant(
        &self,
        experiment_id: &str,
        user_id: Option<&str>,
    ) -> Result<String, String> {
        let experiment = self
            .experiments
            .get(experiment_id)
            .ok_or_else(|| format!("实验不存在: {}", experiment_id))?;

        if !matches!(experiment.status, ExperimentStatus::Active) {
            return Err("实验未激活".to_string());
        }

        // 根据配置的流量分配策略分配变体
        let variant_index = match &self.config.traffic_allocation_strategy {
            TrafficAllocationStrategy::EqualSplit => {
                self.hash_assignment(user_id, experiment_id, experiment.variants.len())
            }
            TrafficAllocationStrategy::WeightedSplit(weights) => {
                self.weighted_assignment(user_id, experiment_id, weights)
            }
            TrafficAllocationStrategy::AdaptiveSplit(_performance_threshold) => {
                // 基于性能的自适应分配（简化实现）
                self.adaptive_assignment(experiment_id)
            }
            TrafficAllocationStrategy::ThompsonSampling => {
                self.thompson_sampling_assignment(experiment_id)
            }
        };

        Ok(experiment.variants[variant_index].id.clone())
    }

    /// 记录实验数据
    pub fn record_data_point(&mut self, data_point: ExperimentDataPoint) -> Result<(), String> {
        // 验证数据点
        if !self.experiments.contains_key(&data_point.experiment_id) {
            return Err("实验不存在".to_string());
        }

        // 存储数据点
        self.results_store.data_points.push_back(data_point.clone());

        // 保持存储大小限制
        if self.results_store.data_points.len() > self.results_store.max_storage {
            self.results_store.data_points.pop_front();
        }

        // 更新聚合结果
        self.update_aggregated_results(&data_point);

        Ok(())
    }

    /// 分析实验结果
    pub fn analyze_experiment(&self, experiment_id: &str) -> Result<ExperimentReport, String> {
        let experiment = self
            .experiments
            .get(experiment_id)
            .ok_or_else(|| format!("实验不存在: {}", experiment_id))?;

        // 收集各变体的数据
        let variant_data = self.collect_variant_data(experiment_id)?;

        if variant_data.is_empty() {
            return Err("没有足够的数据进行分析".to_string());
        }

        // 执行统计检验
        let statistical_tests = self.perform_statistical_tests(experiment, &variant_data)?;

        // 生成结果摘要
        let results_summary =
            self.generate_results_summary(experiment, &variant_data, &statistical_tests);

        // 生成建议
        let recommendations = self.generate_recommendations(experiment, &statistical_tests);

        Ok(ExperimentReport {
            experiment: experiment.clone(),
            results_summary,
            statistical_tests,
            recommendations,
            confidence_level: 1.0 - self.config.significance_level,
            generated_at: Utc::now(),
        })
    }

    /// 停止实验
    pub fn stop_experiment(&mut self, experiment_id: &str) -> Result<(), String> {
        let experiment = self
            .experiments
            .get_mut(experiment_id)
            .ok_or_else(|| format!("实验不存在: {}", experiment_id))?;

        experiment.status = ExperimentStatus::Completed;
        experiment.end_time = Some(Utc::now());

        Ok(())
    }

    /// 获取实验状态
    pub fn get_experiment_status(&self, experiment_id: &str) -> Result<ExperimentStatus, String> {
        let experiment = self
            .experiments
            .get(experiment_id)
            .ok_or_else(|| format!("实验不存在: {}", experiment_id))?;
        Ok(experiment.status.clone())
    }

    /// 列出所有实验
    pub fn list_experiments(&self) -> Vec<&Experiment> {
        self.experiments.values().collect()
    }

    // 私有辅助方法

    fn validate_experiment(&self, experiment: &Experiment) -> Result<(), String> {
        if experiment.variants.len() < 2 {
            return Err("实验至少需要2个变体".to_string());
        }

        let control_count = experiment.variants.iter().filter(|v| v.is_control).count();
        if control_count != 1 {
            return Err("实验必须有且仅有一个控制组".to_string());
        }

        if experiment.metrics.is_empty() {
            return Err("实验必须定义至少一个指标".to_string());
        }

        let primary_metrics = experiment.metrics.iter().filter(|m| m.is_primary).count();
        if primary_metrics != 1 {
            return Err("实验必须有且仅有一个主要指标".to_string());
        }

        Ok(())
    }

    fn hash_assignment(
        &self,
        user_id: Option<&str>,
        experiment_id: &str,
        variant_count: usize,
    ) -> usize {
        let hash_input = format!("{}:{}", user_id.unwrap_or("anonymous"), experiment_id);

        // 简化的哈希函数
        let hash = hash_input
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));

        (hash % variant_count as u64) as usize
    }

    fn weighted_assignment(
        &self,
        user_id: Option<&str>,
        experiment_id: &str,
        weights: &[f64],
    ) -> usize {
        let hash_input = format!("{}:{}", user_id.unwrap_or("anonymous"), experiment_id);

        // 生成0-1之间的伪随机数
        let hash = hash_input
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let random_value = (hash % 10000) as f64 / 10000.0;

        // 根据权重分配
        let mut cumulative = 0.0;
        for (i, &weight) in weights.iter().enumerate() {
            cumulative += weight;
            if random_value < cumulative {
                return i;
            }
        }

        weights.len() - 1 // 容错
    }

    fn adaptive_assignment(&self, experiment_id: &str) -> usize {
        // 简化的自适应分配：选择当前表现最好的变体
        if let Ok(variant_data) = self.collect_variant_data(experiment_id) {
            if !variant_data.is_empty() {
                let experiment = &self.experiments[experiment_id];
                let primary_metric = experiment
                    .metrics
                    .iter()
                    .find(|m| m.is_primary)
                    .map(|m| &m.name);

                if let Some(metric_name) = primary_metric {
                    let mut best_variant = 0;
                    let mut best_value = f64::NEG_INFINITY;

                    for (i, (_variant_id, data)) in variant_data.iter().enumerate() {
                        if !data.is_empty() {
                            let avg_value = data.iter().sum::<f64>() / data.len() as f64;
                            if avg_value > best_value {
                                best_value = avg_value;
                                best_variant = i;
                            }
                        }
                    }

                    return best_variant;
                }
            }
        }

        // 如果没有数据，随机分配
        rand::thread_rng().gen_range(0..self.experiments[experiment_id].variants.len())
    }

    fn thompson_sampling_assignment(&self, experiment_id: &str) -> usize {
        // 简化的汤普森采样实现
        let experiment = &self.experiments[experiment_id];
        let variant_count = experiment.variants.len();

        // 为每个变体生成beta分布的采样值
        let mut rng = rand::thread_rng();
        let mut best_variant = 0;
        let mut best_sample = 0.0;

        for i in 0..variant_count {
            // 简化：使用固定的alpha和beta参数
            let alpha = 1.0;
            let beta = 1.0;
            let sample: f64 = rand::random::<f64>(); // 简化的beta采样

            if sample > best_sample {
                best_sample = sample;
                best_variant = i;
            }
        }

        best_variant
    }

    fn update_aggregated_results(&mut self, data_point: &ExperimentDataPoint) {
        let key = format!("{}:{}", data_point.experiment_id, data_point.variant_id);

        let variant_results = self
            .results_store
            .aggregated_results
            .entry(key)
            .or_insert_with(|| VariantResults {
                variant_id: data_point.variant_id.clone(),
                sample_count: 0,
                metrics: HashMap::new(),
                last_updated: Utc::now(),
            });

        variant_results.sample_count += 1;
        variant_results.last_updated = Utc::now();

        // 更新每个指标的统计信息
        for (metric_name, &value) in &data_point.metrics {
            let stats = variant_results
                .metrics
                .entry(metric_name.clone())
                .or_insert_with(|| MetricStatistics {
                    mean: 0.0,
                    median: 0.0,
                    std_dev: 0.0,
                    min: f64::INFINITY,
                    max: f64::NEG_INFINITY,
                    percentiles: HashMap::new(),
                    count: 0,
                });

            // 更新统计信息（简化实现）
            stats.count += 1;
            stats.mean = (stats.mean * (stats.count - 1) as f64 + value) / stats.count as f64;
            stats.min = stats.min.min(value);
            stats.max = stats.max.max(value);
        }
    }

    fn collect_variant_data(
        &self,
        experiment_id: &str,
    ) -> Result<HashMap<String, Vec<f64>>, String> {
        let experiment = self
            .experiments
            .get(experiment_id)
            .ok_or_else(|| format!("实验不存在: {}", experiment_id))?;

        let primary_metric = experiment
            .metrics
            .iter()
            .find(|m| m.is_primary)
            .ok_or("找不到主要指标")?;

        let mut variant_data: HashMap<String, Vec<f64>> = HashMap::new();

        // 初始化每个变体的数据容器
        for variant in &experiment.variants {
            variant_data.insert(variant.id.clone(), Vec::new());
        }

        // 收集数据点
        for data_point in &self.results_store.data_points {
            if data_point.experiment_id == experiment_id {
                if let Some(metric_value) = data_point.metrics.get(&primary_metric.name) {
                    if let Some(variant_metrics) = variant_data.get_mut(&data_point.variant_id) {
                        variant_metrics.push(*metric_value);
                    }
                }
            }
        }

        Ok(variant_data)
    }

    fn perform_statistical_tests(
        &self,
        experiment: &Experiment,
        variant_data: &HashMap<String, Vec<f64>>,
    ) -> Result<Vec<StatisticalTestResult>, String> {
        let mut results = Vec::new();

        // 找到控制组
        let control_variant = experiment
            .variants
            .iter()
            .find(|v| v.is_control)
            .ok_or("找不到控制组")?;

        let control_data = variant_data
            .get(&control_variant.id)
            .ok_or("控制组无数据")?;

        if control_data.is_empty() {
            return Err("控制组数据为空".to_string());
        }

        // 对每个处理组执行统计检验
        for variant in &experiment.variants {
            if !variant.is_control {
                if let Some(treatment_data) = variant_data.get(&variant.id) {
                    if !treatment_data.is_empty() {
                        // 执行t检验
                        let t_test = &self.statistical_analyzer.significance_tests["t-test"];
                        let test_result = t_test.test(control_data, treatment_data);

                        results.push(StatisticalTestResult {
                            test_name: test_result.test_name,
                            metric_name: experiment
                                .metrics
                                .iter()
                                .find(|m| m.is_primary)
                                .unwrap()
                                .name
                                .clone(),
                            p_value: test_result.p_value,
                            is_significant: test_result.is_significant,
                            effect_size: test_result.effect_size,
                            confidence_interval: test_result.confidence_interval,
                        });

                        // 如果数据不满足正态性假设，也执行非参数检验
                        let mann_whitney =
                            &self.statistical_analyzer.significance_tests["mann-whitney"];
                        let nonparam_result = mann_whitney.test(control_data, treatment_data);

                        results.push(StatisticalTestResult {
                            test_name: nonparam_result.test_name,
                            metric_name: experiment
                                .metrics
                                .iter()
                                .find(|m| m.is_primary)
                                .unwrap()
                                .name
                                .clone(),
                            p_value: nonparam_result.p_value,
                            is_significant: nonparam_result.is_significant,
                            effect_size: nonparam_result.effect_size,
                            confidence_interval: nonparam_result.confidence_interval,
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    fn generate_results_summary(
        &self,
        experiment: &Experiment,
        variant_data: &HashMap<String, Vec<f64>>,
        statistical_tests: &[StatisticalTestResult],
    ) -> ResultsSummary {
        let mut variant_performance = HashMap::new();
        let mut total_samples = 0;

        for variant in &experiment.variants {
            if let Some(data) = variant_data.get(&variant.id) {
                let sample_size = data.len();
                total_samples += sample_size;

                let primary_metric_value = if !data.is_empty() {
                    data.iter().sum::<f64>() / data.len() as f64
                } else {
                    0.0
                };

                let mut all_metrics = HashMap::new();
                all_metrics.insert("primary_metric".to_string(), primary_metric_value);

                variant_performance.insert(
                    variant.id.clone(),
                    VariantPerformance {
                        variant_id: variant.id.clone(),
                        sample_size,
                        primary_metric_value,
                        all_metrics,
                        confidence_intervals: HashMap::new(),
                    },
                );
            }
        }

        // 确定获胜变体
        let winning_variant = variant_performance
            .iter()
            .max_by(|a, b| {
                a.1.primary_metric_value
                    .partial_cmp(&b.1.primary_metric_value)
                    .unwrap()
            })
            .map(|(id, _)| id.clone());

        // 计算性能提升
        let control_variant = experiment.variants.iter().find(|v| v.is_control).unwrap();
        let performance_improvement = if let Some(winner_id) = &winning_variant {
            if winner_id != &control_variant.id {
                let control_perf = variant_performance
                    .get(&control_variant.id)
                    .map(|p| p.primary_metric_value)
                    .unwrap_or(0.0);
                let winner_perf = variant_performance
                    .get(winner_id)
                    .map(|p| p.primary_metric_value)
                    .unwrap_or(0.0);

                if control_perf > 0.0 {
                    Some((winner_perf - control_perf) / control_perf)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        ResultsSummary {
            total_samples,
            variant_performance,
            winning_variant,
            performance_improvement,
        }
    }

    fn generate_recommendations(
        &self,
        experiment: &Experiment,
        statistical_tests: &[StatisticalTestResult],
    ) -> Vec<String> {
        let mut recommendations = Vec::new();

        // 检查是否有显著性结果
        let has_significant = statistical_tests.iter().any(|t| t.is_significant);

        if has_significant {
            recommendations.push("发现统计学显著的差异".to_string());

            let best_test = statistical_tests
                .iter()
                .filter(|t| t.is_significant)
                .max_by(|a, b| {
                    a.effect_size
                        .abs()
                        .partial_cmp(&b.effect_size.abs())
                        .unwrap()
                });

            if let Some(test) = best_test {
                if test.effect_size > 0.2 {
                    recommendations.push("效应量较大，建议采用新算法".to_string());
                } else if test.effect_size > 0.1 {
                    recommendations.push("效应量中等，可以考虑采用新算法".to_string());
                } else {
                    recommendations.push("效应量较小，需要进一步验证".to_string());
                }
            }
        } else {
            recommendations.push("未发现统计学显著差异".to_string());
            recommendations.push("建议继续收集数据或调整实验设计".to_string());
        }

        // 检查样本量
        let total_samples: usize = statistical_tests.len() * 100; // 简化估算
        if total_samples < experiment.sample_size_per_variant * experiment.variants.len() {
            recommendations.push("样本量不足，建议继续收集数据".to_string());
        }

        // 检查实验持续时间
        let duration = Utc::now().signed_duration_since(experiment.start_time);
        if duration.num_days() < 7 {
            recommendations.push("实验时间较短，建议运行至少一周".to_string());
        }

        recommendations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ab_testing_manager_creation() {
        let config = ABTestingConfig::default();
        let manager = ABTestingManager::new(config);

        assert!(manager.experiments.is_empty());
        assert_eq!(manager.statistical_analyzer.significance_tests.len(), 2);
    }

    #[test]
    fn test_experiment_creation() {
        let mut manager = ABTestingManager::new(ABTestingConfig::default());

        let experiment = Experiment {
            id: "test_exp".to_string(),
            name: "Test Experiment".to_string(),
            description: "A test experiment".to_string(),
            variants: vec![
                ExperimentVariant {
                    id: "control".to_string(),
                    name: "Control".to_string(),
                    description: "Control group".to_string(),
                    algorithm_config: AlgorithmConfig::Baseline,
                    is_control: true,
                },
                ExperimentVariant {
                    id: "treatment".to_string(),
                    name: "Treatment".to_string(),
                    description: "Treatment group".to_string(),
                    algorithm_config: AlgorithmConfig::Custom(HashMap::new()),
                    is_control: false,
                },
            ],
            metrics: vec![MetricDefinition {
                name: "accuracy".to_string(),
                metric_type: MetricType::Accuracy,
                aggregation: AggregationType::Mean,
                is_primary: true,
                direction: MetricDirection::Higher,
            }],
            status: ExperimentStatus::Draft,
            start_time: Utc::now(),
            end_time: None,
            sample_size_per_variant: 1000,
            traffic_allocation: vec![0.5, 0.5],
            hypothesis: "New algorithm is better".to_string(),
            success_criteria: SuccessCriteria {
                primary_metric: "accuracy".to_string(),
                min_improvement: 0.05,
                confidence_level: 0.95,
                min_sample_size: 500,
            },
        };

        let experiment_id = manager.create_experiment(experiment).unwrap();
        assert_eq!(experiment_id, "test_exp");
        assert!(manager.experiments.contains_key("test_exp"));
    }

    #[test]
    fn test_variant_assignment() {
        let mut manager = ABTestingManager::new(ABTestingConfig::default());

        let experiment = create_test_experiment();
        let experiment_id = manager.create_experiment(experiment).unwrap();
        manager.start_experiment(&experiment_id).unwrap();

        // 测试相同用户的一致性分配
        let variant1 = manager
            .assign_variant(&experiment_id, Some("user1"))
            .unwrap();
        let variant2 = manager
            .assign_variant(&experiment_id, Some("user1"))
            .unwrap();
        assert_eq!(variant1, variant2);

        // 测试不同用户可能有不同分配
        let variant3 = manager
            .assign_variant(&experiment_id, Some("user2"))
            .unwrap();
        // variant3 可能等于或不等于 variant1，取决于哈希
    }

    #[test]
    fn test_statistical_tests() {
        let t_test = TTest;

        let control = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let treatment = vec![2.0, 3.0, 4.0, 5.0, 6.0];

        let result = t_test.test(&control, &treatment);

        assert_eq!(result.test_name, "t-test");
        assert!(result.effect_size > 0.0);
    }

    #[test]
    fn test_mann_whitney_test() {
        let mann_whitney = MannWhitneyTest;

        let control = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let treatment = vec![6.0, 7.0, 8.0, 9.0, 10.0];

        let result = mann_whitney.test(&control, &treatment);

        assert_eq!(result.test_name, "Mann-Whitney U");
        assert!(result.p_value < 0.05);
        assert!(result.is_significant);
    }

    fn create_test_experiment() -> Experiment {
        Experiment {
            id: "test_exp".to_string(),
            name: "Test Experiment".to_string(),
            description: "A test experiment".to_string(),
            variants: vec![
                ExperimentVariant {
                    id: "control".to_string(),
                    name: "Control".to_string(),
                    description: "Control group".to_string(),
                    algorithm_config: AlgorithmConfig::Baseline,
                    is_control: true,
                },
                ExperimentVariant {
                    id: "treatment".to_string(),
                    name: "Treatment".to_string(),
                    description: "Treatment group".to_string(),
                    algorithm_config: AlgorithmConfig::Custom(HashMap::new()),
                    is_control: false,
                },
            ],
            metrics: vec![MetricDefinition {
                name: "accuracy".to_string(),
                metric_type: MetricType::Accuracy,
                aggregation: AggregationType::Mean,
                is_primary: true,
                direction: MetricDirection::Higher,
            }],
            status: ExperimentStatus::Draft,
            start_time: Utc::now(),
            end_time: None,
            sample_size_per_variant: 100,
            traffic_allocation: vec![0.5, 0.5],
            hypothesis: "New algorithm is better".to_string(),
            success_criteria: SuccessCriteria {
                primary_metric: "accuracy".to_string(),
                min_improvement: 0.05,
                confidence_level: 0.95,
                min_sample_size: 50,
            },
        }
    }
}
