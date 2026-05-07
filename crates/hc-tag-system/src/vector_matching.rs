//! 向量化匹配模块 - 集成轻量级模型进行语义匹配

use std::collections::HashMap;
use ndarray::Array1;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{TagVector, Dimension};

/// 向量化匹配器
pub struct VectorMatcher {
    embedding_model: Box<dyn EmbeddingModel>,
    dimension_embeddings: HashMap<String, Array1<f32>>,
    keyword_embeddings: HashMap<String, Array1<f32>>,
    config: VectorMatcherConfig,
}

/// 向量匹配器配置
#[derive(Debug, Clone)]
pub struct VectorMatcherConfig {
    pub embedding_dim: usize,
    pub similarity_threshold: f32,
    pub cache_embeddings: bool,
    pub normalize_vectors: bool,
    /// 是否允许使用内置 mock embedding。生产环境可设为 false，避免伪语义结果被误用。
    pub allow_mock_model: bool,
    pub model_type: ModelType,
}

/// 模型类型
#[derive(Debug, Clone, PartialEq)]
pub enum ModelType {
    Local(LocalModelConfig),      // 本地模型
    Remote(RemoteModelConfig),    // 远程API模型
    Mock(MockModelConfig),        // 模拟模型（用于测试）
}

/// 本地模型配置
#[derive(Debug, Clone, PartialEq)]
pub struct LocalModelConfig {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub model_name: String,
}

/// 远程模型配置
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteModelConfig {
    pub api_endpoint: String,
    pub api_key: Option<String>,
    pub model_name: String,
    pub max_retries: u32,
}

/// 模拟模型配置
#[derive(Debug, Clone, PartialEq)]
pub struct MockModelConfig {
    pub embedding_dim: usize,
    pub use_random: bool,
}

pub const DEFAULT_EMBEDDING_DIM: usize = 384;
pub const DEFAULT_VECTOR_SIMILARITY_THRESHOLD: f32 = 0.7;

impl Default for VectorMatcherConfig {
    fn default() -> Self {
        Self {
            embedding_dim: DEFAULT_EMBEDDING_DIM,
            similarity_threshold: DEFAULT_VECTOR_SIMILARITY_THRESHOLD,
            cache_embeddings: true,
            normalize_vectors: true,
            allow_mock_model: true,
            model_type: ModelType::Mock(MockModelConfig {
                embedding_dim: DEFAULT_EMBEDDING_DIM,
                use_random: false,
            }),
        }
    }
}

/// 嵌入模型接口
pub trait EmbeddingModel: Send + Sync {
    /// 生成文本嵌入向量
    fn encode(&self, text: &str) -> Result<Array1<f32>, EmbeddingError>;
    
    /// 批量生成嵌入向量
    fn encode_batch(&self, texts: &[&str]) -> Result<Vec<Array1<f32>>, EmbeddingError>;
    
    /// 获取嵌入维度
    fn embedding_dim(&self) -> usize;
    
    /// 模型名称
    fn model_name(&self) -> &str;
}

/// 嵌入错误类型
#[derive(Debug, Clone)]
pub enum EmbeddingError {
    ModelLoadError(String),
    EncodingError(String),
    NetworkError(String),
    InvalidInput(String),
}

impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbeddingError::ModelLoadError(msg) => write!(f, "模型加载错误: {}", msg),
            EmbeddingError::EncodingError(msg) => write!(f, "编码错误: {}", msg),
            EmbeddingError::NetworkError(msg) => write!(f, "网络错误: {}", msg),
            EmbeddingError::InvalidInput(msg) => write!(f, "输入无效: {}", msg),
        }
    }
}

impl std::error::Error for EmbeddingError {}

/// 向量匹配结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorMatchResult {
    pub dimension_id: String,
    pub similarity_score: f32,
    pub matched_keywords: Vec<KeywordMatch>,
    pub semantic_context: SemanticContext,
}

/// 关键词匹配结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordMatch {
    pub keyword: String,
    pub similarity: f32,
    pub weight_category: String, // high, medium, low
}

/// 语义上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticContext {
    pub dominant_themes: Vec<String>,
    pub semantic_density: f32,
    pub context_coherence: f32,
}

impl VectorMatcher {
    /// 创建新的向量匹配器
    pub fn new(config: VectorMatcherConfig) -> Result<Self, EmbeddingError> {
        let embedding_model = Self::create_embedding_model(&config)?;
        
        Ok(Self {
            embedding_model,
            dimension_embeddings: HashMap::new(),
            keyword_embeddings: HashMap::new(),
            config,
        })
    }

    /// 创建嵌入模型
    fn create_embedding_model(config: &VectorMatcherConfig) -> Result<Box<dyn EmbeddingModel>, EmbeddingError> {
        match &config.model_type {
            ModelType::Mock(mock_config) => {
                if !config.allow_mock_model {
                    return Err(EmbeddingError::ModelLoadError(
                        "当前配置禁止使用 mock embedding 模型".to_string(),
                    ));
                }
                Ok(Box::new(MockEmbeddingModel::new(mock_config.clone())))
            }
            #[cfg(feature = "embedding")]
            ModelType::Remote(remote_config) => {
                Ok(Box::new(RemoteEmbeddingModel::new(remote_config.clone())?))
            }
            #[cfg(not(feature = "embedding"))]
            ModelType::Remote(_) => {
                Err(EmbeddingError::ModelLoadError(
                    "远程嵌入功能需要启用 'embedding' 特性".to_string()
                ))
            }
            ModelType::Local(_) => {
                Err(EmbeddingError::ModelLoadError(
                    "本地模型支持尚未实现".to_string()
                ))
            }
        }
    }

    /// 预计算维度和关键词的嵌入向量
    pub fn precompute_embeddings(&mut self, dimensions: &HashMap<String, Dimension>) -> Result<(), EmbeddingError> {
        for (dimension_id, dimension) in dimensions {
            // 预计算维度描述的嵌入
            let dimension_text = format!("{}: {}", dimension.name, dimension.description);
            let dimension_embedding = self.embedding_model.encode(&dimension_text)?;
            self.dimension_embeddings.insert(dimension_id.clone(), dimension_embedding);

            // 预计算关键词嵌入
            let all_keywords: Vec<&str> = dimension.keywords.high.iter()
                .chain(dimension.keywords.medium.iter())
                .chain(dimension.keywords.low.iter())
                .map(|s| s.as_str())
                .collect();

            let keyword_embeddings = self.embedding_model.encode_batch(&all_keywords)?;
            
            for (keyword, embedding) in all_keywords.iter().zip(keyword_embeddings.into_iter()) {
                self.keyword_embeddings.insert(keyword.to_string(), embedding);
            }
        }

        Ok(())
    }

    /// 基于向量相似度进行匹配
    pub fn vector_match(&self, input: &str, dimensions: &HashMap<String, Dimension>) -> Result<Vec<VectorMatchResult>, EmbeddingError> {
        // 生成输入文本的嵌入向量
        let input_embedding = self.embedding_model.encode(input)?;
        
        let mut results = Vec::new();
        
        for (dimension_id, dimension) in dimensions {
            let match_result = self.match_dimension(&input_embedding, dimension_id, dimension)?;
            if match_result.similarity_score >= self.config.similarity_threshold {
                results.push(match_result);
            }
        }

        // 按相似度排序
        results.sort_by(|a, b| b.similarity_score.partial_cmp(&a.similarity_score).unwrap_or(std::cmp::Ordering::Equal));
        
        Ok(results)
    }

    /// 匹配单个维度
    fn match_dimension(&self, input_embedding: &Array1<f32>, dimension_id: &str, dimension: &Dimension) -> Result<VectorMatchResult, EmbeddingError> {
        let mut matched_keywords = Vec::new();
        let mut total_similarity = 0.0f32;
        let mut keyword_count = 0;

        // 匹配高权重关键词
        for keyword in &dimension.keywords.high {
            if let Some(keyword_embedding) = self.keyword_embeddings.get(keyword) {
                let similarity = self.cosine_similarity(input_embedding, keyword_embedding);
                if similarity >= self.config.similarity_threshold {
                    matched_keywords.push(KeywordMatch {
                        keyword: keyword.clone(),
                        similarity,
                        weight_category: "high".to_string(),
                    });
                    total_similarity += similarity * 1.5; // 高权重词加权
                    keyword_count += 1;
                }
            }
        }

        // 匹配中权重关键词
        for keyword in &dimension.keywords.medium {
            if let Some(keyword_embedding) = self.keyword_embeddings.get(keyword) {
                let similarity = self.cosine_similarity(input_embedding, keyword_embedding);
                if similarity >= self.config.similarity_threshold {
                    matched_keywords.push(KeywordMatch {
                        keyword: keyword.clone(),
                        similarity,
                        weight_category: "medium".to_string(),
                    });
                    total_similarity += similarity;
                    keyword_count += 1;
                }
            }
        }

        // 匹配低权重关键词
        for keyword in &dimension.keywords.low {
            if let Some(keyword_embedding) = self.keyword_embeddings.get(keyword) {
                let similarity = self.cosine_similarity(input_embedding, keyword_embedding);
                if similarity >= self.config.similarity_threshold {
                    matched_keywords.push(KeywordMatch {
                        keyword: keyword.clone(),
                        similarity,
                        weight_category: "low".to_string(),
                    });
                    total_similarity += similarity * 0.5; // 低权重词降权
                    keyword_count += 1;
                }
            }
        }

        // 计算维度级别的相似度
        let dimension_similarity = if let Some(dimension_embedding) = self.dimension_embeddings.get(dimension_id) {
            self.cosine_similarity(input_embedding, dimension_embedding)
        } else {
            0.0
        };

        // 综合相似度分数
        let final_similarity = if keyword_count > 0 {
            let keyword_avg = total_similarity / keyword_count as f32;
            (keyword_avg * 0.7 + dimension_similarity * 0.3).min(1.0)
        } else {
            dimension_similarity
        };

        // 生成语义上下文
        let semantic_context = self.analyze_semantic_context(&matched_keywords);

        Ok(VectorMatchResult {
            dimension_id: dimension_id.to_string(),
            similarity_score: final_similarity,
            matched_keywords,
            semantic_context,
        })
    }

    /// 计算余弦相似度
    fn cosine_similarity(&self, a: &Array1<f32>, b: &Array1<f32>) -> f32 {
        if a.len() != b.len() {
            return 0.0;
        }

        let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            0.0
        } else {
            dot_product / (norm_a * norm_b)
        }
    }

    /// 分析语义上下文
    fn analyze_semantic_context(&self, matched_keywords: &[KeywordMatch]) -> SemanticContext {
        // 简化的语义分析
        let dominant_themes = matched_keywords.iter()
            .filter(|m| m.similarity > 0.8)
            .map(|m| m.keyword.clone())
            .collect();

        let semantic_density = if matched_keywords.is_empty() {
            0.0
        } else {
            matched_keywords.iter().map(|m| m.similarity).sum::<f32>() / matched_keywords.len() as f32
        };

        let context_coherence = if matched_keywords.len() >= 2 {
            // 计算关键词之间的一致性
            let mut coherence_sum = 0.0f32;
            let mut pairs = 0;
            
            for i in 0..matched_keywords.len() {
                for j in (i+1)..matched_keywords.len() {
                    if matched_keywords[i].weight_category == matched_keywords[j].weight_category {
                        coherence_sum += 0.1;
                    }
                    pairs += 1;
                }
            }
            
            if pairs > 0 { coherence_sum / pairs as f32 } else { 0.0 }
        } else {
            0.5
        };

        SemanticContext {
            dominant_themes,
            semantic_density,
            context_coherence,
        }
    }

    /// 将向量匹配结果转换为标签向量
    pub fn vector_results_to_tag_vector(&self, vector_results: &[VectorMatchResult], dimensions: &HashMap<String, Dimension>) -> TagVector {
        let mut tag_vector = TagVector::new();

        for result in vector_results {
            if let Some(dimension) = dimensions.get(&result.dimension_id) {
                // 基于相似度和语义密度计算最终分数
                let base_score = result.similarity_score;
                let semantic_boost = result.semantic_context.semantic_density * 0.2;
                let coherence_boost = result.semantic_context.context_coherence * 0.1;
                
                let final_score = (base_score + semantic_boost + coherence_boost).min(1.0);
                
                // 应用维度默认值加权
                let adjusted_score = (dimension.default_value * 0.3 + final_score * 0.7).clamp(0.0, 1.0);
                
                tag_vector.set(&result.dimension_id, adjusted_score);
            }
        }

        tag_vector
    }

    /// 获取嵌入向量缓存状态
    pub fn get_cache_info(&self) -> CacheInfo {
        CacheInfo {
            dimension_embeddings_count: self.dimension_embeddings.len(),
            keyword_embeddings_count: self.keyword_embeddings.len(),
            model_name: self.embedding_model.model_name().to_string(),
            embedding_dim: self.embedding_model.embedding_dim(),
        }
    }
}

/// 缓存信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheInfo {
    pub dimension_embeddings_count: usize,
    pub keyword_embeddings_count: usize,
    pub model_name: String,
    pub embedding_dim: usize,
}

/// 模拟嵌入模型（用于测试和开发）
pub struct MockEmbeddingModel {
    config: MockModelConfig,
    word_embeddings: HashMap<String, Array1<f32>>,
}

impl MockEmbeddingModel {
    pub fn new(config: MockModelConfig) -> Self {
        let mut model = Self {
            config,
            word_embeddings: HashMap::new(),
        };
        model.initialize_mock_embeddings();
        model
    }

    /// 初始化模拟嵌入向量
    fn initialize_mock_embeddings(&mut self) {
        let keywords = [
            // 创造性相关
            "create", "创建", "design", "设计", "innovative", "创新", "invent", "发明",
            "original", "原创", "creative", "创造", "brainstorm", "头脑风暴",
            // 复杂性相关
            "complex", "复杂", "difficult", "困难", "advanced", "高级", "sophisticated", "精密",
            "intricate", "复杂的", "technical", "技术", "detailed", "详细",
            // 紧急性相关
            "urgent", "紧急", "critical", "关键", "important", "重要", "priority", "优先",
            "asap", "立即", "quickly", "快速", "immediate", "马上",
            // 基础词汇
            "simple", "简单", "easy", "容易", "basic", "基础", "routine", "例行",
            "standard", "标准", "normal", "正常", "regular", "常规",
        ];

        for (i, keyword) in keywords.iter().enumerate() {
            let embedding = if self.config.use_random {
                self.generate_random_embedding()
            } else {
                self.generate_deterministic_embedding(keyword, i)
            };
            self.word_embeddings.insert(keyword.to_string(), embedding);
        }
    }

    /// 生成确定性的嵌入向量（用于测试）
    fn generate_deterministic_embedding(&self, word: &str, index: usize) -> Array1<f32> {
        let mut embedding = Array1::zeros(self.config.embedding_dim);
        
        // 基于词汇特征生成向量
        let word_hash = word.chars().map(|c| c as u32).sum::<u32>() as f32;
        
        for i in 0..self.config.embedding_dim {
            let value = ((word_hash + i as f32 + index as f32).sin() * 0.5).abs();
            embedding[i] = value;
        }

        // 归一化
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            embedding.mapv_inplace(|x| x / norm);
        }

        embedding
    }

    /// 生成随机嵌入向量
    fn generate_random_embedding(&self) -> Array1<f32> {
        // 简化的随机生成（实际应用中应使用更好的随机数生成器）
        let mut embedding = Array1::zeros(self.config.embedding_dim);
        for i in 0..self.config.embedding_dim {
            embedding[i] = (i as f32).sin() * 0.5;
        }
        embedding
    }

    /// 基于词汇相似性计算嵌入向量
    fn compute_embedding_for_text(&self, text: &str) -> Array1<f32> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut combined_embedding = Array1::zeros(self.config.embedding_dim);
        let mut found_words = 0;

        for word in words {
            if let Some(word_embedding) = self.word_embeddings.get(&word.to_lowercase()) {
                combined_embedding += word_embedding;
                found_words += 1;
            }
        }

        if found_words > 0 {
            combined_embedding /= found_words as f32;
        } else {
            // 如果没有找到已知词汇，生成基于文本哈希的嵌入
            combined_embedding = self.generate_deterministic_embedding(text, 0);
        }

        combined_embedding
    }
}

impl EmbeddingModel for MockEmbeddingModel {
    fn encode(&self, text: &str) -> Result<Array1<f32>, EmbeddingError> {
        if text.is_empty() {
            return Err(EmbeddingError::InvalidInput("输入文本不能为空".to_string()));
        }

        Ok(self.compute_embedding_for_text(text))
    }

    fn encode_batch(&self, texts: &[&str]) -> Result<Vec<Array1<f32>>, EmbeddingError> {
        let mut results = Vec::new();
        for text in texts {
            results.push(self.encode(text)?);
        }
        Ok(results)
    }

    fn embedding_dim(&self) -> usize {
        self.config.embedding_dim
    }

    fn model_name(&self) -> &str {
        "MockEmbeddingModel"
    }
}

/// 远程嵌入模型（需要启用 embedding 特性）
#[cfg(feature = "embedding")]
pub struct RemoteEmbeddingModel {
    config: RemoteModelConfig,
    client: reqwest::Client,
}

#[cfg(feature = "embedding")]
impl RemoteEmbeddingModel {
    pub fn new(config: RemoteModelConfig) -> Result<Self, EmbeddingError> {
        let client = reqwest::Client::new();
        Ok(Self { config, client })
    }
}

#[cfg(feature = "embedding")]
impl EmbeddingModel for RemoteEmbeddingModel {
    fn encode(&self, text: &str) -> Result<Array1<f32>, EmbeddingError> {
        // 这里应该实现实际的远程API调用
        // 为了简化，现在返回模拟结果
        Err(EmbeddingError::EncodingError("远程嵌入服务尚未实现".to_string()))
    }

    fn encode_batch(&self, texts: &[&str]) -> Result<Vec<Array1<f32>>, EmbeddingError> {
        let mut results = Vec::new();
        for text in texts {
            results.push(self.encode(text)?);
        }
        Ok(results)
    }

    fn embedding_dim(&self) -> usize {
        384 // 默认维度
    }

    fn model_name(&self) -> &str {
        &self.config.model_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dimension, DimensionKeywords};

    fn create_test_dimensions() -> HashMap<String, Dimension> {
        let mut dimensions = HashMap::new();
        
        dimensions.insert("creativity_level".to_string(), Dimension {
            id: "creativity_level".to_string(),
            name: "Creativity Level".to_string(),
            description: "Measures creative and innovative aspects".to_string(),
            scale_min: 0.0,
            scale_max: 1.0,
            default_value: 0.3,
            keywords: DimensionKeywords {
                low: vec!["copy".to_string(), "duplicate".to_string()],
                medium: vec!["modify".to_string(), "improve".to_string()],
                high: vec!["create".to_string(), "innovative".to_string(), "design".to_string()],
            },
        });

        dimensions
    }

    #[test]
    fn test_vector_matcher_creation() {
        let config = VectorMatcherConfig::default();
        let matcher = VectorMatcher::new(config);
        assert!(matcher.is_ok());
    }

    #[test]
    fn test_mock_embedding_model() {
        let config = MockModelConfig {
            embedding_dim: 128,
            use_random: false,
        };
        let model = MockEmbeddingModel::new(config);
        
        let embedding = model.encode("create innovative design").unwrap();
        assert_eq!(embedding.len(), 128);
        
        // 测试批量编码
        let batch_result = model.encode_batch(&["create", "design", "innovative"]).unwrap();
        assert_eq!(batch_result.len(), 3);
        assert_eq!(batch_result[0].len(), 128);
    }

    #[test]
    fn test_vector_matching() {
        let dimensions = create_test_dimensions();
        let mut matcher = VectorMatcher::new(VectorMatcherConfig::default()).unwrap();
        
        // 预计算嵌入向量
        assert!(matcher.precompute_embeddings(&dimensions).is_ok());
        
        // 测试向量匹配
        let results = matcher.vector_match("create innovative design solutions", &dimensions).unwrap();
        assert!(!results.is_empty());
        
        // 验证结果包含创造性维度
        let creativity_result = results.iter()
            .find(|r| r.dimension_id == "creativity_level");
        assert!(creativity_result.is_some());
        
        let creativity_match = creativity_result.unwrap();
        assert!(creativity_match.similarity_score > 0.0);
        assert!(!creativity_match.matched_keywords.is_empty());
    }

    #[test]
    fn test_vector_to_tag_conversion() {
        let dimensions = create_test_dimensions();
        let mut matcher = VectorMatcher::new(VectorMatcherConfig::default()).unwrap();
        
        matcher.precompute_embeddings(&dimensions).unwrap();
        let results = matcher.vector_match("design creative solutions", &dimensions).unwrap();
        let tag_vector = matcher.vector_results_to_tag_vector(&results, &dimensions);
        
        assert!(tag_vector.get("creativity_level") > 0.0);
        assert!(tag_vector.get("creativity_level") <= 1.0);
    }

    #[test]
    fn test_cosine_similarity() {
        let matcher = VectorMatcher::new(VectorMatcherConfig::default()).unwrap();
        
        let vec1 = Array1::from(vec![1.0, 0.0, 0.0]);
        let vec2 = Array1::from(vec![0.0, 1.0, 0.0]);
        let vec3 = Array1::from(vec![1.0, 0.0, 0.0]);
        
        let sim1 = matcher.cosine_similarity(&vec1, &vec2);
        let sim2 = matcher.cosine_similarity(&vec1, &vec3);
        
        assert_eq!(sim1, 0.0); // 垂直向量
        assert_eq!(sim2, 1.0); // 相同向量
    }
}