//! 强化学习模块 - 实现智能体持续优化决策

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::{ComponentWeights, HybridAnalysisResult};

/// 强化学习管理器
pub struct ReinforcementLearningManager {
    workspace_root: PathBuf,
    pub agent: RLAgent,
    environment: AnalysisEnvironment,
    config: RLConfig,
    training_history: VecDeque<Episode>,
    performance_tracker: RLPerformanceTracker,
}

/// 强化学习配置
#[derive(Debug, Clone)]
pub struct RLConfig {
    pub algorithm: RLAlgorithm,
    pub policy_backend: RLPolicyBackend,
    pub learning_rate: f32,
    pub discount_factor: f32,
    pub exploration_rate: f32,
    pub exploration_decay: f32,
    pub min_exploration_rate: f32,
    pub memory_size: usize,
    pub batch_size: usize,
    pub update_frequency: usize,
    pub reward_shaping: RewardShaping,
    pub default_user_profile_features: Vec<f32>,
}

pub const DEFAULT_RL_LEARNING_RATE: f32 = 0.001;
pub const DEFAULT_RL_DISCOUNT_FACTOR: f32 = 0.95;
pub const DEFAULT_RL_EXPLORATION_RATE: f32 = 0.3;
pub const DEFAULT_RL_EXPLORATION_DECAY: f32 = 0.995;
pub const DEFAULT_RL_MIN_EXPLORATION_RATE: f32 = 0.01;
pub const DEFAULT_RL_MEMORY_SIZE: usize = 10_000;
pub const DEFAULT_RL_BATCH_SIZE: usize = 32;
pub const DEFAULT_RL_UPDATE_FREQUENCY: usize = 100;
pub const DEFAULT_USER_PROFILE_FEATURE_VALUE: f32 = 0.5;
pub const DEFAULT_USER_PROFILE_FEATURE_COUNT: usize = 3;

impl Default for RLConfig {
    fn default() -> Self {
        Self {
            algorithm: RLAlgorithm::QLearning,
            policy_backend: RLPolicyBackend::Simulated,
            learning_rate: DEFAULT_RL_LEARNING_RATE,
            discount_factor: DEFAULT_RL_DISCOUNT_FACTOR,
            exploration_rate: DEFAULT_RL_EXPLORATION_RATE,
            exploration_decay: DEFAULT_RL_EXPLORATION_DECAY,
            min_exploration_rate: DEFAULT_RL_MIN_EXPLORATION_RATE,
            memory_size: DEFAULT_RL_MEMORY_SIZE,
            batch_size: DEFAULT_RL_BATCH_SIZE,
            update_frequency: DEFAULT_RL_UPDATE_FREQUENCY,
            reward_shaping: RewardShaping::default(),
            default_user_profile_features: vec![
                DEFAULT_USER_PROFILE_FEATURE_VALUE;
                DEFAULT_USER_PROFILE_FEATURE_COUNT
            ],
        }
    }
}

/// 强化学习算法类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RLAlgorithm {
    QLearning,      // Q学习
    PolicyGradient, // 策略梯度
    ActorCritic,    // Actor-Critic
    DQN,            // 深度Q网络
}

/// 强化学习策略后端。
///
/// 当前内置实现是轻量模拟后端；真实策略网络应通过 External 接入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RLPolicyBackend {
    Simulated,
    External { provider: String },
}

/// 奖励塑形配置
#[derive(Debug, Clone)]
pub struct RewardShaping {
    pub accuracy_weight: f32,
    pub user_satisfaction_weight: f32,
    pub response_time_weight: f32,
    pub consistency_weight: f32,
    pub exploration_bonus: f32,
    pub penalty_for_errors: f32,
}

pub const DEFAULT_REWARD_ACCURACY_WEIGHT: f32 = 0.4;
pub const DEFAULT_REWARD_USER_SATISFACTION_WEIGHT: f32 = 0.4;
pub const DEFAULT_REWARD_RESPONSE_TIME_WEIGHT: f32 = 0.1;
pub const DEFAULT_REWARD_CONSISTENCY_WEIGHT: f32 = 0.1;
pub const DEFAULT_REWARD_EXPLORATION_BONUS: f32 = 0.01;
pub const DEFAULT_REWARD_ERROR_PENALTY: f32 = -0.1;

impl Default for RewardShaping {
    fn default() -> Self {
        Self {
            accuracy_weight: DEFAULT_REWARD_ACCURACY_WEIGHT,
            user_satisfaction_weight: DEFAULT_REWARD_USER_SATISFACTION_WEIGHT,
            response_time_weight: DEFAULT_REWARD_RESPONSE_TIME_WEIGHT,
            consistency_weight: DEFAULT_REWARD_CONSISTENCY_WEIGHT,
            exploration_bonus: DEFAULT_REWARD_EXPLORATION_BONUS,
            penalty_for_errors: DEFAULT_REWARD_ERROR_PENALTY,
        }
    }
}

/// 强化学习智能体
pub struct RLAgent {
    pub q_table: HashMap<StateKey, HashMap<Action, f32>>,
    pub policy_network: Option<PolicyNetwork>,
    pub experience_buffer: VecDeque<Experience>,
    pub current_state: Option<State>,
    pub total_steps: usize,
    pub exploration_rate: f32,
}

/// 状态键（用于Q表索引）
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct StateKey {
    pub input_complexity: u8,            // 输入复杂度等级 (0-9)
    pub context_type: ContextType,       // 上下文类型
    pub user_history: UserHistoryType,   // 用户历史类型
    pub dimension_focus: DimensionFocus, // 主要关注的维度
}

/// 上下文类型
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContextType {
    Creative,      // 创造性任务
    Analytical,    // 分析性任务
    Routine,       // 常规任务
    Urgent,        // 紧急任务
    Collaborative, // 协作任务
}

/// 用户历史类型
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum UserHistoryType {
    NewUser,      // 新用户
    Experienced,  // 有经验的用户
    FrequentUser, // 频繁用户
    PowerUser,    // 高级用户
}

/// 维度焦点
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum DimensionFocus {
    Creativity,    // 创造性
    Urgency,       // 紧急性
    Complexity,    // 复杂度
    Collaboration, // 协作性
    Mixed,         // 混合
}

/// 强化学习状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub key: StateKey,
    pub input: String,
    pub context_features: Vec<f32>,
    pub user_profile_features: Vec<f32>,
    pub historical_performance: f32,
    pub timestamp: DateTime<Utc>,
}

/// 强化学习动作
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum Action {
    UseDefaultWeights,     // 使用默认权重
    EmphasizeLegacy,       // 强调传统方法
    EmphasizeEnhanced,     // 强调增强方法
    EmphasizeVector,       // 强调向量匹配
    EmphasizeMultipath,    // 强调多路径
    EmphasizePersonalized, // 强调个性化
    BalancedApproach,      // 平衡方法
    AdaptiveWeighting,     // 自适应权重
    ExploratoryAnalysis,   // 探索性分析
    ConservativeAnalysis,  // 保守性分析
}

impl Action {
    /// 将动作转换为组件权重
    pub fn to_component_weights(&self) -> ComponentWeights {
        match self {
            Action::UseDefaultWeights => ComponentWeights::default(),
            Action::EmphasizeLegacy => ComponentWeights {
                legacy_weight: 0.4,
                enhanced_weight: 0.2,
                vector_weight: 0.2,
                multipath_weight: 0.1,
                personalized_weight: 0.1,
                ..ComponentWeights::default()
            },
            Action::EmphasizeEnhanced => ComponentWeights {
                legacy_weight: 0.1,
                enhanced_weight: 0.4,
                vector_weight: 0.25,
                multipath_weight: 0.15,
                personalized_weight: 0.1,
                ..ComponentWeights::default()
            },
            Action::EmphasizeVector => ComponentWeights {
                legacy_weight: 0.1,
                enhanced_weight: 0.2,
                vector_weight: 0.45,
                multipath_weight: 0.15,
                personalized_weight: 0.1,
                ..ComponentWeights::default()
            },
            Action::EmphasizeMultipath => ComponentWeights {
                legacy_weight: 0.15,
                enhanced_weight: 0.2,
                vector_weight: 0.2,
                multipath_weight: 0.35,
                personalized_weight: 0.1,
                ..ComponentWeights::default()
            },
            Action::EmphasizePersonalized => ComponentWeights {
                legacy_weight: 0.1,
                enhanced_weight: 0.2,
                vector_weight: 0.25,
                multipath_weight: 0.15,
                personalized_weight: 0.3,
                ..ComponentWeights::default()
            },
            Action::BalancedApproach => ComponentWeights {
                legacy_weight: 0.2,
                enhanced_weight: 0.2,
                vector_weight: 0.2,
                multipath_weight: 0.2,
                personalized_weight: 0.2,
                ..ComponentWeights::default()
            },
            Action::AdaptiveWeighting => ComponentWeights {
                legacy_weight: 0.15,
                enhanced_weight: 0.25,
                vector_weight: 0.3,
                multipath_weight: 0.2,
                personalized_weight: 0.1,
                intent_influence: 0.2,
                context_boost: 0.15,
            },
            Action::ExploratoryAnalysis => ComponentWeights {
                legacy_weight: 0.1,
                enhanced_weight: 0.2,
                vector_weight: 0.35,
                multipath_weight: 0.25,
                personalized_weight: 0.1,
                intent_influence: 0.25,
                context_boost: 0.2,
            },
            Action::ConservativeAnalysis => ComponentWeights {
                legacy_weight: 0.3,
                enhanced_weight: 0.3,
                vector_weight: 0.2,
                multipath_weight: 0.15,
                personalized_weight: 0.05,
                intent_influence: 0.1,
                context_boost: 0.05,
            },
        }
    }

    /// 获取所有可能的动作
    pub fn all_actions() -> Vec<Action> {
        vec![
            Action::UseDefaultWeights,
            Action::EmphasizeLegacy,
            Action::EmphasizeEnhanced,
            Action::EmphasizeVector,
            Action::EmphasizeMultipath,
            Action::EmphasizePersonalized,
            Action::BalancedApproach,
            Action::AdaptiveWeighting,
            Action::ExploratoryAnalysis,
            Action::ConservativeAnalysis,
        ]
    }
}

/// 经验回放
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experience {
    pub state: State,
    pub action: Action,
    pub reward: f32,
    pub next_state: Option<State>,
    pub done: bool,
    pub timestamp: DateTime<Utc>,
}

/// 策略网络（简化版本）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyNetwork {
    weights: Vec<Vec<f32>>,
    biases: Vec<f32>,
    input_size: usize,
    hidden_size: usize,
    output_size: usize,
}

impl PolicyNetwork {
    pub fn new(input_size: usize, hidden_size: usize, output_size: usize) -> Self {
        // 初始化随机权重
        let mut weights = Vec::new();
        weights.push(Self::random_matrix(input_size, hidden_size));
        weights.push(Self::random_matrix(hidden_size, output_size));

        let biases = Self::random_vector(hidden_size + output_size);

        Self {
            weights,
            biases,
            input_size,
            hidden_size,
            output_size,
        }
    }

    fn random_matrix(rows: usize, cols: usize) -> Vec<f32> {
        (0..rows * cols)
            .map(|_| (rand::random::<f32>() - 0.5) * 0.1)
            .collect()
    }

    fn random_vector(size: usize) -> Vec<f32> {
        (0..size)
            .map(|_| (rand::random::<f32>() - 0.5) * 0.1)
            .collect()
    }

    pub fn forward(&self, input: &[f32]) -> Vec<f32> {
        let mut layer_input = input.to_vec();

        for (i, layer_weights) in self.weights.iter().enumerate() {
            let layer_output_size = if i == 0 {
                self.hidden_size
            } else {
                self.output_size
            };
            let mut layer_output = vec![0.0; layer_output_size];

            for j in 0..layer_output_size {
                let mut sum = 0.0;
                for k in 0..layer_input.len() {
                    sum += layer_input[k] * layer_weights[j * layer_input.len() + k];
                }
                sum += self.biases[if i == 0 { j } else { self.hidden_size + j }];
                layer_output[j] = Self::relu(sum);
            }

            layer_input = layer_output;
        }

        // 应用softmax到最后一层
        Self::softmax(&layer_input)
    }

    fn relu(x: f32) -> f32 {
        x.max(0.0)
    }

    fn softmax(x: &[f32]) -> Vec<f32> {
        let max_val = x.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let exp_x: Vec<f32> = x.iter().map(|&val| (val - max_val).exp()).collect();
        let sum_exp: f32 = exp_x.iter().sum();
        exp_x.iter().map(|&val| val / sum_exp).collect()
    }
}

/// 分析环境
pub struct AnalysisEnvironment {
    current_episode: usize,
    episode_rewards: Vec<f32>,
    state_transition_counts: HashMap<(StateKey, Action), usize>,
}

/// 训练episode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub episode_id: usize,
    pub experiences: Vec<Experience>,
    pub total_reward: f32,
    pub average_accuracy: f32,
    pub average_satisfaction: f32,
    pub steps: usize,
    pub duration: std::time::Duration,
    pub timestamp: DateTime<Utc>,
}

/// 强化学习性能跟踪器
pub struct RLPerformanceTracker {
    cumulative_reward: f32,
    episode_rewards: VecDeque<f32>,
    success_rate: f32,
    exploration_efficiency: f32,
    convergence_metrics: ConvergenceMetrics,
}

/// 收敛指标
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceMetrics {
    pub reward_variance: f32,
    pub policy_stability: f32,
    pub learning_progress: f32,
    pub episodes_to_convergence: Option<usize>,
}

impl ReinforcementLearningManager {
    /// 创建强化学习管理器
    pub fn new(workspace_root: PathBuf, config: RLConfig) -> Self {
        Self {
            workspace_root: workspace_root.clone(),
            agent: RLAgent::new(&config),
            environment: AnalysisEnvironment::new(),
            config,
            training_history: VecDeque::new(),
            performance_tracker: RLPerformanceTracker::new(),
        }
    }

    /// 初始化强化学习系统
    pub fn initialize(&mut self) -> Result<(), String> {
        // 创建RL数据目录
        let rl_dir = self.workspace_root.join("rl_data");
        fs::create_dir_all(&rl_dir).map_err(|e| format!("创建RL目录失败: {}", e))?;

        // 加载已有的Q表和经验
        self.load_agent_state()?;

        Ok(())
    }

    /// 选择动作（基于当前策略）
    pub fn select_action(&mut self, state: &State) -> Action {
        match self.config.algorithm {
            RLAlgorithm::QLearning => self.select_action_q_learning(state),
            RLAlgorithm::PolicyGradient => self.select_action_policy_gradient(state),
            RLAlgorithm::ActorCritic => self.select_action_actor_critic(state),
            RLAlgorithm::DQN => self.select_action_dqn(state),
        }
    }

    /// Q-Learning动作选择
    fn select_action_q_learning(&mut self, state: &State) -> Action {
        if rand::random::<f32>() < self.agent.exploration_rate {
            // 探索：随机选择动作
            let actions = Action::all_actions();
            let index = rand::random::<usize>() % actions.len();
            actions[index].clone()
        } else {
            // 利用：选择Q值最高的动作
            self.get_best_action(&state.key)
        }
    }

    /// 策略梯度动作选择
    fn select_action_policy_gradient(&self, state: &State) -> Action {
        if let Some(ref policy_network) = self.agent.policy_network {
            let state_features = self.extract_state_features(state);
            let action_probabilities = policy_network.forward(&state_features);

            // 基于概率分布采样动作
            self.sample_action_from_probabilities(&action_probabilities)
        } else {
            // fallback到随机选择
            let actions = Action::all_actions();
            let index = rand::random::<usize>() % actions.len();
            actions[index].clone()
        }
    }

    /// Actor-Critic动作选择（简化实现）
    fn select_action_actor_critic(&self, state: &State) -> Action {
        // 简化为策略梯度方法
        self.select_action_policy_gradient(state)
    }

    /// DQN动作选择（简化实现）
    fn select_action_dqn(&mut self, state: &State) -> Action {
        // 简化为Q-Learning方法
        self.select_action_q_learning(state)
    }

    /// 获取最佳动作
    fn get_best_action(&self, state_key: &StateKey) -> Action {
        if let Some(q_values) = self.agent.q_table.get(state_key) {
            q_values
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(action, _)| action.clone())
                .unwrap_or(Action::UseDefaultWeights)
        } else {
            Action::UseDefaultWeights
        }
    }

    /// 从概率分布中采样动作
    fn sample_action_from_probabilities(&self, probabilities: &[f32]) -> Action {
        let mut cumsum = 0.0;
        let random = rand::random::<f32>();
        let actions = Action::all_actions();

        for (i, &prob) in probabilities.iter().enumerate() {
            cumsum += prob;
            if random <= cumsum && i < actions.len() {
                return actions[i].clone();
            }
        }

        // fallback
        Action::UseDefaultWeights
    }

    /// 提取状态特征向量
    fn extract_state_features(&self, state: &State) -> Vec<f32> {
        let mut features = Vec::new();

        // 状态键特征
        features.push(state.key.input_complexity as f32 / 9.0);
        features.push(self.context_type_to_value(&state.key.context_type));
        features.push(self.user_history_type_to_value(&state.key.user_history));
        features.push(self.dimension_focus_to_value(&state.key.dimension_focus));

        // 上下文特征
        features.extend_from_slice(&state.context_features);

        // 用户档案特征
        features.extend_from_slice(&state.user_profile_features);

        // 历史性能
        features.push(state.historical_performance);

        features
    }

    fn context_type_to_value(&self, context_type: &ContextType) -> f32 {
        match context_type {
            ContextType::Creative => 0.0,
            ContextType::Analytical => 0.25,
            ContextType::Routine => 0.5,
            ContextType::Urgent => 0.75,
            ContextType::Collaborative => 1.0,
        }
    }

    fn user_history_type_to_value(&self, user_type: &UserHistoryType) -> f32 {
        match user_type {
            UserHistoryType::NewUser => 0.0,
            UserHistoryType::Experienced => 0.33,
            UserHistoryType::FrequentUser => 0.66,
            UserHistoryType::PowerUser => 1.0,
        }
    }

    fn dimension_focus_to_value(&self, focus: &DimensionFocus) -> f32 {
        match focus {
            DimensionFocus::Creativity => 0.0,
            DimensionFocus::Urgency => 0.25,
            DimensionFocus::Complexity => 0.5,
            DimensionFocus::Collaboration => 0.75,
            DimensionFocus::Mixed => 1.0,
        }
    }

    /// 计算奖励
    pub fn calculate_reward(
        &self,
        analysis_result: &HybridAnalysisResult,
        user_satisfaction: f32,
        response_time: std::time::Duration,
    ) -> f32 {
        let shaping = &self.config.reward_shaping;
        let mut reward = 0.0;

        // 基于准确度的奖励
        reward += analysis_result.confidence_score * shaping.accuracy_weight;

        // 基于用户满意度的奖励
        reward += user_satisfaction * shaping.user_satisfaction_weight;

        // 基于响应时间的奖励（越快越好）
        let response_time_secs = response_time.as_secs_f32();
        let time_reward = (2.0 - response_time_secs).max(0.0).min(2.0) / 2.0;
        reward += time_reward * shaping.response_time_weight;

        // 一致性奖励（基于融合策略的稳定性）
        let consistency_score = self.calculate_consistency_score(analysis_result);
        reward += consistency_score * shaping.consistency_weight;

        // 探索奖励
        if self.agent.exploration_rate > self.config.min_exploration_rate {
            reward += shaping.exploration_bonus;
        }

        // 错误惩罚
        if analysis_result.confidence_score < 0.3 {
            reward += shaping.penalty_for_errors;
        }

        reward.clamp(-1.0, 1.0)
    }

    /// 计算一致性分数
    fn calculate_consistency_score(&self, analysis_result: &HybridAnalysisResult) -> f32 {
        // 简化的一致性计算：基于各组件结果的相似性
        let mut total_similarity = 0.0f32;
        let mut comparisons = 0;

        // 比较legacy和enhanced结果
        total_similarity += analysis_result
            .legacy_result
            .cosine_similarity(&analysis_result.enhanced_result);
        comparisons += 1;

        // 比较vector结果（如果存在）
        if let Some(ref vector_result) = analysis_result.vector_result {
            total_similarity += analysis_result
                .final_result
                .cosine_similarity(&vector_result.tag_vector);
            comparisons += 1;
        }

        // 比较multipath结果（如果存在）
        if let Some(ref multipath_result) = analysis_result.multipath_result {
            total_similarity += analysis_result
                .final_result
                .cosine_similarity(&multipath_result.final_tag_vector);
            comparisons += 1;
        }

        if comparisons > 0 {
            total_similarity / comparisons as f32
        } else {
            0.5
        }
    }

    /// 更新智能体
    pub fn update_agent(&mut self, experience: Experience) {
        // 添加经验到缓冲区
        self.agent.experience_buffer.push_back(experience.clone());
        if self.agent.experience_buffer.len() > self.config.memory_size {
            self.agent.experience_buffer.pop_front();
        }

        // 更新探索率
        self.agent.exploration_rate = (self.agent.exploration_rate * self.config.exploration_decay)
            .max(self.config.min_exploration_rate);

        // 根据算法类型更新
        match self.config.algorithm {
            RLAlgorithm::QLearning => self.update_q_learning(&experience),
            RLAlgorithm::PolicyGradient => self.update_policy_gradient(),
            RLAlgorithm::ActorCritic => self.update_actor_critic(),
            RLAlgorithm::DQN => self.update_dqn(),
        }

        self.agent.total_steps += 1;
    }

    /// 更新Q-Learning
    fn update_q_learning(&mut self, experience: &Experience) {
        let state_key = &experience.state.key;
        let action = &experience.action;
        let reward = experience.reward;

        // 获取当前Q值
        let current_q = self
            .agent
            .q_table
            .get(state_key)
            .and_then(|actions| actions.get(action))
            .copied()
            .unwrap_or(0.0);

        // 计算下一状态的最大Q值
        let next_max_q = if let Some(ref next_state) = experience.next_state {
            self.agent
                .q_table
                .get(&next_state.key)
                .map(|actions| actions.values().fold(0.0f32, |acc, &q| acc.max(q)))
                .unwrap_or(0.0)
        } else {
            0.0
        };

        // Q-Learning更新规则
        let target_q = reward + self.config.discount_factor * next_max_q;
        let new_q = current_q + self.config.learning_rate * (target_q - current_q);

        // 更新Q表
        self.agent
            .q_table
            .entry(state_key.clone())
            .or_insert_with(HashMap::new)
            .insert(action.clone(), new_q);
    }

    /// 更新策略梯度（简化实现）
    fn update_policy_gradient(&mut self) {
        if self.agent.experience_buffer.len() < self.config.batch_size {
            return;
        }

        match &self.config.policy_backend {
            RLPolicyBackend::Simulated => {
                // 轻量模拟后端只记录经验，不执行真实网络梯度更新。
            }
            RLPolicyBackend::External { provider: _ } => {
                // 外部策略网络接入点预留在这里，避免把模拟逻辑伪装成真实训练。
            }
        }
    }

    /// 更新Actor-Critic（简化实现）
    fn update_actor_critic(&mut self) {
        // 简化实现，实际中需要同时更新actor和critic网络
        self.update_policy_gradient();
    }

    /// 更新DQN（简化实现）
    fn update_dqn(&mut self) {
        // 简化为Q-Learning更新
        if let Some(experience) = self.agent.experience_buffer.back().cloned() {
            self.update_q_learning(&experience);
        }
    }

    /// 开始新的episode
    pub fn start_episode(&mut self) -> usize {
        self.environment.current_episode += 1;
        self.environment.current_episode
    }

    /// 结束episode并记录
    pub fn end_episode(
        &mut self,
        episode_id: usize,
        experiences: Vec<Experience>,
        duration: std::time::Duration,
    ) -> Episode {
        let total_reward: f32 = experiences.iter().map(|e| e.reward).sum();

        let average_accuracy = if !experiences.is_empty() {
            experiences
                .iter()
                .map(|e| {
                    if let Some(ref next_state) = e.next_state {
                        next_state.historical_performance
                    } else {
                        0.5
                    }
                })
                .sum::<f32>()
                / experiences.len() as f32
        } else {
            0.0
        };

        let average_satisfaction = if !experiences.is_empty() {
            experiences
                .iter()
                .map(|e| (e.reward + 1.0) / 2.0) // 将奖励转换为满意度近似值
                .sum::<f32>()
                / experiences.len() as f32
        } else {
            0.0
        };

        let episode = Episode {
            episode_id,
            experiences,
            total_reward,
            average_accuracy,
            average_satisfaction,
            steps: self.agent.total_steps,
            duration,
            timestamp: Utc::now(),
        };

        // 更新性能跟踪
        self.performance_tracker
            .episode_rewards
            .push_back(total_reward);
        if self.performance_tracker.episode_rewards.len() > 100 {
            self.performance_tracker.episode_rewards.pop_front();
        }
        self.performance_tracker.cumulative_reward += total_reward;

        // 记录episode
        self.training_history.push_back(episode.clone());
        if self.training_history.len() > 1000 {
            self.training_history.pop_front();
        }

        episode
    }

    /// 从输入构建状态
    pub fn build_state_from_input(&self, input: &str, user_id: Option<&str>) -> State {
        let complexity = self.estimate_input_complexity(input);
        let context_type = self.classify_context_type(input);
        let user_history = self.classify_user_history(user_id);
        let dimension_focus = self.identify_dimension_focus(input);

        let state_key = StateKey {
            input_complexity: complexity,
            context_type,
            user_history,
            dimension_focus,
        };

        let context_features = self.extract_context_features(input);
        let user_profile_features = self.extract_user_profile_features(user_id);
        let historical_performance = self.get_historical_performance(&state_key);

        State {
            key: state_key,
            input: input.to_string(),
            context_features,
            user_profile_features,
            historical_performance,
            timestamp: Utc::now(),
        }
    }

    /// 估计输入复杂度
    fn estimate_input_complexity(&self, input: &str) -> u8 {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return 0;
        }

        let word_count = trimmed.split_whitespace().count();
        let char_count = trimmed.chars().count();
        // 中日韩等无空格文本：用语义块估算“词数”，避免整句被当成单词导致复杂度为 0
        let cjk_units = trimmed
            .chars()
            .filter(|c| {
                matches!(c,
                    '\u{4E00}'..='\u{9FFF}'
                        | '\u{3400}'..='\u{4DBF}'
                        | '\u{3040}'..='\u{309F}'
                        | '\u{30A0}'..='\u{30FF}'
                )
            })
            .count();
        let inferred_words = if cjk_units > 0 {
            word_count.max((cjk_units.saturating_add(1)) / 2)
        } else {
            word_count
        }
        .max(1);

        let unique_words = trimmed
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .collect::<std::collections::HashSet<_>>()
            .len()
            .max(if cjk_units > 0 { 1 } else { 0 });

        let complexity_score = (inferred_words as f32 * 0.3
            + char_count as f32 * 0.01
            + unique_words as f32 * 0.5)
            / 10.0;

        let mut score = complexity_score.clamp(0.0, 9.0) as u8;
        if score == 0 {
            score = 1;
        }
        score
    }

    /// 分类上下文类型
    fn classify_context_type(&self, input: &str) -> ContextType {
        let input_lower = input.to_lowercase();

        if input_lower.contains("创新")
            || input_lower.contains("设计")
            || input_lower.contains("创造")
        {
            ContextType::Creative
        } else if input_lower.contains("分析")
            || input_lower.contains("研究")
            || input_lower.contains("调研")
        {
            ContextType::Analytical
        } else if input_lower.contains("紧急")
            || input_lower.contains("urgent")
            || input_lower.contains("急")
        {
            ContextType::Urgent
        } else if input_lower.contains("团队")
            || input_lower.contains("合作")
            || input_lower.contains("协作")
        {
            ContextType::Collaborative
        } else {
            ContextType::Routine
        }
    }

    /// 分类用户历史类型
    fn classify_user_history(&self, user_id: Option<&str>) -> UserHistoryType {
        // 简化实现：基于用户ID的存在性判断
        match user_id {
            Some(id) => {
                // 这里应该基于实际的用户历史数据
                if id.contains("power") || id.contains("admin") {
                    UserHistoryType::PowerUser
                } else if id.contains("frequent") {
                    UserHistoryType::FrequentUser
                } else if id.contains("new") {
                    UserHistoryType::NewUser
                } else {
                    UserHistoryType::Experienced
                }
            }
            None => UserHistoryType::NewUser,
        }
    }

    /// 识别维度焦点
    fn identify_dimension_focus(&self, input: &str) -> DimensionFocus {
        let input_lower = input.to_lowercase();
        let mut focus_scores = HashMap::new();

        // 创造性关键词
        let creativity_keywords = ["创新", "创造", "设计", "原创", "想象"];
        focus_scores.insert(
            DimensionFocus::Creativity,
            creativity_keywords
                .iter()
                .map(|&k| if input_lower.contains(k) { 1 } else { 0 })
                .sum::<i32>(),
        );

        // 紧急性关键词
        let urgency_keywords = ["紧急", "urgent", "急", "立即", "马上"];
        focus_scores.insert(
            DimensionFocus::Urgency,
            urgency_keywords
                .iter()
                .map(|&k| if input_lower.contains(k) { 1 } else { 0 })
                .sum::<i32>(),
        );

        // 复杂度关键词
        let complexity_keywords = ["复杂", "困难", "挑战", "技术", "深度"];
        focus_scores.insert(
            DimensionFocus::Complexity,
            complexity_keywords
                .iter()
                .map(|&k| if input_lower.contains(k) { 1 } else { 0 })
                .sum::<i32>(),
        );

        // 协作关键词
        let collaboration_keywords = ["团队", "合作", "协作", "一起", "共同"];
        focus_scores.insert(
            DimensionFocus::Collaboration,
            collaboration_keywords
                .iter()
                .map(|&k| if input_lower.contains(k) { 1 } else { 0 })
                .sum::<i32>(),
        );

        // 找到得分最高的维度
        focus_scores
            .into_iter()
            .max_by_key(|(_, score)| *score)
            .map(|(focus, score)| {
                if score > 0 {
                    focus
                } else {
                    DimensionFocus::Mixed
                }
            })
            .unwrap_or(DimensionFocus::Mixed)
    }

    /// 提取上下文特征
    fn extract_context_features(&self, input: &str) -> Vec<f32> {
        vec![
            input.len() as f32 / 1000.0,                     // 长度特征
            input.split_whitespace().count() as f32 / 100.0, // 词数特征
            input.chars().filter(|c| c.is_uppercase()).count() as f32 / input.len() as f32, // 大写比例
            input.chars().filter(|c| c.is_ascii_punctuation()).count() as f32 / input.len() as f32, // 标点符号比例
        ]
    }

    /// 提取用户档案特征
    fn extract_user_profile_features(&self, _user_id: Option<&str>) -> Vec<f32> {
        self.config.default_user_profile_features.clone()
    }

    /// 获取历史性能
    fn get_historical_performance(&self, state_key: &StateKey) -> f32 {
        // 基于相似状态的历史性能
        self.training_history
            .iter()
            .filter(|episode| {
                episode
                    .experiences
                    .iter()
                    .any(|exp| exp.state.key == *state_key)
            })
            .map(|episode| episode.average_accuracy)
            .fold(0.0f32, |acc, perf| acc.max(perf))
            .max(0.5) // 默认性能
    }

    /// 获取强化学习统计信息
    pub fn get_rl_statistics(&self) -> RLStatistics {
        let average_episode_reward = if !self.performance_tracker.episode_rewards.is_empty() {
            self.performance_tracker.episode_rewards.iter().sum::<f32>()
                / self.performance_tracker.episode_rewards.len() as f32
        } else {
            0.0
        };

        let recent_performance = if self.training_history.len() >= 10 {
            let recent: f32 = self
                .training_history
                .iter()
                .rev()
                .take(10)
                .map(|e| e.total_reward)
                .sum();
            recent / 10.0
        } else {
            0.0
        };

        RLStatistics {
            total_episodes: self.environment.current_episode,
            total_steps: self.agent.total_steps,
            cumulative_reward: self.performance_tracker.cumulative_reward,
            average_episode_reward,
            current_exploration_rate: self.agent.exploration_rate,
            q_table_size: self.agent.q_table.len(),
            experience_buffer_size: self.agent.experience_buffer.len(),
            recent_performance,
            convergence_metrics: self.calculate_convergence_metrics(),
        }
    }

    /// 计算收敛指标
    fn calculate_convergence_metrics(&self) -> ConvergenceMetrics {
        let reward_variance = if self.performance_tracker.episode_rewards.len() > 1 {
            let mean = self.performance_tracker.episode_rewards.iter().sum::<f32>()
                / self.performance_tracker.episode_rewards.len() as f32;
            let variance = self
                .performance_tracker
                .episode_rewards
                .iter()
                .map(|&r| (r - mean).powi(2))
                .sum::<f32>()
                / self.performance_tracker.episode_rewards.len() as f32;
            variance
        } else {
            1.0
        };

        ConvergenceMetrics {
            reward_variance,
            policy_stability: self.calculate_policy_stability(),
            learning_progress: self.calculate_learning_progress(),
            episodes_to_convergence: self.estimate_episodes_to_convergence(),
        }
    }

    /// 计算策略稳定性
    fn calculate_policy_stability(&self) -> f32 {
        // 简化的策略稳定性计算
        if self.agent.exploration_rate < 0.1 {
            0.9
        } else {
            1.0 - self.agent.exploration_rate
        }
    }

    /// 计算学习进度
    fn calculate_learning_progress(&self) -> f32 {
        if self.training_history.len() < 2 {
            return 0.0;
        }

        let recent_rewards: f32 = self
            .training_history
            .iter()
            .rev()
            .take(10)
            .map(|e| e.total_reward)
            .sum();

        let early_rewards: f32 = self
            .training_history
            .iter()
            .take(10)
            .map(|e| e.total_reward)
            .sum();

        ((recent_rewards - early_rewards) / 10.0).clamp(0.0, 1.0)
    }

    /// 估计收敛所需episode数
    fn estimate_episodes_to_convergence(&self) -> Option<usize> {
        if self.performance_tracker.episode_rewards.len() > 50 {
            let variance = self.calculate_convergence_metrics().reward_variance;
            if variance < 0.01 {
                Some(self.environment.current_episode)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// 保存智能体状态
    pub fn save_agent_state(&self) -> Result<(), String> {
        let rl_dir = self.workspace_root.join("rl_data");

        // 保存Q表
        let q_table_content = serde_json::to_string_pretty(&self.agent.q_table)
            .map_err(|e| format!("序列化Q表失败: {}", e))?;
        fs::write(rl_dir.join("q_table.json"), q_table_content)
            .map_err(|e| format!("保存Q表失败: {}", e))?;

        // 保存训练历史
        let history_content = serde_json::to_string_pretty(&self.training_history)
            .map_err(|e| format!("序列化训练历史失败: {}", e))?;
        fs::write(rl_dir.join("training_history.json"), history_content)
            .map_err(|e| format!("保存训练历史失败: {}", e))?;

        Ok(())
    }

    /// 加载智能体状态
    fn load_agent_state(&mut self) -> Result<(), String> {
        let rl_dir = self.workspace_root.join("rl_data");

        // 加载Q表
        let q_table_file = rl_dir.join("q_table.json");
        if q_table_file.exists() {
            let content =
                fs::read_to_string(&q_table_file).map_err(|e| format!("读取Q表失败: {}", e))?;
            self.agent.q_table =
                serde_json::from_str(&content).map_err(|e| format!("解析Q表失败: {}", e))?;
        }

        // 加载训练历史
        let history_file = rl_dir.join("training_history.json");
        if history_file.exists() {
            let content = fs::read_to_string(&history_file)
                .map_err(|e| format!("读取训练历史失败: {}", e))?;
            self.training_history =
                serde_json::from_str(&content).map_err(|e| format!("解析训练历史失败: {}", e))?;
        }

        Ok(())
    }

    /// 获取最佳动作建议
    pub fn get_action_recommendation(&self, state: &State) -> ActionRecommendation {
        let actions = Action::all_actions();
        let mut action_values = Vec::new();

        for action in &actions {
            let q_value = self
                .agent
                .q_table
                .get(&state.key)
                .and_then(|q_actions| q_actions.get(action))
                .copied()
                .unwrap_or(0.0);

            action_values.push((action.clone(), q_value));
        }

        // 按Q值排序
        action_values.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        let confidence = self.calculate_recommendation_confidence(&state.key);
        let reasoning = self.generate_recommendation_reasoning(&action_values);

        ActionRecommendation {
            recommended_action: action_values[0].0.clone(),
            action_values,
            confidence,
            reasoning,
        }
    }

    /// 计算推荐置信度
    fn calculate_recommendation_confidence(&self, state_key: &StateKey) -> f32 {
        if let Some(q_values) = self.agent.q_table.get(state_key) {
            let values: Vec<f32> = q_values.values().copied().collect();
            if values.len() > 1 {
                let max_val = values.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
                let second_max = values
                    .iter()
                    .filter(|&&v| v < max_val)
                    .fold(f32::NEG_INFINITY, |a, &b| a.max(b));

                if second_max != f32::NEG_INFINITY {
                    ((max_val - second_max).abs() / 2.0).clamp(0.0, 1.0)
                } else {
                    0.5
                }
            } else {
                0.5
            }
        } else {
            0.0
        }
    }

    /// 生成推荐reasoning
    fn generate_recommendation_reasoning(&self, action_values: &[(Action, f32)]) -> String {
        if action_values.is_empty() {
            return "没有足够的数据进行推荐".to_string();
        }

        let best_action = &action_values[0].0;
        let best_value = action_values[0].1;

        match best_action {
            Action::UseDefaultWeights => "使用默认权重配置是最稳妥的选择".to_string(),
            Action::EmphasizeVector => "向量匹配在类似情况下表现最佳".to_string(),
            Action::EmphasizeEnhanced => "增强分析方法在此类任务中更有效".to_string(),
            Action::AdaptiveWeighting => "自适应权重能够动态调整以获得最佳效果".to_string(),
            _ => format!(
                "基于历史经验，{:?}方法的期望值为{:.3}",
                best_action, best_value
            ),
        }
    }
}

impl RLAgent {
    fn new(config: &RLConfig) -> Self {
        let policy_network = match config.algorithm {
            RLAlgorithm::PolicyGradient | RLAlgorithm::ActorCritic => {
                Some(PolicyNetwork::new(10, 64, Action::all_actions().len()))
            }
            _ => None,
        };

        Self {
            q_table: HashMap::new(),
            policy_network,
            experience_buffer: VecDeque::new(),
            current_state: None,
            total_steps: 0,
            exploration_rate: config.exploration_rate,
        }
    }
}

impl AnalysisEnvironment {
    fn new() -> Self {
        Self {
            current_episode: 0,
            episode_rewards: Vec::new(),
            state_transition_counts: HashMap::new(),
        }
    }
}

impl RLPerformanceTracker {
    fn new() -> Self {
        Self {
            cumulative_reward: 0.0,
            episode_rewards: VecDeque::new(),
            success_rate: 0.0,
            exploration_efficiency: 0.0,
            convergence_metrics: ConvergenceMetrics {
                reward_variance: 1.0,
                policy_stability: 0.0,
                learning_progress: 0.0,
                episodes_to_convergence: None,
            },
        }
    }
}

/// 强化学习统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RLStatistics {
    pub total_episodes: usize,
    pub total_steps: usize,
    pub cumulative_reward: f32,
    pub average_episode_reward: f32,
    pub current_exploration_rate: f32,
    pub q_table_size: usize,
    pub experience_buffer_size: usize,
    pub recent_performance: f32,
    pub convergence_metrics: ConvergenceMetrics,
}

/// 动作推荐
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRecommendation {
    pub recommended_action: Action,
    pub action_values: Vec<(Action, f32)>,
    pub confidence: f32,
    pub reasoning: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_rl_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = RLConfig::default();
        let mut manager = ReinforcementLearningManager::new(temp_dir.path().to_path_buf(), config);

        assert!(manager.initialize().is_ok());
        assert_eq!(manager.environment.current_episode, 0);
        assert_eq!(manager.agent.total_steps, 0);
    }

    #[test]
    fn test_state_building() {
        let temp_dir = TempDir::new().unwrap();
        let config = RLConfig::default();
        let manager = ReinforcementLearningManager::new(temp_dir.path().to_path_buf(), config);

        let state = manager.build_state_from_input("创建一个创新的设计方案", Some("test_user"));

        assert_eq!(state.key.context_type, ContextType::Creative);
        assert_eq!(state.key.dimension_focus, DimensionFocus::Creativity);
        assert!(state.key.input_complexity > 0);
        assert!(!state.context_features.is_empty());
    }

    #[test]
    fn test_action_selection() {
        let temp_dir = TempDir::new().unwrap();
        let config = RLConfig::default();
        let mut manager = ReinforcementLearningManager::new(temp_dir.path().to_path_buf(), config);

        let state = manager.build_state_from_input("分析数据", Some("user1"));
        let action = manager.select_action(&state);

        // 应该返回某个有效的动作
        assert!(Action::all_actions().contains(&action));
    }

    #[test]
    fn test_reward_calculation() {
        use crate::{HybridAnalysisResult, TagVector};
        use std::time::Duration;

        let temp_dir = TempDir::new().unwrap();
        let config = RLConfig::default();
        let manager = ReinforcementLearningManager::new(temp_dir.path().to_path_buf(), config);

        let mut final_result = TagVector::new();
        final_result.set("creativity_level", 0.8);

        let analysis_result = HybridAnalysisResult {
            input: "test".to_string(),
            final_result,
            legacy_result: TagVector::new(),
            enhanced_result: TagVector::new(),
            vector_result: None,
            multipath_result: None,
            personalized_result: None,
            fusion_strategy: "test".to_string(),
            confidence_score: 0.8,
            analysis_duration: Duration::from_millis(100),
        };

        let reward = manager.calculate_reward(&analysis_result, 0.9, Duration::from_millis(500));

        assert!(reward >= -1.0 && reward <= 1.0);
        assert!(reward > 0.0); // 应该是正奖励
    }

    #[test]
    fn test_component_weights_conversion() {
        let action = Action::EmphasizeVector;
        let weights = action.to_component_weights();

        assert_eq!(weights.vector_weight, 0.45);
        assert!(weights.legacy_weight < weights.vector_weight);
        assert!(weights.enhanced_weight < weights.vector_weight);
    }

    #[test]
    fn test_episode_management() {
        let temp_dir = TempDir::new().unwrap();
        let config = RLConfig::default();
        let mut manager = ReinforcementLearningManager::new(temp_dir.path().to_path_buf(), config);

        let episode_id = manager.start_episode();
        assert_eq!(episode_id, 1);

        let experiences = vec![];
        let episode =
            manager.end_episode(episode_id, experiences, std::time::Duration::from_secs(10));

        assert_eq!(episode.episode_id, episode_id);
        assert_eq!(episode.total_reward, 0.0);
        assert_eq!(manager.training_history.len(), 1);
    }

    #[test]
    fn test_q_learning_update() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = RLConfig::default();
        config.algorithm = RLAlgorithm::QLearning;
        let mut manager = ReinforcementLearningManager::new(temp_dir.path().to_path_buf(), config);

        let state = manager.build_state_from_input("测试输入", None);
        let action = Action::UseDefaultWeights;

        let experience = Experience {
            state: state.clone(),
            action: action.clone(),
            reward: 0.5,
            next_state: Some(state.clone()),
            done: false,
            timestamp: Utc::now(),
        };

        // Q表初始应该为空
        assert!(!manager.agent.q_table.contains_key(&state.key));

        manager.update_agent(experience);

        // 更新后应该有Q值
        assert!(manager.agent.q_table.contains_key(&state.key));
        assert!(manager.agent.q_table[&state.key].contains_key(&action));
    }
}
