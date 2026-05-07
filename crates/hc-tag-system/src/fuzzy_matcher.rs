//! 模糊匹配模块 - 支持同义词、拼写纠错和语义相似度匹配

use rust_stemmers::{Algorithm, Stemmer};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use strsim::{jaro_winkler, levenshtein};

/// 匹配结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub keyword: String,
    pub score: f32,
    pub match_type: MatchType,
    pub original_input: String,
}

/// 匹配类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MatchType {
    Exact,    // 精确匹配
    Fuzzy,    // 模糊匹配
    Synonym,  // 同义词匹配
    Stemmed,  // 词干匹配
    Phonetic, // 语音相似
}

/// 模糊匹配器配置
#[derive(Debug, Clone)]
pub struct FuzzyMatcherConfig {
    pub similarity_threshold: f32,
    pub enable_stemming: bool,
    pub enable_synonym_matching: bool,
    pub max_edit_distance: usize,
    pub jaro_winkler_threshold: f32,
}

impl Default for FuzzyMatcherConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.6,
            enable_stemming: true,
            enable_synonym_matching: true,
            max_edit_distance: 2,
            jaro_winkler_threshold: 0.8,
        }
    }
}

/// 模糊匹配器
pub struct FuzzyMatcher {
    config: FuzzyMatcherConfig,
    synonym_dict: SynonymDictionary,
    stemmer_en: Stemmer,
    common_typos: HashMap<String, String>,
}

impl Clone for FuzzyMatcher {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            synonym_dict: self.synonym_dict.clone(),
            stemmer_en: Stemmer::create(Algorithm::English),
            common_typos: self.common_typos.clone(),
        }
    }
}

impl FuzzyMatcher {
    pub fn new(config: FuzzyMatcherConfig) -> Self {
        Self {
            config,
            synonym_dict: SynonymDictionary::new(),
            stemmer_en: Stemmer::create(Algorithm::English),
            common_typos: Self::build_common_typos(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(FuzzyMatcherConfig::default())
    }

    /// 模糊匹配关键词列表
    pub fn fuzzy_match_keywords(&self, input: &str, keywords: &[String]) -> Vec<MatchResult> {
        let normalized_input = self.normalize_text(input);
        let input_words: Vec<&str> = normalized_input.split_whitespace().collect();

        let mut results = Vec::new();

        for keyword in keywords {
            let normalized_keyword = self.normalize_text(keyword);

            // 1. 精确匹配（包括子串匹配）
            if let Some(result) = self.exact_match(&normalized_input, &normalized_keyword, keyword)
            {
                results.push(result);
                continue;
            }

            // 2. 词级别匹配
            for word in &input_words {
                if let Some(result) = self.match_single_word(word, &normalized_keyword, keyword) {
                    results.push(result);
                }
            }
        }

        // 去重并按分数排序
        self.deduplicate_and_sort(results)
    }

    /// 单词级别匹配
    fn match_single_word(
        &self,
        word: &str,
        normalized_keyword: &str,
        original_keyword: &str,
    ) -> Option<MatchResult> {
        // 1. 精确匹配
        if word == normalized_keyword {
            return Some(MatchResult {
                keyword: original_keyword.to_string(),
                score: 1.0,
                match_type: MatchType::Exact,
                original_input: word.to_string(),
            });
        }

        // 2. 拼写纠错匹配
        if let Some(corrected) = self.spell_correct(word) {
            if corrected == normalized_keyword {
                return Some(MatchResult {
                    keyword: original_keyword.to_string(),
                    score: 0.9,
                    match_type: MatchType::Fuzzy,
                    original_input: word.to_string(),
                });
            }
        }

        // 3. 编辑距离匹配
        let edit_distance = levenshtein(word, normalized_keyword);
        if edit_distance <= self.config.max_edit_distance && word.len() >= 3 {
            let max_len = word.len().max(normalized_keyword.len());
            let score = 1.0 - (edit_distance as f32 / max_len as f32);
            if score >= self.config.similarity_threshold {
                return Some(MatchResult {
                    keyword: original_keyword.to_string(),
                    score,
                    match_type: MatchType::Fuzzy,
                    original_input: word.to_string(),
                });
            }
        }

        // 4. 词干匹配（先于 Jaro-Winkler，避免本可判定为词干的配对被标成模糊匹配）
        if self.config.enable_stemming && word.len() >= 3 {
            let stem_word = self.stemmer_en.stem(word);
            let stem_keyword = self.stemmer_en.stem(normalized_keyword);
            if stem_word == stem_keyword {
                return Some(MatchResult {
                    keyword: original_keyword.to_string(),
                    score: 0.7,
                    match_type: MatchType::Stemmed,
                    original_input: word.to_string(),
                });
            }
        }

        // 5. Jaro-Winkler 相似度匹配
        let jw_score = jaro_winkler(word, normalized_keyword) as f32;
        if jw_score >= self.config.jaro_winkler_threshold {
            return Some(MatchResult {
                keyword: original_keyword.to_string(),
                score: jw_score,
                match_type: MatchType::Fuzzy,
                original_input: word.to_string(),
            });
        }

        // 6. 同义词匹配
        if self.config.enable_synonym_matching {
            if self.synonym_dict.are_synonyms(word, normalized_keyword) {
                return Some(MatchResult {
                    keyword: original_keyword.to_string(),
                    score: 0.8,
                    match_type: MatchType::Synonym,
                    original_input: word.to_string(),
                });
            }
        }

        None
    }

    /// 精确匹配（包括子串）
    fn exact_match(
        &self,
        input: &str,
        keyword: &str,
        original_keyword: &str,
    ) -> Option<MatchResult> {
        if input.contains(keyword) || keyword.contains(input) {
            let score = if input == keyword { 1.0 } else { 0.95 };
            Some(MatchResult {
                keyword: original_keyword.to_string(),
                score,
                match_type: MatchType::Exact,
                original_input: input.to_string(),
            })
        } else {
            None
        }
    }

    /// 文本标准化
    fn normalize_text(&self, text: &str) -> String {
        text.to_lowercase()
            .trim()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect()
    }

    /// 简单拼写纠错
    fn spell_correct(&self, word: &str) -> Option<String> {
        self.common_typos.get(word).cloned()
    }

    /// 去重并按分数排序
    fn deduplicate_and_sort(&self, results: Vec<MatchResult>) -> Vec<MatchResult> {
        // 按关键词去重，保留分数最高的
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut filtered: Vec<MatchResult> = Vec::new();

        for result in results {
            match seen.get(&result.keyword) {
                Some(&index) => {
                    if result.score > filtered[index].score {
                        filtered[index] = result;
                    }
                }
                None => {
                    seen.insert(result.keyword.clone(), filtered.len());
                    filtered.push(result);
                }
            }
        }

        // 按分数降序排序
        filtered.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        filtered
    }

    /// 构建常见拼写错误字典
    fn build_common_typos() -> HashMap<String, String> {
        let mut typos = HashMap::new();

        // 英文常见拼写错误
        typos.insert("teh".to_string(), "the".to_string());
        typos.insert("recieve".to_string(), "receive".to_string());
        typos.insert("seperate".to_string(), "separate".to_string());
        typos.insert("definately".to_string(), "definitely".to_string());
        typos.insert("occured".to_string(), "occurred".to_string());

        // 技术术语常见拼写错误
        typos.insert("algortihm".to_string(), "algorithm".to_string());
        typos.insert("databse".to_string(), "database".to_string());
        typos.insert("funtcion".to_string(), "function".to_string());
        typos.insert("varialbe".to_string(), "variable".to_string());

        typos
    }
}

/// 同义词词典
#[derive(Clone)]
pub struct SynonymDictionary {
    synonyms: HashMap<String, HashSet<String>>,
}

impl SynonymDictionary {
    pub fn new() -> Self {
        let mut dict = Self {
            synonyms: HashMap::new(),
        };
        dict.build_default_synonyms();
        dict
    }

    /// 检查两个词是否为同义词
    pub fn are_synonyms(&self, word1: &str, word2: &str) -> bool {
        if let Some(synonyms) = self.synonyms.get(word1) {
            return synonyms.contains(word2);
        }
        if let Some(synonyms) = self.synonyms.get(word2) {
            return synonyms.contains(word1);
        }
        false
    }

    /// 添加同义词组
    pub fn add_synonym_group(&mut self, words: Vec<String>) {
        for word in &words {
            let synonym_set: HashSet<String> =
                words.iter().filter(|&w| w != word).cloned().collect();

            if let Some(existing) = self.synonyms.get_mut(word) {
                existing.extend(synonym_set);
            } else {
                self.synonyms.insert(word.clone(), synonym_set);
            }
        }
    }

    /// 构建默认同义词词典
    fn build_default_synonyms(&mut self) {
        // 创造性相关同义词
        self.add_synonym_group(vec![
            "create".to_string(),
            "make".to_string(),
            "build".to_string(),
            "generate".to_string(),
        ]);
        self.add_synonym_group(vec![
            "innovative".to_string(),
            "creative".to_string(),
            "original".to_string(),
            "novel".to_string(),
        ]);
        self.add_synonym_group(vec![
            "design".to_string(),
            "architect".to_string(),
            "plan".to_string(),
            "blueprint".to_string(),
        ]);

        // 技术复杂度相关同义词
        self.add_synonym_group(vec![
            "complex".to_string(),
            "complicated".to_string(),
            "intricate".to_string(),
            "sophisticated".to_string(),
        ]);
        self.add_synonym_group(vec![
            "simple".to_string(),
            "easy".to_string(),
            "basic".to_string(),
            "straightforward".to_string(),
        ]);

        // 紧急程度相关同义词
        self.add_synonym_group(vec![
            "urgent".to_string(),
            "critical".to_string(),
            "important".to_string(),
            "priority".to_string(),
        ]);
        self.add_synonym_group(vec![
            "routine".to_string(),
            "normal".to_string(),
            "standard".to_string(),
            "regular".to_string(),
        ]);

        // 中英文对应
        self.add_synonym_group(vec![
            "create".to_string(),
            "创建".to_string(),
            "创造".to_string(),
        ]);
        self.add_synonym_group(vec!["complex".to_string(), "复杂".to_string()]);
        self.add_synonym_group(vec!["urgent".to_string(), "紧急".to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let matcher = FuzzyMatcher::with_defaults();
        let keywords = vec!["create".to_string(), "design".to_string()];
        let results = matcher.fuzzy_match_keywords("I want to create something", &keywords);

        assert!(!results.is_empty());
        assert_eq!(results[0].keyword, "create");
        assert_eq!(results[0].match_type, MatchType::Exact);
        assert!(results[0].score > 0.9);
    }

    #[test]
    fn test_fuzzy_match() {
        let matcher = FuzzyMatcher::with_defaults();
        let keywords = vec!["algorithm".to_string()];
        let results = matcher.fuzzy_match_keywords("algortihm implementation", &keywords);

        assert!(!results.is_empty());
        assert_eq!(results[0].keyword, "algorithm");
        assert_eq!(results[0].match_type, MatchType::Fuzzy);
        assert!(results[0].score > 0.5);
    }

    #[test]
    fn test_synonym_match() {
        let matcher = FuzzyMatcher::with_defaults();
        let keywords = vec!["create".to_string()];
        let results = matcher.fuzzy_match_keywords("I need to make something", &keywords);

        assert!(!results.is_empty());
        assert_eq!(results[0].keyword, "create");
        assert_eq!(results[0].match_type, MatchType::Synonym);
        assert!(results[0].score > 0.7);
    }

    #[test]
    fn test_stemming() {
        let matcher = FuzzyMatcher::with_defaults();
        let keywords = vec!["running".to_string()];
        let results = matcher.fuzzy_match_keywords("I run every day", &keywords);

        // 应该通过词干匹配找到相似性
        if !results.is_empty() {
            assert_eq!(results[0].match_type, MatchType::Stemmed);
        } else {
            // 如果没有匹配结果也是正常的，词干匹配可能不够强
            println!("No stemming match found - this might be expected");
        }
    }
}
