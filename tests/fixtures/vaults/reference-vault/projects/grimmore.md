---
id: project-grimmore
type: project
status: active
tags:
  - local-first
  - obsidian
  - rust
---

# Grimmore

Grimmore is a single-user, local-first second brain for Obsidian. The plugin owns all Markdown writes. A Rust companion owns operational SQLite data, full-text search, provider calls, schedules, and read-only MCP access.

## Current milestone

Prove the vertical slice with a provenance-backed vault, bundled SQLite FTS5, bounded queries, private authenticated IPC, and a revision-checked `Vault.process` write.

## Non-goals

- No user accounts or hosted control plane.
- No automatic job applications.
- No MCP write tools in version one.
- No second desktop shell or standalone HTML application.
