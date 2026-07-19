# imbh-test-support

Shared test support for IMBH's integration and end-to-end suites.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust. This is an internal dev-only harness (`publish = false`); it is
> not published to crates.io.

This crate is **dev-only**: it is pulled in exclusively through the `[dev-dependencies]` of the
test crates, so it never enters the shipping `imbh` / `imbhd` graph (zero footprint — see
ARCHITECTURE.md §11/§12 and TESTING.md). It consolidates helpers that were previously copy-pasted
across per-crate `#[cfg(test)]` modules:

- `otlp` — OTLP protobuf builders returning encoded request bytes.
- `http` — a tiny blocking HTTP/1.1 client for driving the reference `imbhd` server.
- `harness` — a re-exec harness for multi-process tests (crash / cross-process).
- `assert` / `rt` / `procinfo` — result assertions, a current-thread tokio runtime, and a `VmRSS`
  reader.

## Role in the workspace

A dev-dependency only — never a dependency of any shipping crate. Depends on the
[`imbh`](https://crates.io/crates/imbh) facade (and tokio) for its helpers.

See the design references
[`TESTING.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/TESTING.md) and
[`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§11, §12. License: Apache-2.0.
