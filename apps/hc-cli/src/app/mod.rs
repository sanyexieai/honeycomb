//! Honeycomb CLI 应用层：命令行实现集中在 [`cli`] 子模块（单文件，便于搜索与跳转）。

mod cli;

pub fn run() -> anyhow::Result<()> {
    cli::run()
}
