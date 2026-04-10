//! Markdown-first storage primitives.

pub mod store {
    use anyhow::{Context, Result, bail};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[derive(Debug, Clone, PartialEq, Eq)]
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
}
