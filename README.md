# Honeycomb

Honeycomb is a Rust-first ecosystem centered on a cross-platform command runtime and an optional UI shell.

## Product Split

- `honeycomb-core`: the cross-platform command runtime
- `honeycomb-ui`: the optional graphical shell on top of the core

## Principles

- Rust first for runtime, CLI, and protocols
- Markdown first for durable human-readable data
- Windows are views, not execution units
- Instances communicate; workers execute
- Shared protocols instead of shared hidden internals
- Monorepo for incubation, independent binaries and crates for evolution

## Workspace Layout

```text
apps/
  hc/                 Core CLI entry
crates/
  hc-protocol/        Shared schemas and traits
  hc-core/            Runtime model and orchestration
  hc-store/           Markdown-first storage layer
  hc-ui/              Slint-based desktop shell
docs/
  architecture.md
  core.md
  storage.md
workspace/
  sessions/
  instances/
  decisions/
  indexes/
```

## Current Status

This repository currently contains:

- architecture documents aligned to the core-first design
- a Rust workspace skeleton for the runtime and UI split
- initial runtime data models
- Markdown-first storage guidance
- a first `hc-ui` multi-window Slint shell prototype

## Run

Core terminal:

```powershell
cargo run -p hc
```

Desktop multi-window shell:

```powershell
cargo run -p hc-ui
```

Implementation should proceed in phases:

1. `hc-protocol`, `hc-core`, `hc-store`
2. `hc-ui`
3. optional higher-level extensions

See [docs/architecture.md](/D:/code/honeycomb/docs/architecture.md), [docs/core.md](/D:/code/honeycomb/docs/core.md), and [docs/storage.md](/D:/code/honeycomb/docs/storage.md).
