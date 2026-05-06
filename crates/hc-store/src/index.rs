use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::store::{MarkdownIndex, WorkspaceNamespace};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexedDocument {
    pub id: String,
    pub source_path: String,
    pub doc_type: String,
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexQuery {
    pub text: Option<String>,
    #[serde(default)]
    pub filters: BTreeMap<String, String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexHit {
    pub id: String,
    pub source_path: String,
    pub score: f32,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

pub trait RebuildableIndex {
    type Source;

    fn rebuild(&self, namespace: &WorkspaceNamespace, source: &Self::Source) -> Result<()>;
}

pub trait TextIndex: RebuildableIndex<Source = Vec<IndexedDocument>> {
    fn search(&self, namespace: &WorkspaceNamespace, query: &IndexQuery) -> Result<Vec<IndexHit>>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorDocument {
    pub id: String,
    pub source_path: String,
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub text_preview: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorQuery {
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub filters: BTreeMap<String, String>,
    pub limit: Option<usize>,
}

pub trait VectorIndex: RebuildableIndex<Source = Vec<VectorDocument>> {
    fn search(&self, namespace: &WorkspaceNamespace, query: &VectorQuery) -> Result<Vec<IndexHit>>;
}

pub const DEFAULT_LOCAL_EMBEDDING_DIMS: usize = 256;

pub fn local_hash_embedding(text: &str, dims: usize) -> Vec<f32> {
    let dims = dims.max(8);
    let mut embedding = vec![0.0f32; dims];
    for term in local_embedding_terms(text) {
        let hash = stable_hash(term.as_bytes());
        let index = (hash as usize) % dims;
        let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
        embedding[index] += sign;
    }
    normalize_vector(&mut embedding);
    embedding
}

#[derive(Debug, Clone)]
pub struct LocalJsonVectorIndex {
    root: PathBuf,
    file_name: String,
}

impl LocalJsonVectorIndex {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            file_name: "vector-index.json".to_owned(),
        }
    }

    pub fn with_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = file_name.into();
        self
    }

    pub fn path_in_namespace(&self, namespace: &WorkspaceNamespace) -> PathBuf {
        self.root
            .join(namespace.scoped_prefix())
            .join("indexes")
            .join(&self.file_name)
    }

    fn read_documents(&self, namespace: &WorkspaceNamespace) -> Result<Vec<VectorDocument>> {
        let path = self.path_in_namespace(namespace);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read vector index {}", path.display()))?;
        let payload: LocalVectorIndexPayload = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse vector index {}", path.display()))?;
        Ok(payload.documents)
    }
}

impl RebuildableIndex for LocalJsonVectorIndex {
    type Source = Vec<VectorDocument>;

    fn rebuild(&self, namespace: &WorkspaceNamespace, source: &Self::Source) -> Result<()> {
        let path = self.path_in_namespace(namespace);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create index directory {}", parent.display())
            })?;
        }
        let payload = LocalVectorIndexPayload {
            generated_at_ms: current_timestamp_ms(),
            namespace: namespace.clone(),
            documents: source.clone(),
        };
        let content =
            serde_json::to_string_pretty(&payload).context("failed to serialize vector index")?;
        fs::write(&path, content)
            .with_context(|| format!("failed to write vector index {}", path.display()))?;
        Ok(())
    }
}

impl VectorIndex for LocalJsonVectorIndex {
    fn search(&self, namespace: &WorkspaceNamespace, query: &VectorQuery) -> Result<Vec<IndexHit>> {
        if query.embedding.is_empty() {
            bail!("vector query embedding cannot be empty");
        }
        let mut hits = self
            .read_documents(namespace)?
            .into_iter()
            .filter(|document| metadata_matches(&document.metadata, &query.filters))
            .filter_map(|document| {
                cosine_similarity(&query.embedding, &document.embedding).map(|score| IndexHit {
                    id: document.id,
                    source_path: document.source_path,
                    score,
                    metadata: document.metadata,
                })
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.id.cmp(&right.id))
        });
        hits.truncate(query.limit.unwrap_or(10));
        Ok(hits)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalVectorIndexPayload {
    generated_at_ms: u64,
    namespace: WorkspaceNamespace,
    documents: Vec<VectorDocument>,
}

pub fn indexed_documents_from_markdown_index(index: &MarkdownIndex) -> Vec<IndexedDocument> {
    index
        .documents
        .iter()
        .map(|entry| {
            let mut metadata = BTreeMap::new();
            metadata.insert("doc_type".to_owned(), entry.doc_type.clone());
            if let Some(status) = &entry.status {
                metadata.insert("status".to_owned(), status.clone());
            }
            if let Some(room_id) = &entry.room_id {
                metadata.insert("room_id".to_owned(), room_id.clone());
            }
            if let Some(layer) = &entry.layer {
                metadata.insert("layer".to_owned(), layer.clone());
            }
            if let Some(memory_kind) = &entry.memory_kind {
                metadata.insert("memory_kind".to_owned(), memory_kind.clone());
            }
            if let Some(asset_kind) = &entry.asset_kind {
                metadata.insert("asset_kind".to_owned(), asset_kind.clone());
            }
            IndexedDocument {
                id: entry.id.clone(),
                source_path: entry.relative_path.clone(),
                doc_type: entry.doc_type.clone(),
                title: entry.title.clone(),
                text: if entry.semantic_text.trim().is_empty() {
                    entry.body_preview.clone()
                } else {
                    entry.semantic_text.clone()
                },
                tags: entry.tags.clone(),
                metadata,
            }
        })
        .collect()
}

pub fn vector_documents_from_indexed_documents<F>(
    documents: &[IndexedDocument],
    mut embed: F,
) -> Result<Vec<VectorDocument>>
where
    F: FnMut(&IndexedDocument) -> Result<Vec<f32>>,
{
    documents
        .iter()
        .map(|document| {
            let embedding = embed(document)
                .with_context(|| format!("failed to embed indexed document {}", document.id))?;
            Ok(VectorDocument {
                id: document.id.clone(),
                source_path: document.source_path.clone(),
                embedding,
                text_preview: document.text.chars().take(240).collect(),
                metadata: document.metadata.clone(),
            })
        })
        .collect()
}

fn metadata_matches(
    metadata: &BTreeMap<String, String>,
    filters: &BTreeMap<String, String>,
) -> bool {
    filters
        .iter()
        .all(|(key, expected)| metadata.get(key).is_some_and(|actual| actual == expected))
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        dot += left_value * right_value;
        left_norm += left_value * left_value;
        right_norm += right_value * right_value;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return None;
    }
    Some(dot / (left_norm.sqrt() * right_norm.sqrt()))
}

fn local_embedding_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lowered = text.to_lowercase();
    for token in lowered.split(|ch: char| !ch.is_alphanumeric()) {
        if token.chars().count() > 1 {
            terms.push(token.to_owned());
        }
    }
    for run in text
        .split(|ch: char| ch.is_ascii() || ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|part| !part.is_empty())
    {
        let chars = run.chars().collect::<Vec<_>>();
        if chars.len() > 1 {
            terms.push(chars.iter().collect::<String>().to_lowercase());
        }
        for size in [2usize, 3usize] {
            for window in chars.windows(size) {
                terms.push(window.iter().collect::<String>().to_lowercase());
            }
        }
    }
    terms
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return;
    }
    for value in vector {
        *value /= norm;
    }
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[allow(dead_code)]
fn _assert_path_send_sync(_: &Path) {}
