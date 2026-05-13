//! 可选：在本会话目录下放置 `hc-agent` 可执行入口（复制 / 软链接 / Windows 快捷方式）。
//!
//! 配置环境变量（均在初始化会话目录时读取）：
//! - **`HC_AGENT_SESSION_BIN_MODE`**：`off`（默认）、`copy`、`symlink`、`shortcut`（非 Windows 上视为 `symlink`）。
//! - **`HC_AGENT_SESSION_BIN_SOURCE`**：源可执行文件路径；不设置则尝试使用当前进程 [`std::env::current_exe`]（仅当文件名含 `hc-agent` 时）。
//! - **`HC_AGENT_SESSION_BIN_REFRESH`**：设为 `1` / `true` / `on` 时强制重建目标（否则若目标已存在则跳过）。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

/// 会话根目录下的目标文件名（Windows 为 `hc-agent.exe`）。
pub fn session_hc_agent_dest_file_name() -> &'static str {
    #[cfg(windows)]
    {
        "hc-agent.exe"
    }
    #[cfg(not(windows))]
    {
        "hc-agent"
    }
}

const LNK_NAME: &str = "hc-agent.lnk";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HcAgentSessionBinMode {
    #[default]
    Off,
    Copy,
    Symlink,
    /// Windows：创建 `.lnk`；其他平台与 [`HcAgentSessionBinMode::Symlink`] 相同。
    Shortcut,
}

impl HcAgentSessionBinMode {
    pub fn from_env() -> Self {
        match std::env::var("HC_AGENT_SESSION_BIN_MODE")
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default()
            .as_str()
        {
            "copy" => Self::Copy,
            "symlink" | "link" | "softlink" => Self::Symlink,
            "shortcut" | "lnk" => Self::Shortcut,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionHcAgentBinOptions {
    pub mode: HcAgentSessionBinMode,
    pub source: Option<PathBuf>,
    pub refresh: bool,
}

impl SessionHcAgentBinOptions {
    pub fn from_env() -> Self {
        Self {
            mode: HcAgentSessionBinMode::from_env(),
            source: std::env::var("HC_AGENT_SESSION_BIN_SOURCE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            refresh: env_truthy("HC_AGENT_SESSION_BIN_REFRESH"),
        }
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn resolve_source_exe(options: &SessionHcAgentBinOptions) -> Option<PathBuf> {
    if let Some(path) = &options.source {
        return path.is_file().then_some(path.clone());
    }
    let current = std::env::current_exe().ok()?;
    let stem = current
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())?;
    if stem.contains("hc-agent") {
        return Some(current);
    }
    None
}

fn remove_if_exists(path: &Path) {
    let _ = fs::remove_file(path);
}

fn should_skip_output(path: &Path, refresh: bool) -> bool {
    !refresh && path.exists()
}

/// 在 `session_root`（本会话目录）下安装可配置的 `hc-agent` 入口；失败时记录告警并返回 `Ok(())`，不阻断会话创建。
pub fn maybe_install_session_hc_agent_bin(
    session_root: &Path,
    workspace_root: &Path,
    options: &SessionHcAgentBinOptions,
) -> Result<()> {
    if options.mode == HcAgentSessionBinMode::Off {
        return Ok(());
    }

    let Some(source) = resolve_source_exe(options) else {
        warn!(
            "HC_AGENT_SESSION_BIN_MODE is {:?} but no usable source (set HC_AGENT_SESSION_BIN_SOURCE or run from hc-agent)",
            options.mode
        );
        return Ok(());
    };

    let install_result = match options.mode {
        HcAgentSessionBinMode::Off => Ok(()),
        HcAgentSessionBinMode::Copy => {
            install_copy(session_root, &source, options.refresh)
        }
        HcAgentSessionBinMode::Symlink => {
            install_symlink(session_root, &source, options.refresh)
        }
        HcAgentSessionBinMode::Shortcut => {
            #[cfg(windows)]
            {
                install_shortcut_windows(session_root, workspace_root, &source, options.refresh)
            }
            #[cfg(not(windows))]
            {
                install_symlink(session_root, &source, options.refresh)
            }
        }
    };

    if let Err(error) = install_result {
        warn!(
            ?error,
            "failed to install session hc-agent bin (mode {:?}); see HC_AGENT_SESSION_BIN_* env",
            options.mode
        );
    }
    Ok(())
}

fn dest_copy_or_link_path(session_root: &Path) -> PathBuf {
    session_root.join(session_hc_agent_dest_file_name())
}

fn install_copy(session_root: &Path, source: &Path, refresh: bool) -> Result<()> {
    let dest = dest_copy_or_link_path(session_root);
    if should_skip_output(&dest, refresh) {
        return Ok(());
    }
    if refresh {
        remove_if_exists(&dest);
    }
    fs::copy(source, &dest).with_context(|| {
        format!(
            "copy {} -> {}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(())
}

fn install_symlink(session_root: &Path, source: &Path, refresh: bool) -> Result<()> {
    let dest = dest_copy_or_link_path(session_root);
    if should_skip_output(&dest, refresh) {
        return Ok(());
    }
    if refresh {
        remove_if_exists(&dest);
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, &dest).with_context(|| {
            format!(
                "symlink {} -> {}",
                source.display(),
                dest.display()
            )
        })?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(source, &dest).with_context(|| {
            format!(
                "symlink_file {} -> {} (Windows may require Developer Mode or admin for symlinks; try HC_AGENT_SESSION_BIN_MODE=copy)",
                source.display(),
                dest.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(windows)]
fn install_shortcut_windows(
    session_root: &Path,
    workspace_root: &Path,
    source: &Path,
    refresh: bool,
) -> Result<()> {
    let dest = session_root.join(LNK_NAME);
    if should_skip_output(&dest, refresh) {
        return Ok(());
    }
    if refresh {
        remove_if_exists(&dest);
    }

    let ps = format!(
        "$ws=New-Object -ComObject WScript.Shell;$s=$ws.CreateShortcut({});$s.TargetPath={};$s.WorkingDirectory={};$s.Save()",
        ps_escape_single_quoted(dest.to_string_lossy().as_ref()),
        ps_escape_single_quoted(source.to_string_lossy().as_ref()),
        ps_escape_single_quoted(workspace_root.to_string_lossy().as_ref()),
    );

    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .status()
        .context("spawn powershell for hc-agent.lnk")?;
    if !status.success() {
        anyhow::bail!("powershell exited with {status} while creating shortcut");
    }
    Ok(())
}

#[cfg(windows)]
fn ps_escape_single_quoted(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_places_destination() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("sess");
        fs::create_dir_all(&session).unwrap();
        let source = dir.path().join("real-hc-agent");
        fs::write(&source, b"fakebin").unwrap();
        let opts = SessionHcAgentBinOptions {
            mode: HcAgentSessionBinMode::Copy,
            source: Some(source),
            refresh: false,
        };
        maybe_install_session_hc_agent_bin(&session, dir.path(), &opts).unwrap();
        let dest = session.join(session_hc_agent_dest_file_name());
        assert!(dest.is_file(), "{dest:?}");
        assert_eq!(fs::read(&dest).unwrap(), b"fakebin");
    }

    #[test]
    fn off_leaves_session_empty_of_bin() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("sess");
        fs::create_dir_all(&session).unwrap();
        let opts = SessionHcAgentBinOptions {
            mode: HcAgentSessionBinMode::Off,
            source: None,
            refresh: false,
        };
        maybe_install_session_hc_agent_bin(&session, dir.path(), &opts).unwrap();
        assert!(!session.join(session_hc_agent_dest_file_name()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_places_destination() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("sess");
        fs::create_dir_all(&session).unwrap();
        let source = dir.path().join("real-hc-agent");
        fs::write(&source, b"x").unwrap();
        let opts = SessionHcAgentBinOptions {
            mode: HcAgentSessionBinMode::Symlink,
            source: Some(source.clone()),
            refresh: false,
        };
        maybe_install_session_hc_agent_bin(&session, dir.path(), &opts).unwrap();
        let dest = session.join(session_hc_agent_dest_file_name());
        assert!(dest.is_symlink(), "{dest:?}");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "x");
    }
}
