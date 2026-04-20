//! Markdown-first storage primitives.

pub mod store {
    use anyhow::{Context, Result, bail};
    use serde::{Deserialize, Serialize};
    use serde::de::DeserializeOwned;
    use serde_yaml::Value;
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
                && !entry.tags.iter().any(|candidate| candidate.eq_ignore_ascii_case(tag))
            {
                return false;
            }

            if let Some(path_prefix) = &self.path_prefix {
                let prefix = normalized_path(path_prefix);
                if !entry.relative_path.starts_with(&prefix) {
                    return false;
                }
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
            self.resolve_in_namespace(namespace, PathBuf::from("indexes").join("markdown-index.json"))
        }

        pub fn rebuild_markdown_index_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
        ) -> Result<MarkdownIndex> {
            let namespace_root = self.resolve(namespace.scoped_prefix());
            fs::create_dir_all(&namespace_root).with_context(|| {
                format!("failed to create namespace root {}", namespace_root.display())
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
                let stored = parse_markdown_document::<Value>(&content).with_context(|| {
                    format!("failed to parse markdown file {}", path.display())
                })?;
                documents.push(build_index_entry(&relative_path, stored.frontmatter, &stored.body)?);
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

            Ok(index)
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

        pub fn query_markdown_index_in_namespace(
            &self,
            namespace: &WorkspaceNamespace,
            query: &MarkdownQuery,
        ) -> Result<Vec<MarkdownIndexEntry>> {
            let index = self.read_markdown_index_in_namespace(namespace)?;
            Ok(apply_query(index.documents, query))
        }
    }

    pub fn parse_markdown_document<T: DeserializeOwned>(content: &str) -> Result<StoredMarkdown<T>> {
        let rest = content
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow::anyhow!("markdown document is missing opening frontmatter"))?;
        let Some((frontmatter, remainder)) = rest.split_once("\n---\n") else {
            bail!("markdown document is missing closing frontmatter");
        };
        let frontmatter = serde_yaml::from_str(frontmatter)
            .context("failed to deserialize markdown frontmatter")?;
        let body = remainder
            .strip_prefix('\n')
            .unwrap_or(remainder)
            .to_owned();

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
                format!("failed to read directory entry in {}", current_dir.display())
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

    fn build_index_entry(relative_path: &Path, frontmatter: Value, body: &str) -> Result<MarkdownIndexEntry> {
        let mapping = frontmatter
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("markdown frontmatter must be a YAML mapping"))?;
        let id = required_string_field(mapping, "id")?;
        let doc_type = required_string_field(mapping, "type")?;
        let title = required_string_field(mapping, "title")?;

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
            body_preview: preview_text(body, 160),
        })
    }

    fn apply_query(
        mut documents: Vec<MarkdownIndexEntry>,
        query: &MarkdownQuery,
    ) -> Vec<MarkdownIndexEntry> {
        documents.retain(|entry| query.matches(entry));
        if let Some(limit) = query.limit {
            documents.truncate(limit);
        }
        documents
    }

    fn required_string_field(mapping: &serde_yaml::Mapping, field: &str) -> Result<String> {
        optional_string_field(mapping, field)
            .ok_or_else(|| anyhow::anyhow!("markdown frontmatter is missing required field `{field}`"))
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
