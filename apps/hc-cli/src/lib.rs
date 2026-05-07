//! Honeycomb 统一 CLI：库入口供测试或与二进制相同的 `run()` 行为。

mod app;

pub fn run() -> anyhow::Result<()> {
    app::run()
}
