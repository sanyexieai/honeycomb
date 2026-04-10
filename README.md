# Honeycomb

Honeycomb is a Rust-first ecosystem centered on a cross-platform command runtime, an agent orchestration layer, and an optional UI workbench.

## Product Split

- `honeycomb-core`: the cross-platform command runtime
- `honeycomb-agent`: the task-to-agent orchestration layer
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
  hc-agent/           Task-driven agent bootstrap and orchestration
  hc-responder/       Shared reply/responder abstraction layer
  hc-trace/           Activity, decision-trace, and behavior-code system
  hc-llm/             Minimal pluggable LLM core
  hc-persona/         Persona profiles and collaboration rules
  hc-memory/          Memory records and recall/writeback primitives
  hc-capability/      Shareable capability profiles
  hc-store/           Markdown-first storage layer
  hc-ui/              Slint-based desktop shell
docs/
  architecture.md
  core.md
  storage.md
  task-driven-product.md
  ui-workbench.md
  working-rules.md
workspace/
  tenants/
    local/
      users/
        default/
          personas/
          capabilities/
          memory/
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
- a task-driven `hc-agent` orchestration layer
- an independent `hc-trace` system for observability and decision tracking
- namespaced persona, capability, and memory repositories
- a minimal `hc-llm` crate with provider abstraction and OpenAI-compatible support
- Markdown-first storage guidance
- a first `hc-ui` multi-window Slint shell prototype

## Run

Core terminal:

```powershell
cargo run -p hc
```

With an explicit tenant/user namespace:

```powershell
$env:HC_TENANT_ID="local"
$env:HC_USER_ID="alice"
cargo run -p hc
```

Standalone LLM CLI:

```powershell
cargo run -p hc-llm-cli -- config llm --provider openai --api-key <your-key>
cargo run -p hc-llm-cli -- config show
cargo run -p hc-llm-cli -- providers
cargo run -p hc-llm-cli -- generate "hello"
```

With a real OpenAI-compatible endpoint:

```powershell
$env:OPENAI_API_KEY="..."
cargo run -p hc-llm-cli -- providers
cargo run -p hc-llm-cli -- generate "hello" --provider openai --model gpt-4.1-mini
```

Desktop multi-window shell:

```powershell
cargo run -p hc-ui
```

UI also respects `HC_TENANT_ID` and `HC_USER_ID`.

Implementation should proceed in phases:

1. `hc-protocol`, `hc-core`, `hc-store`
2. `hc-ui`
3. `hc-agent`, `hc-persona`, `hc-memory`, `hc-capability`, and optional higher-level extensions

See [docs/architecture.md](/D:/code/honeycomb/docs/architecture.md), [docs/core.md](/D:/code/honeycomb/docs/core.md), [docs/storage.md](/D:/code/honeycomb/docs/storage.md), and [docs/ui-workbench.md](/D:/code/honeycomb/docs/ui-workbench.md).

Implementation guardrails and task-first product flow are documented in:

- [docs/task-driven-product.md](/D:/code/honeycomb/docs/task-driven-product.md)
- [docs/working-rules.md](/D:/code/honeycomb/docs/working-rules.md)
