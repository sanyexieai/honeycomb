use std::collections::BTreeMap;

use anyhow::Result;
use hc_agent::phrase_match_score;
use hc_protocol::ApiNamespace;
use hc_store::{
    index::{
        DEFAULT_LOCAL_EMBEDDING_DIMS, IndexHit, LocalJsonVectorIndex, RebuildableIndex,
        VectorIndex, VectorQuery, indexed_documents_from_markdown_index, local_hash_embedding,
        vector_documents_from_indexed_documents,
    },
    store::{MarkdownIndexEntry, MarkdownQuery, WorkspaceNamespace, WorkspaceStore},
};
use serde::{Deserialize, Serialize};

use crate::ServiceConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexRebuildRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub vector: bool,
    #[serde(default)]
    pub dims: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexRebuildResponse {
    pub namespace: ApiNamespace,
    pub markdown_documents: usize,
    pub markdown_index_path: String,
    pub markdown_search_index_path: String,
    pub vector_documents: usize,
    pub vector_index_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexSearchRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub vector: bool,
    #[serde(default)]
    pub rebuild: bool,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub dims: Option<usize>,
    #[serde(default)]
    pub filters: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexSearchResponse {
    Markdown { hits: Vec<MarkdownIndexEntry> },
    Vector { hits: Vec<IndexHit> },
}

pub fn rebuild_index(
    config: &ServiceConfig,
    request: IndexRebuildRequest,
) -> Result<IndexRebuildResponse> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let workspace_namespace = workspace_namespace(&namespace);
    let store = WorkspaceStore::new(config.workspace_root.clone());
    let markdown_index = store.rebuild_markdown_index_in_namespace(&workspace_namespace)?;
    let mut vector_path = None;
    let mut vector_count = 0usize;
    if request.vector {
        let dims = request.dims.unwrap_or(DEFAULT_LOCAL_EMBEDDING_DIMS);
        vector_count = rebuild_local_vector_index(
            &config.workspace_root,
            &workspace_namespace,
            &markdown_index,
            dims,
        )?;
        vector_path = Some(
            LocalJsonVectorIndex::new(config.workspace_root.clone())
                .path_in_namespace(&workspace_namespace)
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
    Ok(IndexRebuildResponse {
        namespace,
        markdown_documents: markdown_index.documents.len(),
        markdown_index_path: store
            .markdown_index_path_in_namespace(&markdown_index.namespace)
            .to_string_lossy()
            .replace('\\', "/"),
        markdown_search_index_path: store
            .markdown_search_index_path_in_namespace(&markdown_index.namespace)
            .to_string_lossy()
            .replace('\\', "/"),
        vector_documents: vector_count,
        vector_index_path: vector_path,
    })
}

pub fn search_index(
    config: &ServiceConfig,
    request: IndexSearchRequest,
) -> Result<IndexSearchResponse> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let workspace_namespace = workspace_namespace(&namespace);
    let store = WorkspaceStore::new(config.workspace_root.clone());
    let limit = request.limit.unwrap_or(10).clamp(1, 100);
    let dims = request.dims.unwrap_or(DEFAULT_LOCAL_EMBEDDING_DIMS);
    if request.rebuild {
        let markdown_index = store.rebuild_markdown_index_in_namespace(&workspace_namespace)?;
        if request.vector {
            rebuild_local_vector_index(
                &config.workspace_root,
                &workspace_namespace,
                &markdown_index,
                dims,
            )?;
        }
    }
    if request.vector {
        let vector_index = LocalJsonVectorIndex::new(config.workspace_root.clone());
        if !vector_index
            .path_in_namespace(&workspace_namespace)
            .exists()
        {
            let markdown_index =
                store.read_or_rebuild_markdown_index_in_namespace(&workspace_namespace)?;
            rebuild_local_vector_index(
                &config.workspace_root,
                &workspace_namespace,
                &markdown_index,
                dims,
            )?;
        }
        let hits = vector_index.search(
            &workspace_namespace,
            &VectorQuery {
                embedding: local_hash_embedding(&request.text, dims),
                filters: request.filters,
                limit: Some(limit.saturating_mul(4).max(limit)),
            },
        )?;
        let markdown_index =
            store.read_or_rebuild_markdown_index_in_namespace(&workspace_namespace)?;
        return Ok(IndexSearchResponse::Vector {
            hits: rerank_vector_hits_with_markdown_text(
                hits,
                &markdown_index,
                &request.text,
                limit,
            ),
        });
    }

    let mut query = MarkdownQuery::default()
        .with_text(request.text)
        .with_limit(limit);
    if let Some(doc_type) = request.filters.get("doc_type") {
        query = query.with_doc_type(doc_type.clone());
    }
    if let Some(status) = request.filters.get("status") {
        query = query.with_status(status.clone());
    }
    if let Some(tag) = request.filters.get("tag") {
        query = query.with_tag(tag.clone());
    }
    if let Some(path_prefix) = request.filters.get("path_prefix") {
        query = query.with_path_prefix(path_prefix.clone());
    }
    Ok(IndexSearchResponse::Markdown {
        hits: store.query_markdown_index_in_namespace(&workspace_namespace, &query)?,
    })
}

fn rebuild_local_vector_index(
    root: impl Into<std::path::PathBuf>,
    namespace: &WorkspaceNamespace,
    markdown_index: &hc_store::store::MarkdownIndex,
    dims: usize,
) -> Result<usize> {
    let indexed_documents = indexed_documents_from_markdown_index(markdown_index);
    let vector_documents = vector_documents_from_indexed_documents(&indexed_documents, |doc| {
        Ok(local_hash_embedding(
            &format!(
                "{} {} {} {}",
                doc.title,
                doc.doc_type,
                doc.tags.join(" "),
                doc.text
            ),
            dims,
        ))
    })?;
    let count = vector_documents.len();
    LocalJsonVectorIndex::new(root).rebuild(namespace, &vector_documents)?;
    Ok(count)
}

fn rerank_vector_hits_with_markdown_text(
    mut hits: Vec<IndexHit>,
    markdown_index: &hc_store::store::MarkdownIndex,
    text: &str,
    limit: usize,
) -> Vec<IndexHit> {
    let by_path = markdown_index
        .documents
        .iter()
        .map(|entry| (entry.relative_path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    for hit in &mut hits {
        let Some(entry) = by_path.get(hit.source_path.as_str()) else {
            continue;
        };
        let lexical_score = phrase_match_score(text, &entry.semantic_text) as f32 / 100.0;
        hit.score += lexical_score;
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    hits.truncate(limit);
    hits
}

fn normalized_namespace(
    mut namespace: ApiNamespace,
    tenant_id: Option<String>,
    user_id: Option<String>,
) -> ApiNamespace {
    if let Some(tenant_id) = tenant_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = user_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        namespace.user_id = user_id;
    }
    if namespace.tenant_id.trim().is_empty() {
        namespace.tenant_id = hc_context::runtime::DEFAULT_TENANT_ID.to_owned();
    }
    if namespace.user_id.trim().is_empty() {
        namespace.user_id = hc_context::runtime::DEFAULT_USER_ID.to_owned();
    }
    namespace
}

fn workspace_namespace(namespace: &ApiNamespace) -> WorkspaceNamespace {
    WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}
