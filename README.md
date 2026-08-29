# Project Context Library

Project Context Library is a concrete application of the OKF Library Framework. It keeps durable project understanding outside conversational memory and exposes live Git freshness through the same mounted OKF knowledge space used by other Libraries.

It is intentionally **not** part of OKF Core, the generic SDK, CLI, MCP server, or generic OKF skill.

## What it provides

- `UNINITIALIZED / VALID / DIRTY / UNKNOWN` project-context freshness.
- Git HEAD, branch, staged, unstaged, and untracked change detection.
- Configurable path-to-knowledge impact rules for incremental revalidation.
- A generated OKF Library at `.okf/project-context`.
- Static durable knowledge under `current/` and append-only `history/log.md`.
- Live virtual `okf://project-context/status` knowledge.
- An `okf-provider/1` process provider for catalog/list/read/query/refresh.
- A dedicated Agent skill for recovery and checkpoint discipline.

The Library's own `.okf/project-context` maintenance files, the OKF runtime registry, and runtime cache are excluded from project freshness so maintaining context does not make the project permanently dirty.

## CLI

```bash
# Install the application binary
cargo install --path .

# Bootstrap a target repository
project-context init --repository /path/to/project

# Install and mount it into that repository's OKF Runtime
project-context mount --repository /path/to/project

# Inspect freshness
project-context status --repository /path/to/project
project-context --json status --repository /path/to/project

# After tests/review and only with a clean working tree
project-context checkpoint --repository /path/to/project
```

`checkpoint` refuses a dirty project working tree rather than recording only `HEAD` and silently losing the meaning of uncommitted changes.

## Agent recovery flow

1. Run `project-context status`.
2. If `VALID`, restore task-relevant knowledge with ordinary `okf search` / `okf get`.
3. If `DIRTY`, use `changed_paths` and `impacted_topics` to revalidate only affected knowledge first.
4. If `UNKNOWN`, conservatively re-establish freshness before modifying the project.
5. If `UNINITIALIZED`, bootstrap the Library once.
6. After authorized project work, update affected knowledge and checkpoint only after project validation and a clean working tree.

A new chat or subagent is not evidence that a repository is a new project.

## Architecture

```text
Project Context application
        │
        │ generates / maintains
        ▼
.okf/project-context (concrete Library)
        │
        │ process provider: okf-provider/1
        ▼
Generic OKF Library Runtime
        │
        ├── okf search
        └── okf get okf://project-context/...
```

The dependency direction is one-way: this application follows OKF Library contracts; generic OKF repositories never import Project Context semantics.
