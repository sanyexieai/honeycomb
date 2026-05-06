use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::fs;

/// 标签向量 - 表示多维度的评分
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TagVector {
    pub dimensions: BTreeMap<String, f32>,
}

impl TagVector {
    pub fn new() -> Self {
        Self {
            dimensions: BTreeMap::new(),
        }
    }

    pub fn with_dimension(mut self, dimension: &str, value: f32) -> Self {
        self.dimensions.insert(dimension.to_string(), value.clamp(0.0, 1.0));
        self
    }

    pub fn get(&self, dimension: &str) -> f32 {
        self.dimensions.get(dimension).copied().unwrap_or(0.0)
    }

    pub fn set(&mut self, dimension: &str, value: f32) {
        self.dimensions.insert(dimension.to_string(), value.clamp(0.0, 1.0));
    }

    /// 计算与另一个标签向量的余弦相似度
    pub fn cosine_similarity(&self, other: &TagVector) -> f32 {
        let mut dot_product = 0.0;
        let mut norm_a = 0.0;
        let mut norm_b = 0.0;

        // 收集所有维度
        let mut all_dimensions = std::collections::HashSet::new();
        all_dimensions.extend(self.dimensions.keys());
        all_dimensions.extend(other.dimensions.keys());

        for dimension in all_dimensions {
            let a = self.get(dimension);
            let b = other.get(dimension);
            
            dot_product += a * b;
            norm_a += a * a;
            norm_b += b * b;
        }

        if norm_a == 0.0 || norm_b == 0.0 {
            0.0
        } else {
            dot_product / (norm_a.sqrt() * norm_b.sqrt())
        }
    }

    /// 加权平均合并两个标签向量
    pub fn weighted_merge(&self, other: &TagVector, weight: f32) -> TagVector {
        let weight = weight.clamp(0.0, 1.0);
        let mut result = TagVector::new();

        let mut all_dimensions = std::collections::HashSet::new();
        all_dimensions.extend(self.dimensions.keys());
        all_dimensions.extend(other.dimensions.keys());

        for dimension in all_dimensions {
            let current = self.get(dimension);
            let new_value = other.get(dimension);
            let merged = current * (1.0 - weight) + new_value * weight;
            result.set(dimension, merged);
        }

        result
    }
}

/// 维度定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dimension {
    pub id: String,
    pub name: String,
    pub description: String,
    pub scale_min: f32,
    pub scale_max: f32,
    pub default_value: f32,
    pub keywords: DimensionKeywords,
}

/// 维度关键词映射
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionKeywords {
    pub low: Vec<String>,
    pub medium: Vec<String>,
    pub high: Vec<String>,
}

/// 标签定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub dimension: String,
    pub value: f32,
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub incompatible_tags: Vec<String>,
    pub compatible_weight: f32,
}

/// 实体标签历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTagHistory {
    pub entity_id: String,
    pub entity_type: String,
    pub current_tags: TagVector,
    pub history: Vec<TagHistoryEntry>,
    pub usage_stats: EntityUsageStats,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagHistoryEntry {
    pub timestamp: DateTime<Utc>,
    pub tags: TagVector,
    pub trigger: String, // 触发更新的原因
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityUsageStats {
    pub total_calls: u32,
    pub successful_calls: u32,
    pub user_satisfaction: f32,
    pub avg_response_time: f32,
    pub last_used: Option<DateTime<Utc>>,
}

impl EntityUsageStats {
    pub fn new() -> Self {
        Self {
            total_calls: 0,
            successful_calls: 0,
            user_satisfaction: 0.0,
            avg_response_time: 0.0,
            last_used: None,
        }
    }

    pub fn success_rate(&self) -> f32 {
        if self.total_calls == 0 {
            0.0
        } else {
            self.successful_calls as f32 / self.total_calls as f32
        }
    }
}

/// Room类型枚举
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoomType {
    Dimension,
    Tag,
    EntityTags,
    UserProfile,
    CustomMatcher,
}

/// Room元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMetadata {
    pub room_type: RoomType,
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: BTreeMap<String, String>,
}

/// 标签系统管理器
pub struct TagSystemManager {
    workspace_root: PathBuf,
    dimensions: HashMap<String, Dimension>,
    tags: HashMap<String, Tag>,
    entity_histories: HashMap<String, EntityTagHistory>,
}

impl TagSystemManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            dimensions: HashMap::new(),
            tags: HashMap::new(),
            entity_histories: HashMap::new(),
        }
    }

    pub fn rooms_path(&self) -> PathBuf {
        self.workspace_root.join("rooms")
    }

    pub fn dimensions_path(&self) -> PathBuf {
        self.rooms_path().join("dimensions")
    }

    pub fn tags_path(&self) -> PathBuf {
        self.rooms_path().join("tags")
    }

    pub fn entities_path(&self) -> PathBuf {
        self.rooms_path().join("entities")
    }

    /// 初始化标签系统，扫描所有Room文件
    pub fn initialize(&mut self) -> Result<()> {
        self.ensure_directories_exist()?;
        self.load_dimensions()?;
        self.load_tags()?;
        self.load_entity_histories()?;
        Ok(())
    }

    /// 确保必要的目录存在
    fn ensure_directories_exist(&self) -> Result<()> {
        for path in [
            &self.rooms_path(),
            &self.dimensions_path(),
            &self.tags_path(),
            &self.entities_path(),
            &self.entities_path().join("tools"),
            &self.entities_path().join("memories"),
            &self.entities_path().join("conversations"),
        ] {
            if !path.exists() {
                fs::create_dir_all(path)
                    .with_context(|| format!("Failed to create directory: {}", path.display()))?;
            }
        }
        Ok(())
    }

    /// 加载所有维度定义
    fn load_dimensions(&mut self) -> Result<()> {
        let dimensions_dir = self.dimensions_path();
        if !dimensions_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(&dimensions_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Ok(dimension) = self.parse_dimension_room(&path) {
                    self.dimensions.insert(dimension.id.clone(), dimension);
                }
            }
        }
        Ok(())
    }

    /// 解析维度Room文件
    fn parse_dimension_room(&self, path: &Path) -> Result<Dimension> {
        let content = fs::read_to_string(path)?;
        let (frontmatter, _markdown) = parse_markdown_frontmatter(&content)?;
        
        let dimension = Dimension {
            id: frontmatter.get("dimension_id")
                .ok_or_else(|| anyhow::anyhow!("Missing dimension_id in {}", path.display()))?
                .clone(),
            name: frontmatter.get("name")
                .unwrap_or(&"Unnamed Dimension".to_string())
                .clone(),
            description: frontmatter.get("description")
                .unwrap_or(&"No description".to_string())
                .clone(),
            scale_min: frontmatter.get("scale_min")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            scale_max: frontmatter.get("scale_max")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0),
            default_value: frontmatter.get("default_value")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.5),
            keywords: DimensionKeywords {
                low: parse_keywords_array(frontmatter.get("keywords_low")),
                medium: parse_keywords_array(frontmatter.get("keywords_medium")),
                high: parse_keywords_array(frontmatter.get("keywords_high")),
            },
        };

        Ok(dimension)
    }

    /// 加载所有标签定义
    fn load_tags(&mut self) -> Result<()> {
        let tags_dir = self.tags_path();
        if !tags_dir.exists() {
            return Ok(());
        }

        for dimension_entry in fs::read_dir(&tags_dir)? {
            let dimension_entry = dimension_entry?;
            let dimension_path = dimension_entry.path();
            
            if dimension_path.is_dir() {
                for tag_entry in fs::read_dir(&dimension_path)? {
                    let tag_entry = tag_entry?;
                    let tag_path = tag_entry.path();
                    
                    if tag_path.extension().and_then(|s| s.to_str()) == Some("md") {
                        if let Ok(tag) = self.parse_tag_room(&tag_path) {
                            self.tags.insert(tag.id.clone(), tag);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 解析标签Room文件
    fn parse_tag_room(&self, path: &Path) -> Result<Tag> {
        let content = fs::read_to_string(path)?;
        let (frontmatter, _markdown) = parse_markdown_frontmatter(&content)?;
        
        let tag = Tag {
            id: frontmatter.get("tag_id")
                .ok_or_else(|| anyhow::anyhow!("Missing tag_id in {}", path.display()))?
                .clone(),
            dimension: frontmatter.get("dimension")
                .ok_or_else(|| anyhow::anyhow!("Missing dimension in {}", path.display()))?
                .clone(),
            value: frontmatter.get("value")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.5),
            name: frontmatter.get("name")
                .unwrap_or(&"Unnamed Tag".to_string())
                .clone(),
            description: frontmatter.get("description")
                .unwrap_or(&"No description".to_string())
                .clone(),
            keywords: parse_keywords_array(frontmatter.get("keywords")),
            incompatible_tags: parse_keywords_array(frontmatter.get("incompatible_tags")),
            compatible_weight: frontmatter.get("compatible_weight")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.0),
        };

        Ok(tag)
    }

    /// 加载所有实体标签历史
    fn load_entity_histories(&mut self) -> Result<()> {
        let entities_dir = self.entities_path();
        if !entities_dir.exists() {
            return Ok(());
        }

        for entity_type_entry in fs::read_dir(&entities_dir)? {
            let entity_type_entry = entity_type_entry?;
            let entity_type_path = entity_type_entry.path();
            
            if entity_type_path.is_dir() {
                let entity_type = entity_type_path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                
                for entity_entry in fs::read_dir(&entity_type_path)? {
                    let entity_entry = entity_entry?;
                    let entity_path = entity_entry.path();
                    
                    if entity_path.extension().and_then(|s| s.to_str()) == Some("md") {
                        if let Ok(history) = self.parse_entity_tags_room(&entity_path, &entity_type) {
                            let key = format!("{}:{}", entity_type, history.entity_id);
                            self.entity_histories.insert(key, history);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 解析实体标签Room文件
    fn parse_entity_tags_room(&self, path: &Path, entity_type: &str) -> Result<EntityTagHistory> {
        let content = fs::read_to_string(path)?;
        let (frontmatter, markdown) = parse_markdown_frontmatter(&content)?;
        
        let entity_id = frontmatter.get("entity_id")
            .ok_or_else(|| anyhow::anyhow!("Missing entity_id in {}", path.display()))?
            .clone();

        // 解析当前标签评分
        let current_tags = parse_current_tags_from_markdown(&markdown)?;
        
        // 解析历史记录 
        let history = parse_history_from_markdown(&markdown)?;
        
        // 解析使用统计
        let usage_stats = parse_usage_stats_from_markdown(&markdown)?;

        let history_entry = EntityTagHistory {
            entity_id,
            entity_type: entity_type.to_string(),
            current_tags,
            history,
            usage_stats,
            last_updated: Utc::now(),
        };

        Ok(history_entry)
    }

    /// 根据用户输入分析生成标签向量
    pub fn analyze_input_tags(&self, input: &str) -> TagVector {
        let mut tags = TagVector::new();
        let input_lower = input.to_lowercase();
        let input_words: Vec<&str> = input_lower.split_whitespace().collect();

        // 基于关键词匹配计算每个维度的评分
        for (_, dimension) in &self.dimensions {
            let mut score = dimension.default_value;

            // 改进的关键词匹配 - 支持词汇边界匹配和部分匹配
            let high_matches = dimension.keywords.high.iter()
                .filter(|keyword| {
                    let keyword_lower = keyword.to_lowercase();
                    // 完整词匹配或包含匹配
                    input_words.iter().any(|word| *word == keyword_lower) ||
                    input_lower.contains(&keyword_lower)
                })
                .count();
            
            let medium_matches = dimension.keywords.medium.iter()
                .filter(|keyword| {
                    let keyword_lower = keyword.to_lowercase();
                    input_words.iter().any(|word| *word == keyword_lower) ||
                    input_lower.contains(&keyword_lower)
                })
                .count();
            
            let low_matches = dimension.keywords.low.iter()
                .filter(|keyword| {
                    let keyword_lower = keyword.to_lowercase();
                    input_words.iter().any(|word| *word == keyword_lower) ||
                    input_lower.contains(&keyword_lower)
                })
                .count();

            // 计算加权评分 - 改进权重分配
            if high_matches > 0 {
                // 高复杂度关键词显著提升评分
                score = (score + (high_matches as f32 * 0.25)).min(1.0);
            }
            if medium_matches > 0 {
                // 中等复杂度关键词适度提升评分
                score = (score + (medium_matches as f32 * 0.15)).min(1.0);
            }
            if low_matches > 0 {
                // 低复杂度关键词降低评分
                score = (score - (low_matches as f32 * 0.15)).max(0.0);
            }

            // 特殊逻辑：如果没有匹配任何关键词，使用默认值
            if high_matches == 0 && medium_matches == 0 && low_matches == 0 {
                score = dimension.default_value;
            }

            tags.set(&dimension.id, score);
        }

        tags
    }

    /// 计算实体与查询标签的相似度
    pub fn calculate_entity_similarity(
        &self,
        query_tags: &TagVector,
        entity_id: &str,
        entity_type: &str,
    ) -> f32 {
        let key = format!("{}:{}", entity_type, entity_id);
        
        if let Some(entity_history) = self.entity_histories.get(&key) {
            query_tags.cosine_similarity(&entity_history.current_tags)
        } else {
            0.0 // 未知实体返回0相似度
        }
    }

    /// 更新实体标签
    pub fn update_entity_tags(
        &mut self,
        entity_id: &str,
        entity_type: &str,
        new_tags: &TagVector,
        trigger: &str,
    ) -> Result<()> {
        let key = format!("{}:{}", entity_type, entity_id);
        
        // 先更新内存中的数据
        {
            let history = self.entity_histories.entry(key.clone()).or_insert_with(|| {
                EntityTagHistory {
                    entity_id: entity_id.to_string(),
                    entity_type: entity_type.to_string(),
                    current_tags: TagVector::new(),
                    history: Vec::new(),
                    usage_stats: EntityUsageStats::new(),
                    last_updated: Utc::now(),
                }
            });

            // 记录历史
            history.history.push(TagHistoryEntry {
                timestamp: Utc::now(),
                tags: new_tags.clone(),
                trigger: trigger.to_string(),
            });

            // 更新当前标签(加权平均)
            history.current_tags = history.current_tags.weighted_merge(new_tags, 0.1);
            history.last_updated = Utc::now();
        }

        // 获取更新后的数据并保存到文件
        let history = self.entity_histories.get(&key).unwrap();
        self.save_entity_tags_room(entity_id, entity_type, history)?;

        Ok(())
    }

    /// 保存实体标签Room文件
    fn save_entity_tags_room(
        &self,
        entity_id: &str,
        entity_type: &str,
        history: &EntityTagHistory,
    ) -> Result<()> {
        let path = self.entities_path()
            .join(entity_type)
            .join(format!("{}.md", entity_id));

        // 确保目录存在
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = format_entity_tags_room(history)?;
        fs::write(&path, content)?;

        Ok(())
    }

    /// 获取所有维度
    pub fn get_dimensions(&self) -> &HashMap<String, Dimension> {
        &self.dimensions
    }

    /// 获取所有标签
    pub fn get_tags(&self) -> &HashMap<String, Tag> {
        &self.tags
    }

    /// 获取实体历史
    pub fn get_entity_history(&self, entity_id: &str, entity_type: &str) -> Option<&EntityTagHistory> {
        let key = format!("{}:{}", entity_type, entity_id);
        self.entity_histories.get(&key)
    }
}

// 辅助函数

fn parse_markdown_frontmatter(content: &str) -> Result<(BTreeMap<String, String>, String)> {
    let lines: Vec<&str> = content.lines().collect();
    
    if lines.is_empty() || !lines[0].starts_with("---") {
        return Ok((BTreeMap::new(), content.to_string()));
    }

    let mut frontmatter_end = 0;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.starts_with("---") {
            frontmatter_end = i;
            break;
        }
    }

    if frontmatter_end == 0 {
        return Ok((BTreeMap::new(), content.to_string()));
    }

    let frontmatter_lines = &lines[1..frontmatter_end];
    let markdown_lines = &lines[frontmatter_end + 1..];

    let mut frontmatter = BTreeMap::new();
    for line in frontmatter_lines {
        if let Some((key, value)) = line.split_once(':') {
            frontmatter.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }

    let markdown = markdown_lines.join("\n");
    Ok((frontmatter, markdown))
}

fn parse_keywords_array(value: Option<&String>) -> Vec<String> {
    value
        .map(|s| {
            // 先尝试解析为JSON数组
            if let Ok(array) = serde_json::from_str::<Vec<String>>(s) {
                return array;
            }
            
            // 回退到手动解析
            s.trim_matches(['[', ']', '"'])
                .split(',')
                .map(|item| item.trim().trim_matches('"').to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_current_tags_from_markdown(markdown: &str) -> Result<TagVector> {
    let mut tags = TagVector::new();
    
    // 查找"当前标签评分"部分
    let lines: Vec<&str> = markdown.lines().collect();
    let mut in_tags_section = false;
    
    for line in lines {
        if line.contains("当前标签评分") || line.contains("Current Tag Scores") {
            in_tags_section = true;
            continue;
        }
        
        if in_tags_section {
            if line.starts_with('#') && !line.contains("当前标签评分") {
                break; // 进入下一个section
            }
            
            if line.contains(':') && !line.starts_with('#') {
                if let Some((dimension, value)) = line.split_once(':') {
                    let dimension = dimension.trim().trim_matches('-').trim();
                    let value = value.trim();
                    
                    if let Ok(score) = value.parse::<f32>() {
                        tags.set(dimension, score);
                    }
                }
            }
        }
    }
    
    Ok(tags)
}

fn parse_history_from_markdown(_markdown: &str) -> Result<Vec<TagHistoryEntry>> {
    // 简化实现，实际应该解析JSON或YAML格式的历史记录
    Ok(Vec::new())
}

fn parse_usage_stats_from_markdown(_markdown: &str) -> Result<EntityUsageStats> {
    // 简化实现，实际应该解析统计信息
    Ok(EntityUsageStats::new())
}

fn format_entity_tags_room(history: &EntityTagHistory) -> Result<String> {
    let mut content = String::new();
    
    // Frontmatter
    content.push_str("---\n");
    content.push_str(&format!("room_type: entity_tags\n"));
    content.push_str(&format!("entity_type: {}\n", history.entity_type));
    content.push_str(&format!("entity_id: {}\n", history.entity_id));
    content.push_str(&format!("last_updated: {}\n", history.last_updated.to_rfc3339()));
    content.push_str("---\n\n");
    
    // Title
    content.push_str(&format!("# {} 标签档案\n\n", history.entity_id));
    
    // Current tags
    content.push_str("## 当前标签评分\n\n");
    for (dimension, score) in &history.current_tags.dimensions {
        content.push_str(&format!("- {}: {:.2}\n", dimension, score));
    }
    
    // Usage stats
    content.push_str("\n## 使用统计\n\n");
    content.push_str(&format!("- 总调用次数: {}\n", history.usage_stats.total_calls));
    content.push_str(&format!("- 成功调用次数: {}\n", history.usage_stats.successful_calls));
    content.push_str(&format!("- 成功率: {:.1}%\n", history.usage_stats.success_rate() * 100.0));
    content.push_str(&format!("- 用户满意度: {:.1}/5.0\n", history.usage_stats.user_satisfaction));
    
    if let Some(last_used) = history.usage_stats.last_used {
        content.push_str(&format!("- 最后使用: {}\n", last_used.format("%Y-%m-%d %H:%M:%S")));
    }
    
    // History (simplified)
    if !history.history.is_empty() {
        content.push_str("\n## 标签变更历史\n\n");
        for entry in history.history.iter().take(5) { // 只显示最近5条
            content.push_str(&format!(
                "- {} ({}): 触发原因: {}\n",
                entry.timestamp.format("%m-%d %H:%M"),
                entry.tags.dimensions.len(),
                entry.trigger
            ));
        }
    }
    
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_tag_vector_similarity() {
        let mut vec1 = TagVector::new();
        vec1.set("technical", 0.8);
        vec1.set("creative", 0.3);

        let mut vec2 = TagVector::new();
        vec2.set("technical", 0.9);
        vec2.set("creative", 0.1);

        let similarity = vec1.cosine_similarity(&vec2);
        assert!(similarity > 0.8); // 应该有较高相似度
    }

    #[test]
    fn test_weighted_merge() {
        let mut vec1 = TagVector::new();
        vec1.set("technical", 0.5);

        let mut vec2 = TagVector::new();
        vec2.set("technical", 1.0);

        let merged = vec1.weighted_merge(&vec2, 0.2);
        assert_eq!(merged.get("technical"), 0.6); // 0.5 * 0.8 + 1.0 * 0.2 = 0.6
    }
}