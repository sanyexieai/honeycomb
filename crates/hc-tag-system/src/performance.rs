//! 性能优化模块 - 缓存、预计算和增量更新

use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

use crate::{MatchResult, MatchType, TagAnalysisResult, TagVector};

/// 输入内容的哈希键
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputHashKey {
    pub content_hash: u64,
    pub analysis_type: AnalysisType,
}

/// 分析类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AnalysisType {
    Legacy,   // 传统分析
    Enhanced, // 增强分析
    Detailed, // 详细分析
}

/// 缓存条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub tag_vector: TagVector,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub hit_count: u32,
}

/// 预计算的匹配结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrecomputedMatches {
    pub high_matches: Vec<MatchResult>,
    pub medium_matches: Vec<MatchResult>,
    pub low_matches: Vec<MatchResult>,
    pub total_score_boost: f32,
}

/// 性能优化器
pub struct PerformanceOptimizer {
    // 结果缓存 - 输入 -> 标签向量
    result_cache: LruCache<InputHashKey, CacheEntry>,
    // 预计算的常见词汇匹配结果
    precomputed_matches: HashMap<String, PrecomputedMatches>,
    // 缓存统计
    cache_hits: u64,
    cache_misses: u64,
    // 缓存配置
    cache_config: CacheConfig,
}

/// 缓存配置
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub max_cache_size: usize,
    pub cache_ttl_seconds: i64,
    pub precompute_common_words: bool,
    pub enable_statistics: bool,
}

pub const DEFAULT_MAX_CACHE_SIZE: usize = 1000;
pub const DEFAULT_CACHE_TTL_SECONDS: i64 = 3600;
const FALLBACK_CACHE_SIZE: usize = 100;

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_cache_size: DEFAULT_MAX_CACHE_SIZE,
            cache_ttl_seconds: DEFAULT_CACHE_TTL_SECONDS,
            precompute_common_words: true,
            enable_statistics: true,
        }
    }
}

impl PerformanceOptimizer {
    pub fn new(config: CacheConfig) -> Self {
        let cache_size = NonZeroUsize::new(config.max_cache_size)
            .unwrap_or_else(|| NonZeroUsize::new(FALLBACK_CACHE_SIZE).unwrap());

        Self {
            result_cache: LruCache::new(cache_size),
            precomputed_matches: HashMap::new(),
            cache_hits: 0,
            cache_misses: 0,
            cache_config: config,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(CacheConfig::default())
    }

    /// 计算输入内容的哈希
    pub fn compute_input_hash(&self, input: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        input.hash(&mut hasher);
        hasher.finish()
    }

    /// 从缓存获取结果，如果不存在则返回 None
    pub fn get_cached_result(
        &mut self,
        input: &str,
        analysis_type: AnalysisType,
    ) -> Option<TagVector> {
        let key = InputHashKey {
            content_hash: self.compute_input_hash(input),
            analysis_type,
        };

        if let Some(entry) = self.result_cache.get_mut(&key) {
            // 检查是否过期
            let now = chrono::Utc::now();
            let age = now.timestamp() - entry.timestamp.timestamp();

            if age < self.cache_config.cache_ttl_seconds {
                entry.hit_count += 1;
                if self.cache_config.enable_statistics {
                    self.cache_hits += 1;
                }
                return Some(entry.tag_vector.clone());
            } else {
                // 过期，移除
                self.result_cache.pop(&key);
            }
        }

        if self.cache_config.enable_statistics {
            self.cache_misses += 1;
        }
        None
    }

    /// 缓存分析结果
    pub fn cache_result(&mut self, input: &str, analysis_type: AnalysisType, result: TagVector) {
        let key = InputHashKey {
            content_hash: self.compute_input_hash(input),
            analysis_type,
        };

        let entry = CacheEntry {
            tag_vector: result,
            timestamp: chrono::Utc::now(),
            hit_count: 0,
        };

        self.result_cache.put(key, entry);
    }

    /// 预计算常见词汇的匹配结果
    pub fn precompute_common_matches(&mut self, keywords: &HashMap<String, Vec<String>>) {
        if !self.cache_config.precompute_common_words {
            return;
        }

        let common_words = vec![
            "create",
            "make",
            "build",
            "design",
            "develop",
            "implement",
            "simple",
            "easy",
            "basic",
            "complex",
            "difficult",
            "advanced",
            "urgent",
            "important",
            "critical",
            "routine",
            "normal",
            "standard",
            "innovative",
            "creative",
            "original",
            "traditional",
            "conventional",
        ];

        for word in common_words {
            let mut precomputed = PrecomputedMatches {
                high_matches: Vec::new(),
                medium_matches: Vec::new(),
                low_matches: Vec::new(),
                total_score_boost: 0.0,
            };

            // 为每个维度计算匹配结果
            for (dimension, keyword_lists) in keywords {
                // 这里简化处理，实际应该用 FuzzyMatcher
                if keyword_lists.contains(&word.to_string()) {
                    let match_result = MatchResult {
                        keyword: word.to_string(),
                        score: 1.0,
                        match_type: MatchType::Exact,
                        original_input: word.to_string(),
                    };

                    // 假设放在高权重匹配中
                    precomputed.high_matches.push(match_result);
                    precomputed.total_score_boost += 0.25;
                }
            }

            if precomputed.total_score_boost > 0.0 {
                self.precomputed_matches
                    .insert(word.to_string(), precomputed);
            }
        }
    }

    /// 获取预计算的匹配结果
    pub fn get_precomputed_matches(&self, input: &str) -> Option<&PrecomputedMatches> {
        // 简化版：只查找输入中的第一个单词
        let first_word = input.split_whitespace().next().map(|w| w.to_lowercase());

        if let Some(word) = first_word {
            self.precomputed_matches.get(&word)
        } else {
            None
        }
    }

    /// 获取缓存统计信息
    pub fn get_cache_stats(&self) -> CacheStats {
        CacheStats {
            cache_size: self.result_cache.len(),
            max_cache_size: self.cache_config.max_cache_size,
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
            hit_rate: if (self.cache_hits + self.cache_misses) > 0 {
                self.cache_hits as f32 / (self.cache_hits + self.cache_misses) as f32
            } else {
                0.0
            },
            precomputed_entries: self.precomputed_matches.len(),
        }
    }

    /// 清空缓存
    pub fn clear_cache(&mut self) {
        self.result_cache.clear();
        self.cache_hits = 0;
        self.cache_misses = 0;
    }

    /// 手动清理过期缓存项
    pub fn cleanup_expired(&mut self) {
        let now = chrono::Utc::now();
        let ttl = self.cache_config.cache_ttl_seconds;

        // 收集过期的键
        let expired_keys: Vec<InputHashKey> = self
            .result_cache
            .iter()
            .filter_map(|(key, entry)| {
                let age = now.timestamp() - entry.timestamp.timestamp();
                if age >= ttl { Some(key.clone()) } else { None }
            })
            .collect();

        // 移除过期项
        for key in expired_keys {
            self.result_cache.pop(&key);
        }
    }
}

/// 缓存统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub cache_size: usize,
    pub max_cache_size: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub hit_rate: f32,
    pub precomputed_entries: usize,
}

/// 增量更新器 - 用于增量更新标签评分
pub struct IncrementalUpdater {
    // 之前的分析结果
    previous_results: HashMap<String, TagVector>,
    // 变更检测阈值
    change_threshold: f32,
}

impl IncrementalUpdater {
    pub fn new(change_threshold: f32) -> Self {
        Self {
            previous_results: HashMap::new(),
            change_threshold,
        }
    }

    /// 检查是否需要重新分析（基于输入变化）
    pub fn needs_reanalysis(&self, input: &str) -> bool {
        // 简化版：如果输入之前没有分析过，则需要分析
        !self.previous_results.contains_key(input)
    }

    /// 增量更新结果
    pub fn incremental_update(&mut self, input: &str, new_result: TagVector) -> Option<TagVector> {
        if let Some(previous) = self.previous_results.get(input) {
            let similarity = previous.cosine_similarity(&new_result);
            let max_abs_diff = dimension_max_abs_difference(previous, &new_result);
            if similarity < (1.0 - self.change_threshold) || max_abs_diff > self.change_threshold {
                // 变化较大，更新并返回新结果
                self.previous_results
                    .insert(input.to_string(), new_result.clone());
                Some(new_result)
            } else {
                // 变化不大，返回之前的结果
                Some(previous.clone())
            }
        } else {
            // 首次分析，存储并返回
            self.previous_results
                .insert(input.to_string(), new_result.clone());
            Some(new_result)
        }
    }
}

fn dimension_max_abs_difference(a: &TagVector, b: &TagVector) -> f32 {
    let mut max_d = 0.0f32;
    for key in a.dimensions.keys().chain(b.dimensions.keys()) {
        let d = (a.get(key) - b.get(key)).abs();
        if d > max_d {
            max_d = d;
        }
    }
    max_d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TagVector;

    #[test]
    fn test_cache_operations() {
        let mut optimizer = PerformanceOptimizer::with_defaults();

        // 测试缓存未命中
        assert!(
            optimizer
                .get_cached_result("test input", AnalysisType::Enhanced)
                .is_none()
        );

        // 缓存结果
        let mut tag_vector = TagVector::new();
        tag_vector.set("test_dim", 0.8);
        optimizer.cache_result("test input", AnalysisType::Enhanced, tag_vector.clone());

        // 测试缓存命中
        let cached = optimizer.get_cached_result("test input", AnalysisType::Enhanced);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().get("test_dim"), 0.8);

        // 测试统计信息
        let stats = optimizer.get_cache_stats();
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 1);
        assert_eq!(stats.hit_rate, 0.5);
    }

    #[test]
    fn test_incremental_updater() {
        let mut updater = IncrementalUpdater::new(0.1);

        let mut vec1 = TagVector::new();
        vec1.set("test", 0.5);

        let mut vec2 = TagVector::new();
        vec2.set("test", 0.55); // 小变化

        let mut vec3 = TagVector::new();
        vec3.set("test", 0.8); // 大变化

        // 首次更新
        let result1 = updater.incremental_update("input1", vec1.clone());
        assert!(result1.is_some());

        // 小变化，应该返回之前的结果
        let result2 = updater.incremental_update("input1", vec2);
        assert_eq!(result2.unwrap().get("test"), 0.5); // 返回原值

        // 大变化，应该返回新结果
        let result3 = updater.incremental_update("input1", vec3);
        assert_eq!(result3.unwrap().get("test"), 0.8); // 返回新值
    }
}
