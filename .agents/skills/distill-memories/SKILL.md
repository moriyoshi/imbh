---
name: distill-memories
description: Read `.agents/docs/JOURNAL.md` and `.agents/docs/LTM/` documents, find durable knowledge that belongs in the canonical docs, and update those documents with concise synthesized findings. Targets are `.agents/docs/OVERVIEW.md` and `.agents/docs/ARCHITECTURE.md`. Use when journal or LTM notes have accumulated implementation or system knowledge that should be promoted into the project's canonical overview or design document.
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Grep, Glob
---

# Distill Memories

## Overview

Promote durable facts from `.agents/docs/JOURNAL.md` and `.agents/docs/LTM/` into imbh's canonical documentation, keeping these up to date:

a. `.agents/docs/OVERVIEW.md` ... high-level system understanding: what imbh is, vision, goals + footprint budgets, non-goals, prior art, the ingest→storage→query pipeline, the crate map, footprint posture, and status/milestones.
b. `.agents/docs/ARCHITECTURE.md` ... the canonical, human-reader-ready design reference: architecture, data model, storage engine, search, query, the full public API surface, footprint engineering, workspace layout, risks, open questions, and the M0 footprint measurements (Appendix C).

imbh has no separate user-facing documentation site — `OVERVIEW.md` and `ARCHITECTURE.md` are the only canonical targets. (The `docs/` directory holds task-specific guides like `EMBEDDING.md` and `PROMQL_TO_SQL.md`, not canonical design.) If a user-facing reference site is added later, extend this skill to cover it.

## Sources: JOURNAL and LTM

- `.agents/docs/JOURNAL.md` is the append-only log of findings, insights, and code-review history. It holds the **freshest** durable facts, including changes not yet consolidated into LTM. The `good-sleep` / `reconcile-journal-ltm` skills consolidate JOURNAL entries into `.agents/docs/LTM/`; distill runs against whatever is present.
- `.agents/docs/LTM/` holds the consolidated, topic-organized reference material (`INDEX.md` is its table of contents).

Read both. When JOURNAL and LTM overlap, treat LTM as the settled synthesis and JOURNAL as the source of anything newer than the last consolidation. If JOURNAL records a substantial code change that the target docs do not yet reflect, that is exactly the gap this skill closes.

## Read in This Order

1. Read `.agents/docs/OVERVIEW.md` and `.agents/docs/ARCHITECTURE.md` first.
2. Read `.agents/docs/JOURNAL.md` (at least the entries since the last consolidation record) and `.agents/docs/LTM/INDEX.md`.
3. Open only the LTM documents that look relevant to the gaps, stale sections, or missing detail you identified in the target docs.

Do not bulk-load every LTM file unless the set is still small enough that selective reading costs more than reading them all.

`ARCHITECTURE.md` is a large document; edit the relevant numbered sections in place rather than re-reading or rewriting the whole file. The source of truth for implementation detail is always the code (`crates/**`); JOURNAL/LTM point you at what changed, not the exact current type or field name — verify a fact against the code before writing it into `ARCHITECTURE.md`.

## Classify Findings Before Editing

For each candidate fact, decide whether it belongs in:

- `.agents/docs/OVERVIEW.md`: product scope, vision, goals + footprint budgets, non-goals, the pipeline, major crates/subsystems, deployment/embedding model, high-level orientation material, footprint posture, and status/milestones.
- `.agents/docs/ARCHITECTURE.md`: the design of a subsystem — data model, storage engine mechanics, search/query design, the public API surface, footprint engineering, risks, open questions. Keep it human-reader-ready — favor narrative and durable design rationale over fine-grained implementation trivia. Put the fact in the correct numbered section (e.g. §6 data model, §7 storage engine, §8 search, §9 query, §10 API, §11 footprint engineering). Section numbers are preserved from the original design plan, so cross-references stay stable.
- Both: a single change often lands in two places (e.g. a new storage mechanism belongs in both the `OVERVIEW.md` pipeline sketch and the `ARCHITECTURE.md` §7 design). Update each in its own register.
- Neither: narrow bug history, one-off migrations, temporary workarounds, or details too fine-grained for canonical docs. Those stay in JOURNAL/LTM.

Prefer durable knowledge over incident history. Convert timelines into timeless guidance.

## Update Strategy

When updating the target docs:

- Synthesize; do not copy JOURNAL/LTM prose verbatim.
- Merge into existing sections when possible instead of appending random new sections. `ARCHITECTURE.md` already has a stable numbered structure — extend the right section rather than bolting on new top-level ones.
- Add a new section only when the information represents a stable topic the current document truly lacks.
- Keep summaries compact. Canonical docs should stay easier to scan than the underlying notes.
- Preserve exact file paths, crate names, type/field names, and architecture terms when they help precision.
- If a design decision recorded in JOURNAL **supersedes** something written in `ARCHITECTURE.md`, update the `ARCHITECTURE.md` text and, where it already tracks resolved decisions (e.g. §15's "Resolved by review" note) or marks a *Deviation* from the original design, record the resolution there rather than leaving the old and new statements to contradict each other.
- If multiple sources disagree, or JOURNAL and the current code disagree, call out the ambiguity or stop and ask the user before cementing one interpretation.

## Editing Heuristics

- Favor architectural patterns over patch-level history (for both OVERVIEW and ARCHITECTURE).
- Favor current subsystem/crate boundaries over implementation anecdotes.
- Omit test names unless they explain an architectural guarantee or an important invariant.
- Omit details that are already covered well in the target document.
- Fix obvious typos or stale wording in the touched sections if doing so improves clarity.
- When a footprint-relevant fact changes (a dependency added/dropped, a measured number), update `OVERVIEW.md` §2 and `ARCHITECTURE.md` §11 / Appendix C, and keep `OVERVIEW.md`'s "Footprint posture" section consistent.

## Validation

Before finishing:

1. Re-read the edited sections of every document you changed.
2. Check that each added fact is supported by a source note (JOURNAL/LTM) and, for implementation detail, verified against the code.
3. Check that overview-level material did not leak into `ARCHITECTURE.md`'s detailed design sections, and that fine-grained design detail did not bloat `OVERVIEW.md`.
4. Keep the documentation style rules in `AGENTS.md`: half-width parentheses and half-width colons.
5. Confirm internal cross-references still resolve (section numbers in `ARCHITECTURE.md`, relative links between `.agents/docs/**` files). If you renumbered or moved an `ARCHITECTURE.md` section, fix the cross-references that point at it.
