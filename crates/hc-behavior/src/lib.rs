use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// 行为模式 - 定义系统的思考和执行方式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorPattern {
    /// 被动执行模式 - 严格按照指令执行，不做额外思考
    Passive,
    /// 稳定模式 - 使用已验证的方法，避免风险
    Stable,
    /// 学习模式 - 保守地尝试新功能，注重学习和改进
    Learning,
    /// 创造模式 - 注重创新，愿意尝试新方法
    Creative,
    /// 自适应模式 - 根据上下文自动选择最合适的模式
    Adaptive,
}

impl Default for BehaviorPattern {
    fn default() -> Self {
        Self::get_system_default()
    }
}

impl BehaviorPattern {
    /// 获取系统级别的默认行为模式
    /// 这是整个系统的单一配置点
    pub const fn get_system_default() -> Self {
        BehaviorPattern::Creative
    }
    
    /// 从字符串解析行为模式，失败时返回系统默认值
    pub fn from_str_or_default(s: &str) -> Self {
        Self::from_str(s).unwrap_or_else(|_| Self::get_system_default())
    }
    
    /// 从字符串解析行为模式
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "passive" | "被动" => Ok(BehaviorPattern::Passive),
            "stable" | "稳定" => Ok(BehaviorPattern::Stable),
            "learning" | "学习" => Ok(BehaviorPattern::Learning),
            "creative" | "创造" => Ok(BehaviorPattern::Creative),
            "adaptive" | "自适应" => Ok(BehaviorPattern::Adaptive),
            _ => Err(anyhow!("unknown behavior pattern: {} (available: passive, stable, learning, creative, adaptive)", s)),
        }
    }

    /// 获取模式的描述
    pub fn description(&self) -> &'static str {
        match self {
            BehaviorPattern::Passive => "严格按照指令执行，不做额外的推理或建议",
            BehaviorPattern::Stable => "使用经过验证的稳定方法，优先考虑可靠性",
            BehaviorPattern::Learning => "在稳定的基础上谨慎尝试新方法，注重学习和改进",
            BehaviorPattern::Creative => "积极尝试创新方法，追求最佳解决方案",
            BehaviorPattern::Adaptive => "根据情况自动选择最合适的行为模式",
        }
    }

    /// 获取模式的风险容忍度 (0.0 - 1.0)
    pub fn risk_tolerance(&self) -> f32 {
        match self {
            BehaviorPattern::Passive => 0.0,
            BehaviorPattern::Stable => 0.2,
            BehaviorPattern::Learning => 0.5,
            BehaviorPattern::Creative => 0.8,
            BehaviorPattern::Adaptive => 0.4, // 根据上下文调整
        }
    }

    /// 获取模式的创新倾向 (0.0 - 1.0)
    pub fn innovation_tendency(&self) -> f32 {
        match self {
            BehaviorPattern::Passive => 0.0,
            BehaviorPattern::Stable => 0.1,
            BehaviorPattern::Learning => 0.4,
            BehaviorPattern::Creative => 0.9,
            BehaviorPattern::Adaptive => 0.5,
        }
    }

    /// 获取模式的主动性 (0.0 - 1.0)
    pub fn proactivity(&self) -> f32 {
        match self {
            BehaviorPattern::Passive => 0.0,
            BehaviorPattern::Stable => 0.3,
            BehaviorPattern::Learning => 0.6,
            BehaviorPattern::Creative => 0.8,
            BehaviorPattern::Adaptive => 0.5,
        }
    }
}

/// 行为模式配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// 当前行为模式
    pub pattern: BehaviorPattern,
    /// 模式特定参数
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// 自动切换规则
    pub auto_switch_rules: Vec<PatternSwitchRule>,
    /// 思考深度 (1-10)
    pub thinking_depth: u8,
    /// 是否启用元认知
    pub enable_metacognition: bool,
    /// 学习率 (仅在学习和创造模式下有效)
    pub learning_rate: Option<f32>,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            pattern: BehaviorPattern::default(),
            parameters: BTreeMap::new(),
            auto_switch_rules: Vec::new(),
            thinking_depth: 3,
            enable_metacognition: false,
            learning_rate: Some(0.1),
        }
    }
}

impl BehaviorConfig {
    pub fn new(pattern: BehaviorPattern) -> Self {
        Self {
            pattern,
            ..Default::default()
        }
    }

    pub fn with_thinking_depth(mut self, depth: u8) -> Self {
        self.thinking_depth = depth.clamp(1, 10);
        self
    }

    pub fn with_metacognition(mut self, enable: bool) -> Self {
        self.enable_metacognition = enable;
        self
    }

    pub fn with_learning_rate(mut self, rate: f32) -> Self {
        self.learning_rate = Some(rate.clamp(0.0, 1.0));
        self
    }

    pub fn with_parameter(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.parameters.insert(key.into(), value);
        self
    }
}

/// 模式切换规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternSwitchRule {
    pub id: String,
    pub condition: SwitchCondition,
    pub target_pattern: BehaviorPattern,
    pub priority: u8, // 0-255, 数值越高优先级越高
    pub cooldown_seconds: Option<u64>,
}

/// 切换条件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SwitchCondition {
    /// 基于关键词匹配
    KeywordMatch {
        keywords: Vec<String>,
        case_sensitive: bool,
    },
    /// 基于任务复杂度
    TaskComplexity {
        min_complexity: f32,
        max_complexity: Option<f32>,
    },
    /// 基于错误率
    ErrorRate {
        threshold: f32,
        time_window_minutes: u32,
    },
    /// 基于用户反馈
    UserFeedback {
        positive_threshold: f32,
    },
    /// 基于时间
    TimeBasedSchedule {
        start_hour: u8,
        end_hour: u8,
        days_of_week: Vec<u8>, // 0=Sunday, 1=Monday, ...
    },
    /// 自定义条件表达式
    Expression {
        expression: String,
    },
}

/// 行为上下文 - 影响模式选择的环境信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorContext {
    /// 当前用户ID
    pub user_id: Option<String>,
    /// 当前会话ID
    pub session_id: Option<String>,
    /// 当前房间ID
    pub room_id: Option<String>,
    /// 任务类型
    pub task_type: Option<String>,
    /// 预估复杂度 (0.0-1.0)
    pub estimated_complexity: Option<f32>,
    /// 历史成功率
    pub historical_success_rate: Option<f32>,
    /// 可用的工具数量
    pub available_tools_count: Option<u32>,
    /// 时间压力 (0.0-1.0)
    pub time_pressure: Option<f32>,
    /// 用户偏好
    pub user_preferences: BTreeMap<String, serde_json::Value>,
    /// 环境变量
    pub environment: BTreeMap<String, String>,
}

impl Default for BehaviorContext {
    fn default() -> Self {
        Self {
            user_id: None,
            session_id: None,
            room_id: None,
            task_type: None,
            estimated_complexity: None,
            historical_success_rate: None,
            available_tools_count: None,
            time_pressure: None,
            user_preferences: BTreeMap::new(),
            environment: BTreeMap::new(),
        }
    }
}

/// 决策记录 - 记录系统的思考过程和决策依据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub context: BehaviorContext,
    pub behavior_pattern: BehaviorPattern,
    pub decision_type: DecisionType,
    pub options_considered: Vec<DecisionOption>,
    pub chosen_option: String,
    pub reasoning: String,
    pub confidence: f32, // 0.0-1.0
    pub execution_result: Option<ExecutionResult>,
}

impl DecisionRecord {
    pub fn new(
        context: BehaviorContext,
        behavior_pattern: BehaviorPattern,
        decision_type: DecisionType,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            context,
            behavior_pattern,
            decision_type,
            options_considered: Vec::new(),
            chosen_option: String::new(),
            reasoning: String::new(),
            confidence: 0.0,
            execution_result: None,
        }
    }

    pub fn with_options(mut self, options: Vec<DecisionOption>) -> Self {
        self.options_considered = options;
        self
    }

    pub fn with_choice(mut self, option_id: String, reasoning: String, confidence: f32) -> Self {
        self.chosen_option = option_id;
        self.reasoning = reasoning;
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }
}

/// 决策类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionType {
    /// 工具选择
    ToolSelection,
    /// 策略选择
    StrategySelection,
    /// 响应风格
    ResponseStyle,
    /// 能力创建
    CapabilityCreation,
    /// 模式切换
    PatternSwitch,
    /// 其他决策
    Other(String),
}

/// 决策选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionOption {
    pub id: String,
    pub description: String,
    pub pros: Vec<String>,
    pub cons: Vec<String>,
    pub estimated_effort: Option<f32>,
    pub success_probability: Option<f32>,
    pub innovation_level: Option<f32>,
    pub risk_level: Option<f32>,
}

impl DecisionOption {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            pros: Vec::new(),
            cons: Vec::new(),
            estimated_effort: None,
            success_probability: None,
            innovation_level: None,
            risk_level: None,
        }
    }

    pub fn with_pros(mut self, pros: Vec<String>) -> Self {
        self.pros = pros;
        self
    }

    pub fn with_cons(mut self, cons: Vec<String>) -> Self {
        self.cons = cons;
        self
    }

    pub fn with_metrics(
        mut self, 
        effort: f32, 
        success_prob: f32, 
        innovation: f32, 
        risk: f32
    ) -> Self {
        self.estimated_effort = Some(effort.clamp(0.0, 1.0));
        self.success_probability = Some(success_prob.clamp(0.0, 1.0));
        self.innovation_level = Some(innovation.clamp(0.0, 1.0));
        self.risk_level = Some(risk.clamp(0.0, 1.0));
        self
    }

    pub fn with_estimated_effort(mut self, effort: Option<f32>) -> Self {
        self.estimated_effort = effort;
        self
    }

    pub fn with_success_probability(mut self, prob: Option<f32>) -> Self {
        self.success_probability = prob;
        self
    }

    pub fn with_innovation_level(mut self, level: Option<f32>) -> Self {
        self.innovation_level = level;
        self
    }

    pub fn with_risk_level(mut self, risk: Option<f32>) -> Self {
        self.risk_level = risk;
        self
    }
}

/// 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub execution_time_ms: u64,
    pub output: Option<String>,
    pub error: Option<String>,
    pub user_satisfaction: Option<f32>, // 用户反馈 0.0-1.0
    pub learned_insights: Vec<String>,
}

/// 行为模式引擎 - 核心决策和执行逻辑
#[derive(Debug, Clone)]
pub struct BehaviorEngine {
    pub config: BehaviorConfig,
    pub context: BehaviorContext,
    decision_history: Vec<DecisionRecord>,
}

impl BehaviorEngine {
    pub fn new(config: BehaviorConfig, context: BehaviorContext) -> Self {
        Self {
            config,
            context,
            decision_history: Vec::new(),
        }
    }

    /// 根据当前模式和上下文做出决策
    pub fn make_decision(
        &mut self,
        decision_type: DecisionType,
        options: Vec<DecisionOption>,
    ) -> Result<DecisionRecord> {
        let mut decision = DecisionRecord::new(
            self.context.clone(),
            self.config.pattern.clone(),
            decision_type,
        ).with_options(options.clone());

        let chosen_option = match self.config.pattern {
            BehaviorPattern::Passive => self.passive_decision(&options)?,
            BehaviorPattern::Stable => self.stable_decision(&options)?,
            BehaviorPattern::Learning => self.learning_decision(&options)?,
            BehaviorPattern::Creative => self.creative_decision(&options)?,
            BehaviorPattern::Adaptive => self.adaptive_decision(&options)?,
        };

        decision = decision.with_choice(
            chosen_option.id.clone(),
            self.generate_reasoning(&chosen_option, &self.config.pattern),
            self.calculate_confidence(&chosen_option),
        );

        self.decision_history.push(decision.clone());
        
        // 限制历史记录数量
        if self.decision_history.len() > 1000 {
            self.decision_history.remove(0);
        }

        Ok(decision)
    }

    /// 被动模式决策 - 选择最直接、风险最低的选项
    fn passive_decision<'a>(&self, options: &'a [DecisionOption]) -> Result<&'a DecisionOption> {
        options.iter()
            .min_by(|a, b| {
                let risk_a = a.risk_level.unwrap_or(0.5);
                let risk_b = b.risk_level.unwrap_or(0.5);
                risk_a.partial_cmp(&risk_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| anyhow!("No options available"))
    }

    /// 稳定模式决策 - 平衡风险和收益，优选已验证的方法
    fn stable_decision<'a>(&self, options: &'a [DecisionOption]) -> Result<&'a DecisionOption> {
        options.iter()
            .max_by(|a, b| {
                let score_a = self.calculate_stability_score(a);
                let score_b = self.calculate_stability_score(b);
                score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| anyhow!("No options available"))
    }

    /// 学习模式决策 - 在稳定的基础上适度创新
    fn learning_decision<'a>(&self, options: &'a [DecisionOption]) -> Result<&'a DecisionOption> {
        options.iter()
            .max_by(|a, b| {
                let score_a = self.calculate_learning_score(a);
                let score_b = self.calculate_learning_score(b);
                score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| anyhow!("No options available"))
    }

    /// 创造模式决策 - 追求创新和最优解
    fn creative_decision<'a>(&self, options: &'a [DecisionOption]) -> Result<&'a DecisionOption> {
        options.iter()
            .max_by(|a, b| {
                let score_a = self.calculate_creative_score(a);
                let score_b = self.calculate_creative_score(b);
                score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| anyhow!("No options available"))
    }

    /// 自适应模式决策 - 根据上下文选择最合适的策略
    fn adaptive_decision<'a>(&self, options: &'a [DecisionOption]) -> Result<&'a DecisionOption> {
        let strategy = self.determine_adaptive_strategy();
        match strategy {
            BehaviorPattern::Passive => self.passive_decision(options),
            BehaviorPattern::Stable => self.stable_decision(options),
            BehaviorPattern::Learning => self.learning_decision(options),
            BehaviorPattern::Creative => self.creative_decision(options),
            BehaviorPattern::Adaptive => self.stable_decision(options), // 默认回退
        }
    }

    /// 计算稳定性得分
    fn calculate_stability_score(&self, option: &DecisionOption) -> f32 {
        let success_prob = option.success_probability.unwrap_or(0.5);
        let risk = option.risk_level.unwrap_or(0.5);
        let effort = option.estimated_effort.unwrap_or(0.5);
        
        // 稳定模式重视成功概率，避免高风险和高成本
        success_prob * 0.5 + (1.0 - risk) * 0.3 + (1.0 - effort) * 0.2
    }

    /// 计算学习得分
    fn calculate_learning_score(&self, option: &DecisionOption) -> f32 {
        let success_prob = option.success_probability.unwrap_or(0.5);
        let innovation = option.innovation_level.unwrap_or(0.3);
        let risk = option.risk_level.unwrap_or(0.5);
        
        // 学习模式平衡稳定性和创新性
        success_prob * 0.4 + innovation * 0.3 + (1.0 - risk) * 0.3
    }

    /// 计算创造得分
    fn calculate_creative_score(&self, option: &DecisionOption) -> f32 {
        let innovation = option.innovation_level.unwrap_or(0.3);
        let success_prob = option.success_probability.unwrap_or(0.5);
        
        // 创造模式重视创新程度，对风险有较高容忍度
        innovation * 0.6 + success_prob * 0.4
    }

    /// 确定自适应策略
    fn determine_adaptive_strategy(&self) -> BehaviorPattern {
        // 基于上下文决定使用哪种策略
        if let Some(complexity) = self.context.estimated_complexity {
            if complexity > 0.8 {
                return BehaviorPattern::Creative; // 复杂任务需要创新
            } else if complexity < 0.3 {
                return BehaviorPattern::Stable; // 简单任务保持稳定
            }
        }

        if let Some(success_rate) = self.context.historical_success_rate {
            if success_rate < 0.5 {
                return BehaviorPattern::Learning; // 成功率低需要学习
            }
        }

        if let Some(time_pressure) = self.context.time_pressure {
            if time_pressure > 0.7 {
                return BehaviorPattern::Stable; // 时间紧迫选择稳定方案
            }
        }

        // 默认使用学习模式
        BehaviorPattern::Learning
    }

    /// 生成决策推理说明
    fn generate_reasoning(&self, option: &DecisionOption, pattern: &BehaviorPattern) -> String {
        match pattern {
            BehaviorPattern::Passive => {
                format!("选择 '{}' 因为这是最直接的执行方式，风险最低。", option.description)
            },
            BehaviorPattern::Stable => {
                format!(
                    "选择 '{}' 基于稳定性考虑：成功概率 {:.1}%，风险水平 {:.1}%。这是经过验证的可靠方案。",
                    option.description,
                    option.success_probability.unwrap_or(0.5) * 100.0,
                    option.risk_level.unwrap_or(0.5) * 100.0
                )
            },
            BehaviorPattern::Learning => {
                format!(
                    "选择 '{}' 用于学习和改进：在保证 {:.1}% 成功率的基础上，引入 {:.1}% 的创新元素。",
                    option.description,
                    option.success_probability.unwrap_or(0.5) * 100.0,
                    option.innovation_level.unwrap_or(0.3) * 100.0
                )
            },
            BehaviorPattern::Creative => {
                format!(
                    "选择 '{}' 以追求最佳解决方案：创新程度 {:.1}%，虽有一定风险但潜在收益最大。",
                    option.description,
                    option.innovation_level.unwrap_or(0.3) * 100.0
                )
            },
            BehaviorPattern::Adaptive => {
                format!(
                    "选择 '{}' 基于当前上下文的自适应分析，这是在当前情况下的最优选择。",
                    option.description
                )
            },
        }
    }

    /// 计算决策信心度
    fn calculate_confidence(&self, option: &DecisionOption) -> f32 {
        let base_confidence = match self.config.pattern {
            BehaviorPattern::Passive => 0.9, // 被动模式很确定
            BehaviorPattern::Stable => 0.8,   // 稳定模式较确定
            BehaviorPattern::Learning => 0.6, // 学习模式适度确定
            BehaviorPattern::Creative => 0.5, // 创造模式不太确定
            BehaviorPattern::Adaptive => 0.7, // 自适应模式较确定
        };

        let success_factor = option.success_probability.unwrap_or(0.5);
        let risk_factor = 1.0 - option.risk_level.unwrap_or(0.5);
        
        (base_confidence + success_factor + risk_factor) / 3.0
    }

    /// 更新行为配置
    pub fn update_config(&mut self, config: BehaviorConfig) {
        self.config = config;
    }

    /// 更新上下文
    pub fn update_context(&mut self, context: BehaviorContext) {
        self.context = context;
    }

    /// 获取决策历史
    pub fn get_decision_history(&self) -> &[DecisionRecord] {
        &self.decision_history
    }

    /// 评估当前模式的有效性
    pub fn evaluate_pattern_effectiveness(&self) -> PatternEffectiveness {
        let recent_decisions = self.decision_history.iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>();

        if recent_decisions.is_empty() {
            return PatternEffectiveness::default();
        }

        let avg_confidence = recent_decisions.iter()
            .map(|d| d.confidence)
            .sum::<f32>() / recent_decisions.len() as f32;

        let success_rate = recent_decisions.iter()
            .filter_map(|d| d.execution_result.as_ref())
            .map(|r| if r.success { 1.0 } else { 0.0 })
            .sum::<f32>() / recent_decisions.len() as f32;

        let avg_satisfaction = recent_decisions.iter()
            .filter_map(|d| d.execution_result.as_ref())
            .filter_map(|r| r.user_satisfaction)
            .sum::<f32>() / recent_decisions.len() as f32;

        PatternEffectiveness {
            pattern: self.config.pattern.clone(),
            avg_confidence,
            success_rate,
            avg_user_satisfaction: if avg_satisfaction > 0.0 { Some(avg_satisfaction) } else { None },
            decision_count: recent_decisions.len() as u32,
            recommended_adjustments: self.generate_pattern_recommendations(&recent_decisions),
        }
    }

    /// 生成模式调整建议
    fn generate_pattern_recommendations(&self, decisions: &[&DecisionRecord]) -> Vec<String> {
        let mut recommendations = Vec::new();

        let avg_confidence = decisions.iter()
            .map(|d| d.confidence)
            .sum::<f32>() / decisions.len() as f32;

        if avg_confidence < 0.5 {
            recommendations.push("考虑切换到更保守的模式以提高决策信心".to_string());
        }

        if let Some(last_decision) = decisions.first() {
            if let Some(result) = &last_decision.execution_result {
                if !result.success {
                    recommendations.push("最近的执行失败，建议暂时使用稳定模式".to_string());
                }
                
                if let Some(satisfaction) = result.user_satisfaction {
                    if satisfaction < 0.5 {
                        recommendations.push("用户满意度较低，考虑调整响应风格".to_string());
                    }
                }
            }
        }

        if recommendations.is_empty() {
            recommendations.push("当前模式运行良好，继续保持".to_string());
        }

        recommendations
    }
}

/// 模式有效性评估
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternEffectiveness {
    pub pattern: BehaviorPattern,
    pub avg_confidence: f32,
    pub success_rate: f32,
    pub avg_user_satisfaction: Option<f32>,
    pub decision_count: u32,
    pub recommended_adjustments: Vec<String>,
}

impl Default for PatternEffectiveness {
    fn default() -> Self {
        Self {
            pattern: BehaviorPattern::Stable,
            avg_confidence: 0.0,
            success_rate: 0.0,
            avg_user_satisfaction: None,
            decision_count: 0,
            recommended_adjustments: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_behavior_pattern_metrics() {
        assert_eq!(BehaviorPattern::Passive.risk_tolerance(), 0.0);
        assert_eq!(BehaviorPattern::Creative.innovation_tendency(), 0.9);
        assert!(BehaviorPattern::Stable.risk_tolerance() < BehaviorPattern::Learning.risk_tolerance());
    }

    #[test]
    fn test_behavior_engine_decision() {
        let config = BehaviorConfig::new(BehaviorPattern::Stable);
        let context = BehaviorContext::default();
        let mut engine = BehaviorEngine::new(config, context);

        let options = vec![
            DecisionOption::new("safe", "Safe option").with_metrics(0.2, 0.9, 0.1, 0.1),
            DecisionOption::new("risky", "Risky option").with_metrics(0.8, 0.3, 0.9, 0.8),
        ];

        let decision = engine.make_decision(DecisionType::ToolSelection, options).unwrap();
        assert_eq!(decision.chosen_option, "safe");
        assert!(decision.confidence > 0.5);
    }

    #[test]
    fn test_pattern_switch_conditions() {
        let condition = SwitchCondition::KeywordMatch {
            keywords: vec!["create".to_string(), "innovate".to_string()],
            case_sensitive: false,
        };
        
        // 这里可以添加条件匹配测试
        match condition {
            SwitchCondition::KeywordMatch { keywords, .. } => {
                assert!(keywords.contains(&"create".to_string()));
            }
            _ => panic!("Expected KeywordMatch condition"),
        }
    }
}