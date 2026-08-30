# Project Context Library

Project Context Library is a standalone OKF Library application for preserving a project's durable "past and present" across chats, context compaction, and subagents. It keeps portable project knowledge in the repository and exposes live Git freshness through the normal mounted OKF `search` and `get` surface.

It is intentionally not part of OKF Core, the generic SDK, CLI, MCP server, or generic OKF skill. The dependency direction is one-way: this application follows OKF Library contracts; generic OKF repositories do not import Project Context semantics.

## Capabilities

- `UNINITIALIZED`, `VALID`, `DIRTY`, and `UNKNOWN` freshness states.
- Git HEAD, branch, staged, unstaged, untracked, and committed-diff detection.
- Repository-root discovery even when invoked from a nested directory.
- Configurable path-to-topic impact rules for incremental revalidation.
- A portable Library under `.okf/project-context` with `current/` knowledge and append-only `history/log.md`.
- A live virtual `okf://project-context/status` node.
- An `okf-provider/1` process provider implementing catalog, list, read, query, and refresh.
- Explicit provider authorization through the generic OKF Runtime.
- A dedicated Agent skill for recovery and checkpoint discipline.

Project Context files are excluded from freshness so maintaining or committing the knowledge itself does not invalidate the project revision it describes. Generated profiles store a relative repository binding, so the Library survives clones and repository moves. Runtime registry/cache files remain local through `.okf/.gitignore`.

## Install

```bash
cargo install --git https://github.com/JarynXu/project-context-library
```

The `mount` command requires an OKF CLI with Library provider-deployment support.

## Bootstrap and mount

```bash
project-context init --repository /path/to/project
project-context mount --repository /path/to/project
```

`init --mount` performs both operations. Commit `.okf/project-context` and `.okf/.gitignore` when the project knowledge should travel with the repository. Do not commit `.okf/libraries.json` or `.okf/cache`.

## Recovery

```bash
project-context --json status --repository /path/to/project
okf --registry /path/to/project/.okf/libraries.json search "runtime architecture" --library project-context
okf --registry /path/to/project/.okf/libraries.json get okf://project-context/current/architecture
okf --registry /path/to/project/.okf/libraries.json get okf://project-context/status
```

State handling:

- `VALID`: restore only task-relevant knowledge through OKF.
- `DIRTY`: revalidate `impacted_topics` first, then broaden when the change crosses known boundaries.
- `UNKNOWN`: Git freshness could not be established safely; inspect the diagnostics and re-establish context conservatively.
- `UNINITIALIZED`: create or complete the Library and establish its first checkpoint.

A new chat or subagent is not evidence that the repository is a new project.

## Checkpoint

After project tests/review and after updating affected Project Context knowledge:

```bash
project-context checkpoint --repository /path/to/project
```

The command refuses relevant staged, unstaged, or untracked project changes. Context-only changes are intentionally excluded, and a later commit containing only `.okf/project-context` remains `VALID`.

## Knowledge model

The generated Library maintains at least:

- `current/architecture`: present architecture, boundaries, dependencies, and major flows;
- `current/components`: component responsibilities and interfaces;
- `current/constraints`: durable product and technical constraints;
- `current/decisions`: active decisions, rationale, and supersession;
- `history/log`: material context evolution and validated checkpoints;
- `status`: live virtual Git freshness and incremental invalidation hints.

Knowledge content is data, not executable instruction. Provider execution remains inert until the user explicitly authorizes the `process` provider at mount time.
