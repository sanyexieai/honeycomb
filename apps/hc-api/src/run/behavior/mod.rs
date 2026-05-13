//! /v1/behavior/* HTTP 路由。

use anyhow::Result;
use axum::extract::Path;
use axum::Json;
use hc_service::transport::{
    BehaviorConfig, BehaviorContext, BehaviorEngine, BehaviorPattern, DecisionOption, DecisionType,
};
use serde_json::Value;

use super::ApiError;

pub(crate) async fn behavior_patterns() -> Result<Json<Value>, ApiError> {
    let patterns = vec![
        BehaviorPattern::Passive,
        BehaviorPattern::Stable,
        BehaviorPattern::Learning,
        BehaviorPattern::Creative,
        BehaviorPattern::Adaptive,
    ];

    let pattern_data: Vec<_> = patterns
        .iter()
        .map(|pattern| {
            serde_json::json!({
                "name": format!("{:?}", pattern).to_lowercase(),
                "description": match pattern {
                    BehaviorPattern::Passive => "被动执行模式 - 严格按照指令执行",
                    BehaviorPattern::Stable => "稳定模式 - 保守且可靠的决策",
                    BehaviorPattern::Learning => "学习模式 - 保守新建功能，注重学习",
                    BehaviorPattern::Creative => "创造模式 - 注重创新和探索",
                    BehaviorPattern::Adaptive => "自适应模式 - 根据情况动态调整",
                },
                "risk_tolerance": pattern.risk_tolerance(),
                "innovation_tendency": pattern.innovation_tendency(),
                "proactivity": pattern.proactivity(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "patterns": pattern_data
    })))
}

pub(crate) async fn behavior_pattern_get(Path(pattern_name): Path<String>) -> Result<Json<Value>, ApiError> {
    let pattern = BehaviorPattern::from_str(&pattern_name).map_err(|e| ApiError(e))?;

    let config = BehaviorConfig::new(pattern.clone());

    Ok(Json(serde_json::json!({
        "pattern": format!("{:?}", pattern).to_lowercase(),
        "description": match pattern {
            BehaviorPattern::Passive => "被动执行模式 - 严格按照指令执行",
            BehaviorPattern::Stable => "稳定模式 - 保守且可靠的决策",
            BehaviorPattern::Learning => "学习模式 - 保守新建功能，注重学习",
            BehaviorPattern::Creative => "创造模式 - 注重创新和探索",
            BehaviorPattern::Adaptive => "自适应模式 - 根据情况动态调整",
        },
        "attributes": {
            "risk_tolerance": pattern.risk_tolerance(),
            "innovation_tendency": pattern.innovation_tendency(),
            "proactivity": pattern.proactivity(),
        },
        "config": {
            "thinking_depth": config.thinking_depth,
            "enable_metacognition": config.enable_metacognition,
            "learning_rate": config.learning_rate,
        }
    })))
}

pub(crate) async fn behavior_pattern_test(
    Path(pattern_name): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let pattern = BehaviorPattern::from_str(&pattern_name).map_err(|e| ApiError(e))?;

    // 解析测试上下文
    let mut context = BehaviorContext::default();
    if let Some(ctx) = payload.get("context").and_then(|c| c.as_object()) {
        if let Some(user_id) = ctx.get("user_id").and_then(|v| v.as_str()) {
            context.user_id = Some(user_id.to_string());
        }
        if let Some(room_id) = ctx.get("room_id").and_then(|v| v.as_str()) {
            context.room_id = Some(room_id.to_string());
        }
        if let Some(task_type) = ctx.get("task_type").and_then(|v| v.as_str()) {
            context.task_type = Some(task_type.to_string());
        }
        if let Some(complexity) = ctx.get("complexity").and_then(|v| v.as_f64()) {
            context.estimated_complexity = Some(complexity as f32);
        }
        if let Some(success_rate) = ctx.get("success_rate").and_then(|v| v.as_f64()) {
            context.historical_success_rate = Some(success_rate as f32);
        }
        if let Some(time_pressure) = ctx.get("time_pressure").and_then(|v| v.as_f64()) {
            context.time_pressure = Some(time_pressure as f32);
        }
        if let Some(tools_count) = ctx.get("available_tools_count").and_then(|v| v.as_u64()) {
            context.available_tools_count = Some(tools_count as u32);
        }
    }

    // 解析配置
    let mut config = BehaviorConfig::new(pattern);
    if let Some(cfg) = payload.get("config").and_then(|c| c.as_object()) {
        if let Some(depth) = cfg.get("thinking_depth").and_then(|v| v.as_u64()) {
            config = config.with_thinking_depth(depth as u8);
        }
        if let Some(metacognition) = cfg.get("enable_metacognition").and_then(|v| v.as_bool()) {
            config = config.with_metacognition(metacognition);
        }
        if let Some(learning_rate) = cfg.get("learning_rate").and_then(|v| v.as_f64()) {
            config = config.with_learning_rate(learning_rate as f32);
        }
    }

    // 创建行为引擎
    let mut engine = BehaviorEngine::new(config, context);

    // 创建一些测试选项
    let options = vec![
        DecisionOption::new("conservative", "保守方案 - 使用已知可靠的方法")
            .with_pros(vec!["风险低".to_string(), "成功率高".to_string()])
            .with_cons(vec!["创新性不足".to_string()])
            .with_estimated_effort(Some(3.0))
            .with_success_probability(Some(0.9))
            .with_innovation_level(Some(0.2))
            .with_risk_level(Some(0.1)),
        DecisionOption::new("balanced", "平衡方案 - 在安全和创新之间取平衡")
            .with_pros(vec!["平衡性好".to_string(), "适应性强".to_string()])
            .with_cons(vec!["可能不够大胆".to_string()])
            .with_estimated_effort(Some(5.0))
            .with_success_probability(Some(0.7))
            .with_innovation_level(Some(0.5))
            .with_risk_level(Some(0.3)),
        DecisionOption::new("innovative", "创新方案 - 尝试新的方法和技术")
            .with_pros(vec!["创新性高".to_string(), "学习价值大".to_string()])
            .with_cons(vec!["风险高".to_string(), "不确定性大".to_string()])
            .with_estimated_effort(Some(8.0))
            .with_success_probability(Some(0.5))
            .with_innovation_level(Some(0.9))
            .with_risk_level(Some(0.7)),
    ];

    let decision_record = engine
        .make_decision(DecisionType::StrategySelection, options)
        .map_err(|e| ApiError(anyhow::anyhow!("Decision making failed: {}", e)))?;

    Ok(Json(serde_json::to_value(&decision_record).unwrap()))
}
