use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub const DEFAULT_TENANT_ID: &str = "local";
pub const DEFAULT_USER_ID: &str = "default";
pub const DEFAULT_WORKSPACE_ROOT: &str = "workspace";

pub fn default_tenant_id() -> String {
    DEFAULT_TENANT_ID.to_owned()
}

pub fn default_user_id() -> String {
    DEFAULT_USER_ID.to_owned()
}

pub fn tenant_id_from_env() -> String {
    env::var("HC_TENANT_ID").unwrap_or_else(|_| default_tenant_id())
}

pub fn user_id_from_env() -> String {
    env::var("HC_USER_ID").unwrap_or_else(|_| default_user_id())
}

pub fn workspace_root() -> PathBuf {
    env::var("HC_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_WORKSPACE_ROOT))
}

pub fn env_file_path() -> Result<PathBuf> {
    Ok(env::current_dir()
        .context("failed to read current directory")?
        .join(".env"))
}

pub fn read_env_map(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut vars = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        vars.insert(key.to_owned(), clean_env_value(value));
    }
    Ok(vars)
}

pub fn load_local_env_file() -> Result<()> {
    let env_path = env_file_path()?;
    if !env_path.exists() {
        return Ok(());
    }

    for (key, value) in read_env_map(&env_path)? {
        if env::var_os(&key).is_none() {
            unsafe { env::set_var(key, value) };
        }
    }
    Ok(())
}

/// 初始化控制台 tracing 输出：`RUST_LOG` 优先，否则读取 `HC_LOG`，默认 `info,hyper=warn,reqwest=warn`。
///
/// 使用 `try_init`，忽略重复初始化（例如测试或多次调用）。
pub fn init_console_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let directive =
            env::var("HC_LOG").unwrap_or_else(|_| "info,hyper=warn,reqwest=warn".to_owned());
        tracing_subscriber::EnvFilter::try_new(&directive)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn clean_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}
