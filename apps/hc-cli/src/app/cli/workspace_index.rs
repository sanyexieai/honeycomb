//! `hc-cli index` 子命令（工作区 Markdown / 向量索引）。
use anyhow::{Context, Result, bail};
use hc_agent::phrase_match_score;
use hc_bootstrap::workspace_root;
use hc_store::{
    index::{
        DEFAULT_LOCAL_EMBEDDING_DIMS, LocalJsonVectorIndex, RebuildableIndex, VectorIndex,
        VectorQuery, indexed_documents_from_markdown_index, local_hash_embedding,
        vector_documents_from_indexed_documents,
    },
    store::{WorkspaceNamespace, WorkspaceStore},
};
use std::collections::BTreeMap;

pub(super) fn handle_index(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "rebuild" => handle_index_rebuild(rest),
        [cmd, rest @ ..] if cmd == "search" => handle_index_search(rest),
        [] => bail!("usage: hc-cli index <rebuild|search> ..."),
        [other, ..] => bail!("unknown index command: {other}"),
    }
}

fn handle_index_rebuild(args: &[String]) -> Result<()> {
    let mut json = false;
    let mut vector = false;
    let mut dims = DEFAULT_LOCAL_EMBEDDING_DIMS;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--vector" => {
                vector = true;
                index += 1;
            }
            "--dims" => {
                dims = super::parse_usize_arg(
                    args.get(index + 1).context("missing value for --dims")?,
                    "--dims",
                )?;
                index += 2;
            }
            other => bail!("unexpected argument for index rebuild: {other}"),
        }
    }

    let namespace = super::runtime_namespace();
    let store = WorkspaceStore::new(workspace_root());
    let markdown_index = store.rebuild_markdown_index_in_namespace(&namespace)?;
    let mut vector_path = None;
    let mut vector_count = 0usize;
    if vector {
        let indexed_documents = indexed_documents_from_markdown_index(&markdown_index);
        let vector_documents =
            vector_documents_from_indexed_documents(&indexed_documents, |doc| {
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
        vector_count = vector_documents.len();
        let vector_index = LocalJsonVectorIndex::new(workspace_root());
        vector_index.rebuild(&namespace, &vector_documents)?;
        vector_path = Some(vector_index.path_in_namespace(&namespace));
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "namespace": namespace,
                "markdown_documents": markdown_index.documents.len(),
                "markdown_index_path": store.markdown_index_path_in_namespace(&markdown_index.namespace),
                "markdown_search_index_path": store.markdown_search_index_path_in_namespace(&markdown_index.namespace),
                "vector_documents": vector_count,
                "vector_index_path": vector_path,
            }))?
        );
        return Ok(());
    }

    println!(
        "index> markdown documents {}",
        markdown_index.documents.len()
    );
    println!(
        "index> markdown {}",
        store
            .markdown_index_path_in_namespace(&markdown_index.namespace)
            .display()
    );
    println!(
        "index> search {}",
        store
            .markdown_search_index_path_in_namespace(&markdown_index.namespace)
            .display()
    );
    if let Some(path) = vector_path {
        println!("index> vector documents {vector_count}");
        println!("index> vector {}", path.display());
    }
    Ok(())
}

fn handle_index_search(args: &[String]) -> Result<()> {
    let mut text = None;
    let mut json = false;
    let mut vector = false;
    let mut rebuild = false;
    let mut limit = 10usize;
    let mut dims = DEFAULT_LOCAL_EMBEDDING_DIMS;
    let mut filters = BTreeMap::<String, String>::new();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--text" => {
                text = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --text")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--vector" => {
                vector = true;
                index += 1;
            }
            "--rebuild" => {
                rebuild = true;
                index += 1;
            }
            "--limit" => {
                limit = super::parse_usize_arg(
                    args.get(index + 1).context("missing value for --limit")?,
                    "--limit",
                )?;
                index += 2;
            }
            "--dims" => {
                dims = super::parse_usize_arg(
                    args.get(index + 1).context("missing value for --dims")?,
                    "--dims",
                )?;
                index += 2;
            }
            "--filter" => {
                let filter = args.get(index + 1).context("missing value for --filter")?;
                let (key, value) = super::parse_key_value(filter)?;
                filters.insert(key, value);
                index += 2;
            }
            value if text.is_none() => {
                text = Some(value.to_owned());
                index += 1;
            }
            other => bail!("unexpected argument for index search: {other}"),
        }
    }

    let text = text.context("missing search text")?;
    let namespace = super::runtime_namespace();
    let store = WorkspaceStore::new(workspace_root());
    if rebuild {
        let markdown_index = store.rebuild_markdown_index_in_namespace(&namespace)?;
        if vector {
            rebuild_local_vector_index(&namespace, &markdown_index, dims)?;
        }
    }

    if vector {
        let vector_index = LocalJsonVectorIndex::new(workspace_root());
        if !vector_index.path_in_namespace(&namespace).exists() {
            let markdown_index = store.read_or_rebuild_markdown_index_in_namespace(&namespace)?;
            rebuild_local_vector_index(&namespace, &markdown_index, dims)?;
        }
        let hits = vector_index.search(
            &namespace,
            &VectorQuery {
                embedding: local_hash_embedding(&text, dims),
                filters,
                limit: Some(limit.saturating_mul(4).max(limit)),
            },
        )?;
        let markdown_index = store.read_or_rebuild_markdown_index_in_namespace(&namespace)?;
        let hits = rerank_vector_hits_with_markdown_text(hits, &markdown_index, &text, limit);
        if json {
            println!("{}", serde_json::to_string_pretty(&hits)?);
            return Ok(());
        }
        for hit in hits {
            println!("{:.3} | {} | {}", hit.score, hit.id, hit.source_path);
        }
        return Ok(());
    }

    let mut query = hc_store::store::MarkdownQuery::default()
        .with_text(text)
        .with_limit(limit);
    if let Some(doc_type) = filters.get("doc_type") {
        query = query.with_doc_type(doc_type.clone());
    }
    if let Some(status) = filters.get("status") {
        query = query.with_status(status.clone());
    }
    if let Some(tag) = filters.get("tag") {
        query = query.with_tag(tag.clone());
    }
    if let Some(path_prefix) = filters.get("path_prefix") {
        query = query.with_path_prefix(path_prefix.clone());
    }
    let matches = store.query_markdown_index_in_namespace(&namespace, &query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&matches)?);
        return Ok(());
    }
    for entry in matches {
        println!(
            "{} | {} | {}",
            entry.id, entry.doc_type, entry.relative_path
        );
    }
    Ok(())
}

fn rebuild_local_vector_index(
    namespace: &WorkspaceNamespace,
    markdown_index: &hc_store::store::MarkdownIndex,
    dims: usize,
) -> Result<()> {
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
    LocalJsonVectorIndex::new(workspace_root()).rebuild(namespace, &vector_documents)
}

fn rerank_vector_hits_with_markdown_text(
    mut hits: Vec<hc_store::index::IndexHit>,
    markdown_index: &hc_store::store::MarkdownIndex,
    text: &str,
    limit: usize,
) -> Vec<hc_store::index::IndexHit> {
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
