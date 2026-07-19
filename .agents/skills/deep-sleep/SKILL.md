---
name: deep-sleep
description: "Consolidate related long-term memory documents under .agents/docs/LTM/ into broader synthesis documents and refresh the LTM index, keeping the source documents intact. Use when LTM has grown into overlapping topic files that should be merged into higher-level syntheses."
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Grep, Glob
---

# Deep Sleep: Consolidate Long-Term Memory Documents

This skill reads `.agents/docs/LTM/INDEX.md` and the relevant documents under `.agents/docs/LTM/`, groups overlapping topics into broader synthesis documents, and refreshes the index. Use it after `good-sleep` has already distilled chronological journal entries into topic-oriented LTM files and those files now need a second-stage consolidation.

## Goal

Turn a set of narrow LTM notes into a smaller set of durable synthesis documents while keeping the source documents intact for traceability.

## Step 1: Inspect the Existing LTM Set

Read `.agents/docs/LTM/INDEX.md` first, then inspect only the LTM documents needed to understand the candidate clusters.

Look for:

- Multiple documents about the same subsystem or crate
- Repeated pitfalls, file references, or test guidance across documents
- One overview document plus several implementation-detail documents that should be summarised together

Do not bulk-load every file unless the LTM set is still small enough that doing so is cheaper than selective reading.

## Step 2: Propose Consolidation Clusters

Before writing, present a plan to the user with:

- The synthesis documents you propose to create
- The source LTM documents that feed each synthesis document
- Any source documents that should remain standalone because they are already cohesive

Cluster by durable topic, not by date and not by arbitrary file-count balancing.

## Step 3: Write Synthesis Documents

Create new synthesis documents under `.agents/docs/LTM/` using descriptive kebab-case filenames such as:

- `storage-engine-synthesis.md`
- `query-path-synthesis.md`

Keep the original source LTM documents. Do not delete or overwrite them unless the user explicitly asks for replacement.

Use this structure:

```markdown
# <Synthesis Title>

## Summary

<2-4 sentence overview of the merged topic and why it matters>

## Included Documents

| Document | Focus |
|----------|-------|
| [source-a.md](./source-a.md) | <short note> |
| ... | ... |

## Stable Knowledge

<Bulleted list of the durable facts, constraints, and design decisions>

## Operational Guidance

<How an agent should approach work in this area>

## Files

<Important file paths / crates and why they matter>

## Tests

<Regression coverage and command examples, if applicable>

## Pitfalls

<Failure modes, tricky assumptions, and gotchas>
```

## Step 4: Synthesize, Do Not Merely Concatenate

When merging documents:

- Deduplicate repeated explanations
- Convert chronological narratives into timeless guidance
- Preserve exact file paths, commands, and test names when they are useful
- Keep contradictions visible; if two source docs disagree, call that out explicitly instead of silently choosing one

Prefer compact synthesis over exhaustive restatement. The new document should help future agents orient quickly and then drill into the source docs only when needed.

## Step 5: Refresh the Index

Update `.agents/docs/LTM/INDEX.md` so it clearly distinguishes:

- Source topic documents
- Synthesis documents

A simple approach is to keep one table for synthesis docs and one table for source docs. Each synthesis entry should mention which source documents it consolidates.

## Step 6: Record the Consolidation

Append a short note to `.agents/docs/JOURNAL.md` describing:

- Which synthesis documents were created
- Which source LTM documents they consolidate
- Any documents intentionally left standalone

Do not edit or delete existing journal sections. Only append.

## Guardrails

- Do not delete source LTM documents without explicit user approval
- Do not collapse unrelated topics just to reduce file count
- Prefer a few high-value synthesis documents over a full rewrite of the entire LTM tree
- Preserve the documentation style rules in `AGENTS.md`: half-width parentheses and half-width colons

## Notes

- This skill complements `good-sleep`; it does not replace it
- Re-running this skill should extend or refresh synthesis documents when new source LTM files appear
- If the current LTM set is already small and non-overlapping, say so and avoid forced consolidation
