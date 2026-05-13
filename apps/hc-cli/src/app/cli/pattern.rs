//! `hc-cli pattern` 子命令（行为模式）。
use anyhow::{Context, Result, bail};
use hc_service::transport::{
    BehaviorConfig, BehaviorContext, BehaviorEngine, BehaviorPattern, DecisionOption, DecisionType,
};
use serde_json;
use std::collections::BTreeMap;

pub(super) fn handle_pattern(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "list" => handle_pattern_list(rest),
        [cmd, rest @ ..] if cmd == "show" => handle_pattern_show(rest),
        [cmd, rest @ ..] if cmd == "test" => handle_pattern_test(rest),
        [cmd, rest @ ..] if cmd == "config" => handle_pattern_config(rest),
        [cmd, rest @ ..] if cmd == "default" => handle_pattern_default(rest),
        [] => {
            println!("pattern commands:");
            println!("  list      - list available behavior patterns");
            println!("  show      - show pattern details");
            println!("  test      - test pattern decision making");
            println!("  config    - configure pattern settings");
            println!("  default   - show system default pattern");
            Ok(())
        }
        [other, ..] => bail!("unknown pattern command: {other}"),
    }
}

pub(super) fn handle_pattern_list(args: &[String]) -> Result<()> {
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected argument: {other}"),
        }
    }

    let patterns = vec![
        BehaviorPattern::Passive,
        BehaviorPattern::Stable,
        BehaviorPattern::Learning,
        BehaviorPattern::Creative,
        BehaviorPattern::Adaptive,
    ];

    if json {
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

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "patterns": pattern_data
            }))?
        );
    } else {
        println!("可用的行为模式:");
        for pattern in patterns {
            println!(
                "  {:10} - {} (风险容忍度: {:.1}, 创新倾向: {:.1}, 主动性: {:.1})",
                format!("{:?}", pattern).to_lowercase(),
                match pattern {
                    BehaviorPattern::Passive => "被动执行模式 - 严格按照指令执行",
                    BehaviorPattern::Stable => "稳定模式 - 保守且可靠的决策",
                    BehaviorPattern::Learning => "学习模式 - 保守新建功能，注重学习",
                    BehaviorPattern::Creative => "创造模式 - 注重创新和探索",
                    BehaviorPattern::Adaptive => "自适应模式 - 根据情况动态调整",
                },
                pattern.risk_tolerance(),
                pattern.innovation_tendency(),
                pattern.proactivity(),
            );
        }
    }

    Ok(())
}

pub(super) fn handle_pattern_show(args: &[String]) -> Result<()> {
    let mut pattern_name = None;
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            name if pattern_name.is_none() => {
                pattern_name = Some(name.to_string());
                index += 1;
            }
            other => bail!("unexpected argument: {other}"),
        }
    }

    let pattern_name = pattern_name.context("missing pattern name")?;
    let pattern = BehaviorPattern::from_str(&pattern_name)?;
    let config = BehaviorConfig::new(pattern.clone());

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
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
            }))?
        );
    } else {
        println!("行为模式: {:?}", pattern);
        println!(
            "描述: {}",
            match pattern {
                BehaviorPattern::Passive => "被动执行模式 - 严格按照指令执行",
                BehaviorPattern::Stable => "稳定模式 - 保守且可靠的决策",
                BehaviorPattern::Learning => "学习模式 - 保守新建功能，注重学习",
                BehaviorPattern::Creative => "创造模式 - 注重创新和探索",
                BehaviorPattern::Adaptive => "自适应模式 - 根据情况动态调整",
            }
        );
        println!();
        println!("属性:");
        println!("  风险容忍度: {:.1}", pattern.risk_tolerance());
        println!("  创新倾向:   {:.1}", pattern.innovation_tendency());
        println!("  主动性:     {:.1}", pattern.proactivity());
        println!();
        println!("默认配置:");
        println!("  思考深度:     {}", config.thinking_depth);
        println!(
            "  启用元认知:   {}",
            if config.enable_metacognition {
                "是"
            } else {
                "否"
            }
        );
        println!("  学习率:       {:.2}", config.learning_rate.unwrap_or(0.0));
    }

    Ok(())
}

pub(super) fn handle_pattern_test(args: &[String]) -> Result<()> {
    let mut pattern_name = None;
    let mut context_pairs = Vec::new();
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--context" => {
                let context_value = args.get(index + 1).context("missing value for --context")?;

                if let Some((key, value)) = context_value.split_once('=') {
                    context_pairs.push((key.to_string(), value.to_string()));
                } else {
                    bail!("invalid context format: {context_value} (expected key=value)");
                }
                index += 2;
            }
            name if pattern_name.is_none() => {
                pattern_name = Some(name.to_string());
                index += 1;
            }
            other => bail!("unexpected argument: {other}"),
        }
    }

    let pattern_name = pattern_name.context("missing pattern name")?;
    let pattern = BehaviorPattern::from_str(&pattern_name)?;

    // 构建测试上下文
    let mut context = BehaviorContext {
        user_id: Some("test-user".to_string()),
        session_id: None,
        room_id: Some("test-room".to_string()),
        task_type: Some("cli-test".to_string()),
        estimated_complexity: Some(5.0),
        historical_success_rate: Some(0.8),
        available_tools_count: None,
        time_pressure: None,
        user_preferences: BTreeMap::new(),
        environment: BTreeMap::new(),
    };

    // 应用用户提供的上下文
    for (key, value) in context_pairs {
        match key.as_str() {
            "user_id" => context.user_id = Some(value),
            "room_id" => context.room_id = Some(value),
            "task_type" => context.task_type = Some(value),
            "complexity" => {
                context.estimated_complexity = Some(
                    value
                        .parse()
                        .context("invalid complexity value (expected number)")?,
                );
            }
            "success_rate" => {
                context.historical_success_rate = Some(
                    value
                        .parse()
                        .context("invalid success_rate value (expected number 0.0-1.0)")?,
                );
            }
            "time_pressure" => {
                context.time_pressure = Some(
                    value
                        .parse()
                        .context("invalid time_pressure value (expected number)")?,
                );
            }
            "available_tools_count" => {
                context.available_tools_count = Some(
                    value
                        .parse()
                        .context("invalid available_tools_count value (expected number)")?,
                );
            }
            other => bail!("unknown context key: {other}"),
        }
    }

    let config = BehaviorConfig::new(pattern.clone());
    let mut engine = BehaviorEngine::new(config, context.clone());

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

    let decision_record = engine.make_decision(DecisionType::StrategySelection, options.clone())?;

    if json {
        println!("{}", serde_json::to_string_pretty(&decision_record)?);
    } else {
        println!("行为模式测试: {:?}", pattern);
        println!();
        println!("上下文:");
        if let Some(user_id) = &context.user_id {
            println!("  用户ID: {}", user_id);
        }
        if let Some(room_id) = &context.room_id {
            println!("  房间ID: {}", room_id);
        }
        if let Some(task_type) = &context.task_type {
            println!("  任务类型: {}", task_type);
        }
        if let Some(complexity) = context.estimated_complexity {
            println!("  预估复杂度: {:.1}", complexity);
        }
        if let Some(success_rate) = context.historical_success_rate {
            println!("  历史成功率: {:.1}%", success_rate * 100.0);
        }
        println!();

        println!("决策结果:");
        println!("  选择的方案: {}", decision_record.chosen_option);
        println!("  决策理由: {}", decision_record.reasoning);
        println!("  信心度: {:.1}%", decision_record.confidence * 100.0);

        if !decision_record.options_considered.is_empty() {
            println!();
            println!("考虑的选项:");
            for option in &decision_record.options_considered {
                println!("  {} - {}", option.id, option.description);
                println!(
                    "    工作量: {:.1}, 成功率: {:.1}%, 创新性: {:.1}, 风险: {:.1}",
                    option.estimated_effort.unwrap_or(0.0),
                    option.success_probability.unwrap_or(0.0) * 100.0,
                    option.innovation_level.unwrap_or(0.0),
                    option.risk_level.unwrap_or(0.0)
                );
            }
        }
    }

    Ok(())
}

pub(super) fn handle_pattern_config(args: &[String]) -> Result<()> {
    let mut pattern_name = None;
    let mut thinking_depth = None;
    let mut enable_metacognition = None;
    let mut learning_rate = None;
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--thinking-depth" => {
                thinking_depth = Some(
                    args.get(index + 1)
                        .context("missing value for --thinking-depth")?
                        .parse::<u8>()
                        .context("invalid thinking-depth value (expected integer)")?,
                );
                index += 2;
            }
            "--metacognition" => {
                let value = args
                    .get(index + 1)
                    .context("missing value for --metacognition")?;
                enable_metacognition = Some(match value.as_str() {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    _ => bail!(
                        "invalid metacognition value: {} (expected true/false)",
                        value
                    ),
                });
                index += 2;
            }
            "--learning-rate" => {
                learning_rate = Some(
                    args.get(index + 1)
                        .context("missing value for --learning-rate")?
                        .parse::<f32>()
                        .context("invalid learning-rate value (expected float)")?,
                );
                index += 2;
            }
            name if pattern_name.is_none() => {
                pattern_name = Some(name.to_string());
                index += 1;
            }
            other => bail!("unexpected argument: {other}"),
        }
    }

    let pattern_name = pattern_name.context("missing pattern name")?;
    let pattern = BehaviorPattern::from_str(&pattern_name)?;

    let mut config = BehaviorConfig::new(pattern.clone());

    // 应用用户指定的配置修改
    if let Some(depth) = thinking_depth {
        config.thinking_depth = depth;
    }
    if let Some(metacognition) = enable_metacognition {
        config.enable_metacognition = metacognition;
    }
    if let Some(rate) = learning_rate {
        config.learning_rate = Some(rate);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!("行为模式配置: {:?}", pattern);
        println!();
        println!("配置参数:");
        println!("  思考深度:     {}", config.thinking_depth);
        println!(
            "  启用元认知:   {}",
            if config.enable_metacognition {
                "是"
            } else {
                "否"
            }
        );
        println!("  学习率:       {:.2}", config.learning_rate.unwrap_or(0.0));
        println!();
        println!("注意: 这只是显示配置，实际保存配置功能需要额外实现");
    }

    Ok(())
}

pub(super) fn handle_pattern_default(args: &[String]) -> Result<()> {
    let mut json = false;
    let mut index = 0usize;

    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected argument: {other}"),
        }
    }

    let default_pattern = BehaviorPattern::get_system_default();

    if json {
        let response = serde_json::json!({
            "system_default_pattern": default_pattern,
            "description": default_pattern.description(),
            "risk_tolerance": default_pattern.risk_tolerance(),
            "innovation_tendency": default_pattern.innovation_tendency(),
            "proactivity": default_pattern.proactivity(),
            "note": "这是系统级别的默认行为模式，所有未指定模式的操作都会使用此模式"
        });
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("系统默认行为模式: {:?}", default_pattern);
        println!();
        println!("模式描述: {}", default_pattern.description());
        println!();
        println!("模式属性:");
        println!("  风险容忍度: {:.1}", default_pattern.risk_tolerance());
        println!("  创新倾向:   {:.1}", default_pattern.innovation_tendency());
        println!("  主动性:     {:.1}", default_pattern.proactivity());
        println!();
        println!("注意: 这是系统级别的默认行为模式，在以下情况下会使用:");
        println!("  - API请求中未指定 behavior_pattern 参数");
        println!("  - 解析指定模式失败时的回退选项");
        println!("  - BehaviorConfig 的默认实例化");
        println!();
        println!(
            "如需修改系统默认值，请在源代码中更改 BehaviorPattern::get_system_default() 的返回值"
        );
    }

    Ok(())
}
