---
name: tackle-todos
description: "Read TODO.md and scan source code for TODO/FIXME comments and todo!()/unimplemented!() markers, build a consolidated list, then dispatch parallel agents to address as many items as possible."
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Grep, Glob, Agent
---

# Tackle TODOs: Consolidate and Resolve Open Items

This skill scans the project for all outstanding work items — both from `.agents/docs/TODO.md` and from `// TODO` / `// FIXME` comments and `todo!()` / `unimplemented!()` markers in Rust source — builds a consolidated, deduplicated, prioritized list, and then dispatches parallel agents to resolve as many items as possible.

**Use this skill when:** you want to make a focused sweep of outstanding TODOs and fix them in bulk.

## Arguments

- `[filter]` (optional): A crate name or keyword to restrict which TODOs to tackle (e.g., `storage`, `index`, `query`, `otlp`). If omitted, all TODOs are considered.

## Step 0: Collect TODOs from TODO.md

Read `.agents/docs/TODO.md` and extract every unchecked `- [ ]` item. Record its area, description, and source.

## Step 1: Scan Source Code for TODO/FIXME Markers

Use Grep to search the repo for `// TODO`, `// FIXME`, `todo!(`, and `unimplemented!(` patterns in `*.rs` files.

Group and deduplicate the results:
- Identical or near-identical comments repeated across many files should be collapsed into a single work item with a note about which files/crates are affected.
- Comments that are informational-only (e.g., noting a known limitation that cannot be fixed without large design changes) should be flagged but deprioritized.
- `todo!()` / `unimplemented!()` in non-test code are runtime panics — treat them as behavioural gaps, not mere notes.

## Step 2: Build a Consolidated TODO List

Merge the two sources into a single list. Deduplicate items that appear in both TODO.md and as code markers.

For each item, assign a category:

| Category | Description | Priority |
|----------|-------------|----------|
| **systematic** | Same pattern repeated across many files | High — one fix propagates widely |
| **behavioural** | Missing or incorrect behaviour affecting correctness (incl. `todo!()`/`unimplemented!()` in live paths) | High |
| **validation** | Missing input validation or error handling | Medium |
| **serialization** | Wire format, encoding, or storage-format bugs (OTLP, Parquet, WAL, manifest, canonical JSON) | Medium |
| **test-only** | TODO in test code noting a test gap, not a code defect | Low |
| **design** | Requires significant design work or new abstractions | Deferred — flag for user |

Write the consolidated list to `.agents-workspace/tmp/consolidated-todos.md` for reference.

## Step 2b: Verify stale items before dispatch

Before dispatching agents on any TODO entry that is more than 24 hours old, verify the entry is still applicable: grep the relevant code for the symptom (the call site, the missing handler, the `todo!()`, etc.). Skip or close stale entries instead of dispatching.

## Step 3: Filter (if argument provided)

If the user passed a `[filter]` argument, restrict the work list to items matching that filter (crate name or keyword).

## Step 4: Plan Parallel Work Items

Group the consolidated TODOs into independent, parallelizable work units. Each work unit should:
- Be self-contained (touching one crate or one cross-cutting concern)
- Not conflict with other parallel work units (no two agents editing the same file)

Respect the crate dependency direction (`core ← {otlp, storage, index, query} ← imbh ← {exporter, server}`) when judging independence — a change to `imbh-core` types can ripple into every downstream crate, so treat core-schema changes as their own serial unit rather than parallelizing them against dependents.

Present the plan to the user and get confirmation before dispatching.

## Step 5: Dispatch Parallel Agents

For each approved work unit, launch an Agent (subagent_type: general-purpose) with a clear prompt that includes:

1. The specific TODO(s) to address
2. The files/crate to modify
3. The expected behaviour
4. Instructions to run the local gate after making changes: `cargo fmt` on the touched crate, then `cargo build --workspace`, `cargo clippy -p <crate> --all-targets -- -D warnings`, and the relevant focused tests (`cargo test -p <crate>`).

Launch as many agents in parallel as there are independent work units, in a single batch.

❌ Never use `isolation: worktree` for parallel agents — it has repeatedly caused trouble. Instead, launch tasks that are independent of one another in a batch.

## Step 6: Collect Results and Update TODO.md

After all agents complete:

1. Review each agent's results — check if the change built, clippy'd clean, and tests passed.
2. For successfully resolved items, mark them as `- [x]` in `.agents/docs/TODO.md` and remove the corresponding `// TODO` / `// FIXME` comment or `todo!()` marker from source code.
3. For items that could not be resolved, add notes about why and what was attempted.
4. Append a summary to `.agents/docs/JOURNAL.md` documenting what was tackled and the outcomes.

## Notes

- **Do not attempt design-category items** without user approval. These require architectural decisions — many of imbh's are still open in `ARCHITECTURE.md` §15.
- Respect `AGENTS.md` rules: no `git checkout`, no `git restore`, no discretionary commits. Agents should only edit files, not commit.
- When running tests, always use focused crate targeting (`cargo test -p <crate>`) rather than the whole workspace when iterating. Keep the default test path hermetic — no external daemons or network.
