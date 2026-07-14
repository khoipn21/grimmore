---
id: knowledge-context-engineering
type: knowledge
status: evergreen
tags:
  - ai
  - context-engineering
  - agents
source: https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f
freshness:
  reviewed: 2026-07-13
  review_after: 2026-10-13
---

# Context engineering for durable agents

An agent performs better when its working context is deliberately selected, compact, and grounded in durable evidence. The useful unit is not an endless chat transcript; it is a bounded task packet containing the goal, current state, relevant source material, decisions, and verification evidence.

Grimmore separates durable Markdown knowledge from rebuildable retrieval indexes. Projects can ingest only their scoped notes while general learning can draw from approved evergreen knowledge. Retrieval should preserve provenance and freshness so a model can distinguish a current primary source from an old summary.

## Practical rules

- Keep goals and acceptance criteria explicit.
- Retrieve the smallest evidence set that can answer the task.
- Treat generated summaries as claims that still need source links.
- Write reviewed knowledge through Obsidian's vault API, never through an external indexer.
- Expire or re-review time-sensitive notes instead of silently trusting stale context.
