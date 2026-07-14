# Reference vault provenance

This is a real, minimal Obsidian vault maintained as Grimmore's deterministic indexing corpus. Its original prose was authored for this project on 2026-07-13 and is dedicated to the public domain under CC0-1.0.

The notes summarize implementation decisions rather than inventing feed items, jobs, people, or activity. Technical claims link to their primary references:

- Obsidian developer documentation: <https://docs.obsidian.md/Plugins/Vault>
- SQLite FTS5 documentation: <https://sqlite.org/fts5.html>
- Model Context Protocol specification: <https://modelcontextprotocol.io/specification/2025-11-25>
- Andrej Karpathy's LLM operating notes: <https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f>

The `.obsidian/app.json` file makes the fixture an actual Obsidian vault. Tests copy this captured vault into a temporary directory before exercising mutation or reconciliation behavior; the committed source fixture remains read-only.
