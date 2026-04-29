//! Markdown-first storage primitives.

pub mod store {
    use anyhow::{Context, Result, bail};
    use rusqlite::{Connection, params};
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};
    use serde_yaml::Value;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct WorkspaceNamespace {
        pub tenant_id: String,
        pub user_id: String,
    }

    impl WorkspaceNamespace {
        pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
            Self {
                tenant_id: tenant_id.into(),
                user_id: user_id.into(),
            }
        }

        pub fn local_default() -> Self {
            Self::new("local", "default")
        }

        pub fn scoped_prefix(&self) -> PathBuf {
            PathBuf::from("tenants")
                .join(&self.tenant_id)
                .join("users")
                .join(&self.user_id)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StoredMarkdown<T> {
        pub frontmatter: T,
        pub body: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct MarkdownIndexEntry {
        pub id: String,
        pub doc_type: String,
        pub title: String,
        pub relative_path: String,
        pub tags: Vec<String>,
        pub status: Option<String>,
        pub visibility: Option<String>,
        pub tenant_id: Option<String>,
        pub user_id: Option<String>,
        pub created_at: Option<String>,
        pub updated_at: Option<String>,
        pub relations: Vec<String>,
        pub owners: Vec<String>,
        pub capabilities: Vec<String>,
        pub room_id: Option<String>,
        pub layer: Option<String>,
        pub memory_kind: Option<String>,
        pub asset_kind: Option<String>,
        pub body_preview: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct MarkdownIndex {
        pub generated_at_ms: u64,
        pub namespace: WorkspaceNamespace,
        pub documents: Vec<MarkdownIndexEntry>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct MarkdownQuery {
        pub ids: Vec<String>,
        pub doc_type: Option<String>,
        pub status: Option<String>,
        pub tag: Option<String>,
        pub path_prefix: Option<String>,
        pub text: Option<String>,
        pub limit: Option<usize>,
    }

    impl MarkdownQuery {
        pub fn with_id(mut self, id: impl Into<String>) -> Self {
            self.ids.push(id.into());
            self
        }

        pub fn with_doc_type(mut self, doc_type: impl Into<String>) -> Self {
            self.doc_type = Some(doc_type.into());
            self
        }

        pub fn with_status(mut self, status: impl Into<String>) -> Self {
            self.status = Some(status.into());
            self
        }

        pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
            self.tag = Some(tag.into());
            self
        }

        pub fn with_path_prefix(mut self, path_prefix: impl Into<String>) -> Self {
            self.path_prefix = Some(path_prefix.into());
            self
        }

        pub fn with_text(mut self, text: impl Into<String>) -> Self {
            self.text = Some(text.into());
            self
        }

        pub fn with_limit(mut self, limit: usize) -> Self {
            self.limit = Some(limit);
            self
        }

        fn matches(&self, entry: &MarkdownIndexEntry) -> bool {
            if !self.matches_without_text(entry) {
                return false;
            }

            if let Some(text) = &self.text {
                let needle = text.to_ascii_lowercase();
                let haystacks = [
                    entry.id.as_str(),
                    entry.doc_type.as_str(),
                    entry.title.as_str(),
                    entry.relative_path.as_str(),
                    entry.body_preview.as_str(),
                ];
                let metadata_match = haystacks
                    .iter()
                    .any(|candidate| candidate.to_ascii_lowercase().contains(&needle));
                let tag_match = entry
                    .tags
                    .iter()
                    .any(|candidate| candidate.to_ascii_lowercase().contains(&needle));
                let relation_match = entry
                    .relations
                    .iter()
                    .any(|candidate| candidate.to_ascii_lowercase().contains(&needle));

                if !(metadata_match || tag_match || relation_match) {
                    return false;
                }
            }

            true
        }

        fn matches_without_text(&self, entry: &MarkdownIndexEntry) -> bool {
            if !self.ids.is_empty() && !self.ids.iter().any(|id| id == &entry.id) {
                return false;
            }

            if let Some(doc_type) = &self.doc_type
                && !entry.doc_type.eq_ignore_ascii_case(doc_type)
            {
                return false;
            }

            if let Some(status) = &self.status
                && !entry
                    .status
                    .as_ref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(status))
            {
                return false;
            }

            if let Some(tag) = &self.tag
                && !entry
                    .tags
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(tag))
            {
                return false;
            }

            if let Some(path_prefix) = &self.path_prefix {
                let prefix = normalized_path(path_prefix);
                if !entry.relative_path.starts_with(&prefix) {
                    return false;
                }
            }

            true
        }
    }

    #[derive(Debug, Clone)]
    pub struct WorkspaceStore {
        root: PathBuf,
    }

    impl WorkspaceStore {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self { root: root.into() }
        }

        pub fn root(&self) -> &Path {
            &self.root
        }

        pub fn resolve(&self, relative_path: impl AsRef<Path>) -> PathBuf {
            self.root.join(relative_path.as_ref())
        }

        pub fn resolve_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
            relative_path: impl AsRef<Path>,
        ) -> PathBuf {
            self.resolve(namespace.scoped_prefix().join(relative_path.as_ref()))
        }

        pub fn ensure_dir(&self, relative_dir: impl AsRef<Path>) -> Result<PathBuf> {
            let path = self.resolve(relative_dir);
            fs::create_dir_all(&path)
                .with_context(|| format!("failed to create directory {}", path.display()))?;
            Ok(path)
        }

        pub fn write_markdown<T: Serialize>(
            &self,
            relative_path: impl AsRef<Path>,
            frontmatter: &T,
            body: &str,
        ) -> Result<PathBuf> {
            let path = self.resolve(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create parent directory {}", parent.display())
                })?;
            }

            let yaml = serde_yaml::to_string(frontmatter)
                .context("failed to serialize markdown frontmatter")?;
            let content = format!("---\n{}---\n\n{}", yaml, body);
            fs::write(&path, content)
                .with_context(|| format!("failed to write markdown file {}", path.display()))?;
            Ok(path)
        }

        pub fn write_markdown_in_namespace<T: Serialize>(
            &self,
            namespace: &WorkspaceNamespace,
            relative_path: impl AsRef<Path>,
            frontmatter: &T,
            body: &str,
        ) -> Result<PathBuf> {
            let path = self.resolve_in_namespace(namespace, relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create parent directory {}", parent.display())
                })?;
            }

            let yaml = serde_yaml::to_string(frontmatter)
                .context("failed to serialize markdown frontmatter")?;
            let content = format!("---\n{}---\n\n{}", yaml, body);
            fs::write(&path, content)
                .with_context(|| format!("failed to write markdown file {}", path.display()))?;
            Ok(path)
        }

        pub fn write_text_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
            relative_path: impl AsRef<Path>,
            body: &str,
        ) -> Result<PathBuf> {
            let path = self.resolve_in_namespace(namespace, relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create parent directory {}", parent.display())
                })?;
            }

            fs::write(&path, body)
                .with_context(|| format!("failed to write markdown file {}", path.display()))?;
            Ok(path)
        }

        pub fn read_markdown<T: DeserializeOwned>(
            &self,
            relative_path: impl AsRef<Path>,
        ) -> Result<StoredMarkdown<T>> {
            let path = self.resolve(relative_path);
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read markdown file {}", path.display()))?;
            parse_markdown_document(&content)
                .with_context(|| format!("failed to parse markdown file {}", path.display()))
        }

        pub fn read_markdown_in_namespace<T: DeserializeOwned>(
            &self,
            namespace: &WorkspaceNamespace,
            relative_path: impl AsRef<Path>,
        ) -> Result<StoredMarkdown<T>> {
            let path = self.resolve_in_namespace(namespace, relative_path);
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read markdown file {}", path.display()))?;
            parse_markdown_document(&content)
                .with_context(|| format!("failed to parse markdown file {}", path.display()))
        }

        pub fn markdown_index_path_in_namespace(&self, namespace: &WorkspaceNamespace) -> PathBuf {
            self.resolve_in_namespace(
                namespace,
                PathBuf::from("indexes").join("markdown-index.json"),
            )
        }

        pub fn markdown_search_index_path_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
        ) -> PathBuf {
            self.resolve_in_namespace(
                namespace,
                PathBuf::from("indexes").join("markdown-search.sqlite"),
            )
        }

        pub fn rebuild_markdown_index_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
        ) -> Result<MarkdownIndex> {
            let namespace_root = self.resolve(namespace.scoped_prefix());
            fs::create_dir_all(&namespace_root).with_context(|| {
                format!(
                    "failed to create namespace root {}",
                    namespace_root.display()
                )
            })?;

            let mut files = Vec::new();
            collect_markdown_files(
                &namespace_root,
                &namespace_root,
                Path::new("indexes"),
                &mut files,
            )?;
            files.sort();

            let mut documents = Vec::new();
            for relative_path in files {
                let path = namespace_root.join(&relative_path);
                let content = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read markdown file {}", path.display()))?;
                if content.starts_with("---\n") {
                    let stored = parse_markdown_document::<Value>(&content).with_context(|| {
                        format!("failed to parse markdown file {}", path.display())
                    })?;
                    documents.push(build_index_entry(
                        &relative_path,
                        stored.frontmatter,
                        &stored.body,
                    )?);
                } else {
                    documents.push(
                        build_plain_index_entry(&path, &relative_path, &content).with_context(
                            || format!("failed to index plain markdown file {}", path.display()),
                        )?,
                    );
                }
            }

            let index = MarkdownIndex {
                generated_at_ms: current_timestamp_ms(),
                namespace: namespace.clone(),
                documents,
            };

            let index_path = self.markdown_index_path_in_namespace(namespace);
            if let Some(parent) = index_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create index directory {}", parent.display())
                })?;
            }
            let payload = serde_json::to_string_pretty(&index)
                .context("failed to serialize markdown index")?;
            fs::write(&index_path, payload)
                .with_context(|| format!("failed to write index file {}", index_path.display()))?;

            self.rebuild_markdown_search_index_in_namespace(&index)?;

            Ok(index)
        }

        pub fn rebuild_markdown_search_index_in_namespace(
            &self,
            index: &MarkdownIndex,
        ) -> Result<()> {
            let path = self.markdown_search_index_path_in_namespace(&index.namespace);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create search index directory {}",
                        parent.display()
                    )
                })?;
            }

            let mut conn = Connection::open(&path)
                .with_context(|| format!("failed to open search index {}", path.display()))?;
            conn.execute_batch(
                r#"
                PRAGMA journal_mode = WAL;
                DROP TABLE IF EXISTS markdown_documents_fts;
                DROP TABLE IF EXISTS markdown_documents;

                CREATE TABLE markdown_documents (
                    relative_path TEXT PRIMARY KEY,
                    id TEXT NOT NULL,
                    doc_type TEXT NOT NULL,
                    title TEXT NOT NULL,
                    tags TEXT NOT NULL,
                    status TEXT,
                    relations TEXT NOT NULL,
                    owners TEXT NOT NULL,
                    capabilities TEXT NOT NULL,
                    room_id TEXT,
                    layer TEXT,
                    memory_kind TEXT,
                    asset_kind TEXT,
                    body_preview TEXT NOT NULL
                );

                CREATE VIRTUAL TABLE markdown_documents_fts USING fts5(
                    relative_path UNINDEXED,
                    id,
                    doc_type,
                    title,
                    tags,
                    status,
                    relations,
                    owners,
                    capabilities,
                    room_id,
                    layer,
                    memory_kind,
                    asset_kind,
                    body_preview
                );
                "#,
            )
            .with_context(|| format!("failed to initialize search index {}", path.display()))?;

            let tx = conn
                .transaction()
                .context("failed to start search index transaction")?;
            {
                let mut insert_doc = tx.prepare(
                    r#"
                    INSERT INTO markdown_documents (
                        relative_path, id, doc_type, title, tags, status,
                        relations, owners, capabilities, room_id, layer,
                        memory_kind, asset_kind, body_preview
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                    "#,
                )?;
                let mut insert_fts = tx.prepare(
                    r#"
                    INSERT INTO markdown_documents_fts (
                        relative_path, id, doc_type, title, tags, status,
                        relations, owners, capabilities, room_id, layer,
                        memory_kind, asset_kind, body_preview
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                    "#,
                )?;

                for entry in &index.documents {
                    let tags = entry.tags.join(" ");
                    let relations = entry.relations.join(" ");
                    let owners = entry.owners.join(" ");
                    let capabilities = entry.capabilities.join(" ");
                    let status = entry.status.clone().unwrap_or_default();
                    let values = params![
                        entry.relative_path,
                        entry.id,
                        entry.doc_type,
                        entry.title,
                        tags,
                        status,
                        relations,
                        owners,
                        capabilities,
                        entry.room_id.as_deref(),
                        entry.layer.as_deref(),
                        entry.memory_kind.as_deref(),
                        entry.asset_kind.as_deref(),
                        entry.body_preview
                    ];
                    insert_doc.execute(values)?;
                    insert_fts.execute(params![
                        entry.relative_path,
                        entry.id,
                        entry.doc_type,
                        entry.title,
                        entry.tags.join(" "),
                        entry.status.clone().unwrap_or_default(),
                        entry.relations.join(" "),
                        entry.owners.join(" "),
                        entry.capabilities.join(" "),
                        entry.room_id.clone().unwrap_or_default(),
                        entry.layer.clone().unwrap_or_default(),
                        entry.memory_kind.clone().unwrap_or_default(),
                        entry.asset_kind.clone().unwrap_or_default(),
                        entry.body_preview
                    ])?;
                }
            }
            tx.commit()
                .context("failed to commit search index transaction")?;
            Ok(())
        }

        pub fn read_markdown_index_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
        ) -> Result<MarkdownIndex> {
            let path = self.markdown_index_path_in_namespace(namespace);
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read index file {}", path.display()))?;
            serde_json::from_str(&content)
                .with_context(|| format!("failed to parse index file {}", path.display()))
        }

        pub fn read_or_rebuild_markdown_index_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
        ) -> Result<MarkdownIndex> {
            let index = match self.read_markdown_index_in_namespace(namespace) {
                Ok(index) => index,
                Err(_) => self.rebuild_markdown_index_in_namespace(namespace)?,
            };
            let search_index_path = self.markdown_search_index_path_in_namespace(namespace);
            if !search_index_path.exists() {
                self.rebuild_markdown_search_index_in_namespace(&index)?;
            }
            Ok(index)
        }

        pub fn query_markdown_index_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
            query: &MarkdownQuery,
        ) -> Result<Vec<MarkdownIndexEntry>> {
            let index = self.read_or_rebuild_markdown_index_in_namespace(namespace)?;
            query_markdown_index_with_search_index(self, namespace, &index, query)
        }
    }

    pub fn query_markdown_index_with_search_index(
        store: &WorkspaceStore,
        namespace: &WorkspaceNamespace,
        index: &MarkdownIndex,
        query: &MarkdownQuery,
    ) -> Result<Vec<MarkdownIndexEntry>> {
        if query
            .text
            .as_ref()
            .is_none_or(|text| text.trim().is_empty())
        {
            return Ok(query_markdown_index(index, query));
        }

        let search_index_path = store.markdown_search_index_path_in_namespace(namespace);
        if !search_index_path.exists() {
            store.rebuild_markdown_search_index_in_namespace(index)?;
        }

        let fts_paths = search_markdown_paths(&search_index_path, query, index.documents.len())
            .unwrap_or_default();
        if fts_paths.is_empty() {
            return Ok(query_markdown_index(index, query));
        }

        let by_path = index
            .documents
            .iter()
            .map(|entry| (entry.relative_path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        let mut matches = Vec::new();

        for path in fts_paths {
            let Some(entry) = by_path.get(path.as_str()) else {
                continue;
            };
            seen.insert(path);
            if query.matches_without_text(entry) {
                matches.push((*entry).clone());
                if query.limit.is_some_and(|limit| matches.len() >= limit) {
                    return Ok(matches);
                }
            }
        }

        for entry in &index.documents {
            if seen.contains(&entry.relative_path) {
                continue;
            }
            if query.matches(entry) {
                matches.push(entry.clone());
                if query.limit.is_some_and(|limit| matches.len() >= limit) {
                    break;
                }
            }
        }

        Ok(matches)
    }

    pub fn query_markdown_index(
        index: &MarkdownIndex,
        query: &MarkdownQuery,
    ) -> Vec<MarkdownIndexEntry> {
        let mut matches = Vec::new();
        for entry in &index.documents {
            if query.matches(entry) {
                matches.push(entry.clone());
                if query.limit.is_some_and(|limit| matches.len() >= limit) {
                    break;
                }
            }
        }
        matches
    }

    fn search_markdown_paths(
        search_index_path: &Path,
        query: &MarkdownQuery,
        document_count: usize,
    ) -> Result<Vec<String>> {
        let Some(text) = query.text.as_ref() else {
            return Ok(Vec::new());
        };
        let Some(match_query) = fts5_match_query(text) else {
            return Ok(Vec::new());
        };

        let conn = Connection::open(search_index_path).with_context(|| {
            format!(
                "failed to open markdown search index {}",
                search_index_path.display()
            )
        })?;
        let limit = query
            .limit
            .map(|limit| limit.saturating_mul(8).max(32))
            .unwrap_or(document_count)
            .min(document_count.max(1));
        let mut stmt = conn.prepare(
            r#"
            SELECT relative_path
            FROM markdown_documents_fts
            WHERE markdown_documents_fts MATCH ?1
            ORDER BY rank
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(params![match_query, limit as i64], |row| row.get(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    fn fts5_match_query(text: &str) -> Option<String> {
        let mut terms = Vec::<String>::new();
        let mut current = String::new();
        for character in text.chars() {
            if character.is_alphanumeric() || character == '_' || character == '-' {
                current.push(character.to_ascii_lowercase());
                continue;
            }

            push_fts5_term(&mut terms, &mut current);
            if is_cjk(character) {
                terms.push(character.to_string());
            }
        }
        push_fts5_term(&mut terms, &mut current);

        let mut unique = BTreeSet::new();
        let mut quoted = Vec::new();
        for term in terms {
            if is_fts5_stopword(&term) {
                continue;
            }
            if unique.insert(term.clone()) {
                quoted.push(format!("\"{}\"", term.replace('"', "\"\"")));
            }
            if quoted.len() >= 12 {
                break;
            }
        }

        if quoted.is_empty() {
            None
        } else {
            Some(quoted.join(" OR "))
        }
    }

    fn push_fts5_term(terms: &mut Vec<String>, current: &mut String) {
        if current.chars().count() >= 2 {
            terms.push(std::mem::take(current));
        } else {
            current.clear();
        }
    }

    fn is_fts5_stopword(term: &str) -> bool {
        matches!(
            term,
            "a" | "an"
                | "and"
                | "are"
                | "as"
                | "be"
                | "but"
                | "by"
                | "can"
                | "do"
                | "does"
                | "for"
                | "from"
                | "how"
                | "i"
                | "in"
                | "is"
                | "it"
                | "of"
                | "on"
                | "or"
                | "should"
                | "the"
                | "this"
                | "to"
                | "was"
                | "what"
                | "when"
                | "where"
                | "with"
                | "you"
                | "your"
        )
    }

    fn is_cjk(character: char) -> bool {
        matches!(
            character as u32,
            0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0x20000..=0x2A6DF
                | 0x3000..=0x303F
                | 0x3040..=0x30FF
                | 0xAC00..=0xD7AF
        )
    }

    pub fn parse_markdown_document<T: DeserializeOwned>(
        content: &str,
    ) -> Result<StoredMarkdown<T>> {
        let rest = content
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow::anyhow!("markdown document is missing opening frontmatter"))?;
        let Some((frontmatter, remainder)) = rest.split_once("\n---\n") else {
            bail!("markdown document is missing closing frontmatter");
        };
        let frontmatter = serde_yaml::from_str(frontmatter)
            .context("failed to deserialize markdown frontmatter")?;
        let body = remainder.strip_prefix('\n').unwrap_or(remainder).to_owned();

        Ok(StoredMarkdown { frontmatter, body })
    }

    fn collect_markdown_files(
        root: &Path,
        current_dir: &Path,
        ignored_dir: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for entry in fs::read_dir(current_dir)
            .with_context(|| format!("failed to read directory {}", current_dir.display()))?
        {
            let entry = entry.with_context(|| {
                format!(
                    "failed to read directory entry in {}",
                    current_dir.display()
                )
            })?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("failed to strip prefix from {}", path.display()))?;

            if path.is_dir() {
                if relative == ignored_dir {
                    continue;
                }
                collect_markdown_files(root, &path, ignored_dir, files)?;
                continue;
            }

            if path.extension().is_some_and(|extension| extension == "md") {
                files.push(relative.to_path_buf());
            }
        }

        Ok(())
    }

    fn build_index_entry(
        relative_path: &Path,
        frontmatter: Value,
        body: &str,
    ) -> Result<MarkdownIndexEntry> {
        let mapping = frontmatter
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("markdown frontmatter must be a YAML mapping"))?;
        let id = required_string_field(mapping, "id")?;
        let doc_type = required_string_field(mapping, "type")?;
        let title = optional_string_field(mapping, "title")
            .or_else(|| title_from_body(body))
            .or_else(|| {
                relative_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(|value| value.replace(['-', '_'], " "))
            })
            .unwrap_or_else(|| id.clone());
        let room_id = optional_string_field(mapping, "room_id").or_else(|| {
            if doc_type == "memory_room" {
                Some(id.clone())
            } else {
                None
            }
        });

        Ok(MarkdownIndexEntry {
            id,
            doc_type,
            title,
            relative_path: normalized_path(relative_path),
            tags: string_list_field(mapping, "tags"),
            status: optional_string_field(mapping, "status"),
            visibility: optional_string_field(mapping, "visibility"),
            tenant_id: optional_string_field(mapping, "tenant_id"),
            user_id: optional_string_field(mapping, "user_id"),
            created_at: optional_string_field(mapping, "created_at"),
            updated_at: optional_string_field(mapping, "updated_at"),
            relations: relation_targets_field(mapping, "relations"),
            owners: string_list_field(mapping, "owners"),
            capabilities: string_list_field(mapping, "capabilities"),
            room_id,
            layer: optional_string_field(mapping, "layer"),
            memory_kind: optional_string_field(mapping, "memory_kind"),
            asset_kind: optional_string_field(mapping, "asset_kind"),
            body_preview: preview_text(body, 160),
        })
    }

    fn build_plain_index_entry(
        absolute_path: &Path,
        relative_path: &Path,
        body: &str,
    ) -> Result<MarkdownIndexEntry> {
        let normalized = normalized_path(relative_path);
        let sidecar = read_plain_markdown_sidecar(absolute_path)?;
        let id = required_json_string_field(&sidecar, "id")?;
        let doc_type = required_json_string_field(&sidecar, "type")?;
        let title = optional_json_string_field(&sidecar, "title")
            .or_else(|| title_from_body(body))
            .or_else(|| {
                relative_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(|value| value.replace(['-', '_', '.'], " "))
            })
            .unwrap_or_else(|| id.clone());

        Ok(MarkdownIndexEntry {
            id,
            doc_type,
            title,
            relative_path: normalized,
            tags: json_string_list_field(&sidecar, "tags").unwrap_or_default(),
            status: optional_json_string_field(&sidecar, "status"),
            visibility: optional_json_string_field(&sidecar, "visibility"),
            tenant_id: optional_json_string_field(&sidecar, "tenant_id"),
            user_id: optional_json_string_field(&sidecar, "user_id"),
            created_at: None,
            updated_at: None,
            relations: Vec::new(),
            owners: json_owner_list_field(&sidecar, "owners").unwrap_or_default(),
            capabilities: Vec::new(),
            room_id: optional_json_string_field(&sidecar, "room_id"),
            layer: optional_json_string_field(&sidecar, "layer"),
            memory_kind: optional_json_string_field(&sidecar, "memory_kind"),
            asset_kind: optional_json_string_field(&sidecar, "asset_kind"),
            body_preview: preview_text(body, 160),
        })
    }

    fn read_plain_markdown_sidecar(absolute_path: &Path) -> Result<serde_json::Value> {
        let file_name = absolute_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("plain markdown path is missing a valid file name"))?;
        let sidecar_name = format!("{}.meta.json", file_name.trim_end_matches(".md"));
        let sidecar_path = absolute_path.with_file_name(sidecar_name);
        let content = fs::read_to_string(&sidecar_path).with_context(|| {
            format!("failed to read sidecar metadata {}", sidecar_path.display())
        })?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "failed to parse sidecar metadata {}",
                sidecar_path.display()
            )
        })
    }

    fn optional_json_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
        value.get(field).and_then(|value| match value {
            serde_json::Value::String(value) => Some(value.clone()),
            serde_json::Value::Number(value) => Some(value.to_string()),
            serde_json::Value::Bool(value) => Some(value.to_string()),
            _ => None,
        })
    }

    fn required_json_string_field(value: &serde_json::Value, field: &str) -> Result<String> {
        optional_json_string_field(value, field).ok_or_else(|| {
            anyhow::anyhow!("plain markdown sidecar is missing required field `{field}`")
        })
    }

    fn json_string_list_field(value: &serde_json::Value, field: &str) -> Option<Vec<String>> {
        Some(
            value
                .get(field)?
                .as_array()?
                .iter()
                .filter_map(|entry| match entry {
                    serde_json::Value::String(value) => Some(value.clone()),
                    _ => None,
                })
                .collect(),
        )
    }

    fn json_owner_list_field(value: &serde_json::Value, field: &str) -> Option<Vec<String>> {
        Some(
            value
                .get(field)?
                .as_array()?
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("id")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .collect(),
        )
    }

    fn required_string_field(mapping: &serde_yaml::Mapping, field: &str) -> Result<String> {
        optional_string_field(mapping, field).ok_or_else(|| {
            anyhow::anyhow!("markdown frontmatter is missing required field `{field}`")
        })
    }

    fn optional_string_field(mapping: &serde_yaml::Mapping, field: &str) -> Option<String> {
        let key = Value::String(field.to_owned());
        mapping.get(&key).and_then(value_to_string)
    }

    fn string_list_field(mapping: &serde_yaml::Mapping, field: &str) -> Vec<String> {
        let key = Value::String(field.to_owned());
        mapping
            .get(&key)
            .and_then(Value::as_sequence)
            .map(|values| values.iter().filter_map(value_to_string).collect())
            .unwrap_or_default()
    }

    fn relation_targets_field(mapping: &serde_yaml::Mapping, field: &str) -> Vec<String> {
        let key = Value::String(field.to_owned());
        mapping
            .get(&key)
            .and_then(Value::as_sequence)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| {
                        let relation = value.as_mapping()?;
                        let target = relation.get(Value::String("target".to_owned()))?;
                        value_to_string(target)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn value_to_string(value: &Value) -> Option<String> {
        match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        }
    }

    fn title_from_body(body: &str) -> Option<String> {
        body.lines().find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix('#')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
    }

    fn preview_text(body: &str, limit: usize) -> String {
        let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.chars().count() <= limit {
            return normalized;
        }

        let shortened = normalized.chars().take(limit).collect::<String>();
        format!("{shortened}...")
    }

    fn normalized_path(path: impl AsRef<Path>) -> String {
        path.as_ref().to_string_lossy().replace('\\', "/")
    }

    fn current_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_millis() as u64
    }
}
