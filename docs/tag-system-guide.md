# 蜂巢标签系统 (Honeycomb Tag System)

## 概述

蜂巢标签系统是一个基于Room文件的动态维度标签管理系统，通过Markdown文件定义维度、标签和实体关联关系，实现零硬编码的灵活配置和实时热更新。

## 核心概念

### 1. 维度 (Dimensions)
维度定义了评估实体的不同方面，如技术复杂度、创造性需求、紧急程度等。

**文件位置**: `workspace/tenants/{tenant}/users/{user}/rooms/dimensions/`

**示例**: `technical_complexity.md`
```markdown
---
room_type: dimension
dimension_id: technical_complexity
name: 技术复杂度维度
description: 评估任务或内容的技术难度和复杂性
scale_min: 0.0
scale_max: 1.0
default_value: 0.5
keywords_low: ["basic", "simple", "easy", "简单", "基础"]
keywords_medium: ["moderate", "standard", "中等", "普通"]
keywords_high: ["complex", "advanced", "复杂", "高级", "算法"]
---
```

### 2. 标签 (Tags)
标签是维度上的具体取值点，代表某个维度的特定级别。

**文件位置**: `workspace/tenants/{tenant}/users/{user}/rooms/tags/{dimension}/`

**示例**: `tags/technical_complexity/high.md`
```markdown
---
room_type: tag
dimension: technical_complexity
tag_id: high_technical
value: 0.85
name: 高技术复杂度
description: 需要深度技术知识和专业技能的复杂任务
keywords: ["system", "algorithm", "architecture"]
incompatible_tags: ["low_technical", "simple_technical"]
compatible_weight: 1.0
---
```

### 3. 实体标签 (Entity Tags)
记录具体实体（工具、记忆、对话等）的标签评分历史和使用统计。

**文件位置**: `workspace/tenants/{tenant}/users/{user}/rooms/entities/{entity_type}/`

**示例**: `entities/tools/code_generation.md`
```markdown
---
room_type: entity_tags
entity_type: tools
entity_id: code_generation
last_updated: 2026-05-06T09:39:00Z
---

# code_generation 工具标签档案

## 当前标签评分

- technical_complexity: 0.75
- creativity_level: 0.65
- urgency: 0.4

## 使用统计

- 总调用次数: 89
- 成功率: 85.4%
- 用户满意度: 4.1/5.0
```

## 系统架构

### TagSystemManager
核心管理器，负责：
- 扫描和解析Room文件
- 维护维度、标签和实体数据
- 提供标签分析和相似度计算

### TagVector
多维度标签向量，支持：
- 余弦相似度计算
- 加权平均合并
- 维度评分管理

### 关键算法

#### 1. 输入文本标签分析
```rust
pub fn analyze_input_tags(&self, input: &str) -> TagVector {
    // 1. 分词和预处理
    let input_lower = input.to_lowercase();
    let input_words: Vec<&str> = input_lower.split_whitespace().collect();
    
    // 2. 关键词匹配评分
    for dimension in &self.dimensions {
        let high_matches = dimension.keywords.high.iter()
            .filter(|keyword| matches_keyword(&input_lower, &input_words, keyword))
            .count();
        
        // 3. 加权评分计算
        if high_matches > 0 {
            score = (score + (high_matches as f32 * 0.25)).min(1.0);
        }
        // ...
    }
}
```

#### 2. 实体相似度计算
```rust
pub fn calculate_entity_similarity(
    &self,
    query_tags: &TagVector,
    entity_id: &str,
    entity_type: &str,
) -> f32 {
    if let Some(entity_history) = self.entity_histories.get(&key) {
        query_tags.cosine_similarity(&entity_history.current_tags)
    } else {
        0.0
    }
}
```

## 使用指南

### 1. 初始化标签系统

```rust
use hc_tag_system::TagSystemManager;
use std::path::PathBuf;

let workspace_root = PathBuf::from("workspace/tenants/local/users/default");
let mut manager = TagSystemManager::new(workspace_root);
manager.initialize()?;
```

### 2. 分析用户输入

```rust
let input = "创建一个复杂的算法来优化系统性能";
let tags = manager.analyze_input_tags(input);

// 输出: 
// technical_complexity = 1.00
// creativity_level = 0.70
// urgency = 0.40
```

### 3. 计算实体相似度

```rust
let similarity = manager.calculate_entity_similarity(
    &query_tags,
    "code_generation",
    "tools"
);
// 输出: 0.95 (高相似度)
```

### 4. 更新实体标签

```rust
manager.update_entity_tags(
    "code_generation",
    "tools", 
    &new_tags,
    "user_feedback"
)?;
```

## 配置示例

### 维度配置最佳实践

1. **技术复杂度维度**
   - 低: 基础操作、文件管理、简单查询
   - 中: 配置管理、数据处理、脚本编写  
   - 高: 系统架构、算法实现、性能优化

2. **创造性需求维度**
   - 低: 重复任务、模板使用、标准流程
   - 中: 定制化、改进优化、问题适配
   - 高: 原创设计、创新方案、艺术创作

3. **紧急程度维度**
   - 低: 背景任务、长期规划、学习研究
   - 中: 常规开发、计划内任务、定期维护
   - 高: 紧急修复、关键问题、时间敏感

### 关键词策略

#### 中英文混合
支持中英文关键词混合使用，提高匹配准确性：
```yaml
keywords_high: [
  "complex", "advanced", "expert",
  "复杂", "高级", "专家", "算法", "架构"
]
```

#### 领域特定词汇
根据业务领域添加专业词汇：
```yaml
# 开发领域
keywords_high: ["algorithm", "architecture", "optimization", "微服务", "分布式"]

# 设计领域  
keywords_high: ["creative", "innovative", "artistic", "创意", "设计", "美学"]
```

## 性能优化

### 1. 缓存策略
- 维度和标签定义缓存在内存中
- 实体标签按需加载和更新
- 文件变更监控自动刷新缓存

### 2. 批量操作
```rust
// 批量更新多个实体标签
for (entity_id, new_tags) in batch_updates {
    manager.update_entity_tags(entity_id, entity_type, &new_tags, "batch_update")?;
}
```

### 3. 异步处理
```rust
// 异步文件写入，避免阻塞主流程
tokio::spawn(async move {
    manager.save_entity_tags_room(entity_id, entity_type, &history).await
});
```

## 扩展能力

### 1. 自定义维度
通过添加新的维度Room文件扩展评估体系：
- 情感倾向 (sentiment)
- 交互复杂度 (interaction_complexity)  
- 数据敏感性 (data_sensitivity)

### 2. 机器学习集成
- 基于历史数据训练标签预测模型
- 用户行为分析优化关键词权重
- A/B测试不同标签策略效果

### 3. 多租户支持
- 租户级别的标签系统隔离
- 跨租户标签模板共享
- 权限控制和访问管理

## 监控和调试

### 1. 日志记录
```rust
// 关键操作日志
log::info!("Tag analysis: input='{}', result={:?}", input, tags);
log::debug!("Entity similarity: {}:{} vs query = {:.2}", entity_type, entity_id, similarity);
```

### 2. 统计指标
- 标签匹配准确率
- 相似度计算性能
- 用户满意度反馈

### 3. 健康检查
```rust
// 检查标签系统状态
pub fn health_check(&self) -> SystemHealth {
    SystemHealth {
        dimensions_loaded: self.dimensions.len(),
        tags_loaded: self.tags.len(), 
        entities_tracked: self.entity_histories.len(),
        last_update: self.last_scan_time,
    }
}
```

## 总结

蜂巢标签系统通过Room文件架构实现了：

1. **零硬编码**: 所有配置通过Markdown文件管理
2. **实时更新**: 文件变更自动生效，无需重启
3. **多维评估**: 支持任意数量的自定义维度
4. **智能匹配**: 基于关键词和相似度的双重算法
5. **历史追踪**: 完整记录标签变更和使用统计
6. **扩展性强**: 易于添加新维度、标签和实体类型

这个系统为AI助手提供了强大的上下文理解和工具选择能力，能够根据用户输入的语义特征动态匹配最合适的工具和资源。