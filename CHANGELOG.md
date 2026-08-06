# Changelog

All notable changes to IMBH are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0/).

Every crate in the workspace shares one version and is released together under a single `vX.Y.Z`
tag (see `[workspace.metadata.release]` in the root `Cargo.toml`). On release, cargo-release closes
the `## [Unreleased]` section below into a dated version heading and opens a fresh one; write new
entries under `## [Unreleased]` as you go. The heading must stay present and multiline-anchorable —
the `pre-release-replacements` in `crates/imbh/Cargo.toml` match it with `exactly = 1`, so a
release aborts if it is missing or duplicated.

## [Unreleased]

## [0.6.0] - 2026-08-06

### Added

- **The Docker log driver remaps container lines with VRL: `imbh-server`'s new `docker-remap`
  feature.** A Vector Remap Language program runs between the reassembled wire entry and the OTLP
  record. The built-in script recognises JSON, logfmt, klog/glog and `key=value`, lifting each line's
  own severity, timestamp and trace context onto the record and leaving its fields queryable in the
  body. The event a script receives carries both data models at once — Docker's log-driver fields
  (`.line`, `.source`, `.time_nano`, `.info.*`) and the OTel record the driver would have stored on
  its own. That seeding is what makes the identity script `.` byte-for-byte the previous behaviour,
  and it is why the built-in script only ever *overrides*: it never re-derives `service.name`,
  `container.*` or `log.iostream`.

  A script is operator input, so three invariants are re-asserted after every run. The resource's
  `container.id` is overwritten (`ReadLogs` filters history on it, and a wrong id would merge two
  containers' histories), `service.name` is restored when absent or empty, and `log.iostream` is
  restored if removed. A runtime error stores the line un-remapped rather than losing it; an explicit
  `abort` drops it. A parsed timestamp is accepted only within 26h of Docker's capture time, because
  `ReadLogs` pages and computes its follow watermark from that clock. Configuration is one grammar
  shared by `--log-opt imbh-remap` (per container) and `IMBH_DOCKER_REMAP` (daemon-wide): `default`,
  `off`, `@PATH`, or an inline script. See `docs/DOCKER_LOG_DRIVER.md` "Remapping".

  **Breaking for the published plugin**, which ships with the feature enabled: a recognised line's
  `body` is now the parsed fields rather than the raw text, and `docker logs` re-renders it as a
  logfmt line (`ts=… level=… k=v`) instead of replaying the original bytes — so a query reading
  `body` as text sees structured data for those lines. `--log-opt imbh-remap=off` or
  `IMBH_DOCKER_REMAP=off` restores the previous behaviour, which is also what a build without
  `docker-remap` does; a plain string body still leaves `docker logs` verbatim, so OTLP-ingested
  records are unaffected.

  This is the one feature in the workspace that adds crates on purpose: `imbh-server`'s plugin
  feature set goes 308 → 397 crates and the plugin binary 34.3 → 38.1 MiB. Neither axis the footprint
  gate enforces moves — it counts `cargo tree -p imbh` (the facade, which `imbh-server` sits *above*)
  and builds `imbhd` at default features — so `scripts/footprint-gate.sh` now also prints an
  informational, never-failing measurement of the plugin build. vrl is the graph's first MPL-2.0
  component (MPL §3.3 keeps the Larger Work Apache-2.0), and it falsifies two `ARCHITECTURE.md` §11
  statements, corrected there: C code is no longer libzstd-only (`stdlib-base` forces vrl's `datadog`
  feature, which pulls `onig_sys`), and `lz4_flex` is now a dependency. `deny.toml` and `about.toml`
  gain MIT-0 and 0BSD accordingly.

- **`LogQuery::observed_after` and `LogOrder { Time, ObservedTime }`** expose the arrival clock
  (`observed_time`, already stored on every log row) as a filter and sort axis. Additive: the `SELECT`
  projection is unchanged, and both new fields carry `#[serde(default)]` so previously serialized
  `LogQuery` JSON still deserializes.

- **`GET /stats` and the MCP `db_stats` tool report the ingest gauges, and their output parses back
  into a typed value.** Both are served by one hand-written serializer, `imbh_mcp::stats_json`, which
  reported `buffer_bytes`, `wal_bytes`, `durable_lsn` and the per-table breakdown — but none of
  `ingest_queue_depth`, `ingest_dropped`, `ingest_errors` or `ingest_rejected`. An operator watching
  `/stats` could therefore not see an async-ingest queue backing up, an `Overflow::DropOldest`
  eviction, a worker failure, or a duplicate-timestamp rejection, all of which are losses with no
  caller left to report them to.

  Rather than widen the hand-written writer, `stats_json` now converts a `DbStats` into
  `imbh_head::dto::Stats` (a new `From<&imbh::DbStats>`, which `exec::stats` also uses) and
  serializes *that* derive. `GET /stats`, `db_stats` and `GET /api/head/stats` are consequently one
  serializer with one shape, and the plain endpoint's body deserializes into `imbh_head::dto::Stats`
  like the head API's does — which is also how the `db_stats` tool's parse-back is now guaranteed to
  succeed. `imbh-mcp` gains an `imbh-head` dependency (`dto` only) to do it; the feature adds no
  third-party crate to any graph, and none of it reaches the `imbh` facade.

### Changed

- **BREAKING (Docker log-driver plugin): the database is now provisioned by the daemon, not
  bind-mounted from a host path.** `config.json` drops the settable `data` mount and declares
  `/var/lib/imbh` as the plugin's `propagatedMount`, so Docker creates the storage behind it at
  `plugin enable`.

  The old configuration could not be installed without a manual step nobody documented: a bind mount
  needs its source directory to already exist, and a missing bind *source* is the one thing the
  daemon will not create for a plugin. `docker plugin enable` failed with `error mounting
  "/var/lib/imbh" to rootfs … no such file or directory` — after a `plugin set` that had reported
  success, and with no `docker plugin logs` to consult. The plugin could not fix this itself: mounts
  are established before the entrypoint runs, so `imbhd` never got far enough to call the
  `create_dir_all` it already had. It was worst on Docker Desktop, where the path is resolved inside
  the Linux VM and `sudo mkdir` on the Mac or Windows host has no effect on it.

  Install is now a single `docker plugin install` on every daemon, Docker Desktop included, with no
  host directory to create, one fewer permission to grant, and no dependence on Desktop's host file
  sharing — a FUSE-family filesystem that imbh's advisory `flock` and memory-mapped segments have no
  business relying on.

  **Migrating from 0.5.0:** the database moves from the host path you set as `data.source` into
  `/var/lib/docker/plugins/<id>/propagated-mount`; existing logs are not carried over, and the old
  host directory is left untouched for you to keep or delete. **`docker plugin rm` now deletes the
  database with the plugin** — measured, no prompt and no undo — and the database can no longer be
  placed on a different disk, so size `Retention` against the filesystem holding `/var/lib/docker`.
  `docker plugin disable`/`enable`, including the cycle every `docker plugin set` requires, leaves it
  intact. See `docs/DOCKER_LOG_DRIVER.md` "Where the database lives" for backup and restore.
- **Breaking: `GET /stats` and the MCP `db_stats` tool spell a `None` durable LSN as `null` instead
  of `0`.** Zero is not a legal LSN — `imbh::Lsn` is a `NonZero<u64>` — so "nothing is durable yet"
  was indistinguishable from a real watermark to any typed reader, and it is the one part of this
  change a current consumer can see. A consumer that compared the number against a receipt's LSN was
  already correct by accident and stays correct; one that parsed the field as a plain integer must
  now accept `null`.
- **`imbh_head::dto::Stats::durable_lsn` and `dto::TableStats`'s `min_time_unix_nano` /
  `max_time_unix_nano` are serialized as `null` when absent** rather than omitted, now that
  `GET /stats` shares the derive and has always emitted an explicit `null` for an absent per-table
  bound. They keep `#[serde(default)]`, so a payload that omits them still deserializes and older
  peers are unaffected in that direction.

### Fixed

- **Compaction could silently corrupt a day partition whose segments were sealed under different
  promoted-key sets.** `concat_batches` takes columns **positionally** and does not validate them
  against the schema it is handed, and compaction passed it `batches[0].schema()` — the first
  segment's. Changing `DbBuilder::promote(...)` between two seals in the same UTC day therefore gave
  one of three outcomes depending on segment order: a panic (`index out of bounds`) when the first
  segment was wider, **silent** truncation of the later segments' promoted columns when it was
  narrower, or — worst — **silent** concatenation of two differently-named promoted columns into one
  when the widths matched, labelled with whichever name the first segment used. Two of the three
  failed silently and wrote the result back as the merged segment. Compaction now coerces every batch
  read off disk to the table's current canonical schema first. Note this was reachable through a
  documented-as-supported operation: `ARCHITECTURE.md` §6.1 called adding or removing promoted keys
  backward-compatible, which was true of the read path but not the compaction path.

- **The Docker log driver's `--tail 0 -f` could miss a line emitted just before the follow started.**
  The follow watermark jumped to `Timestamp::now()` on the **event** clock, but a record's timestamp
  is when the container emitted the line while ingest lands it up to one batch interval later, so
  event time is not monotone in arrival. The driver now watermarks and follows on `observed_time`,
  which for this driver is dockerd's capture stamp. `--tail N` and full history are unchanged and
  still print in event-time order. See `docs/DOCKER_LOG_DRIVER.md` for the residues this does not
  eliminate — a VRL script can overwrite `.observed_timestamp`, an exact-nanosecond tie is still
  broken once, and `--tail 0` has no uniquely correct cut against a batching store.

- **Typed `MetricsApi::range`/`instant` no longer inflate `sum`/`count` on duplicate metric points**
  under `Duplicates::LastWins`, bringing them into line with PromQL. The surviving point of a
  duplicated instant is chosen by the same total order on the **value** that §10.5.1 already
  specified — never by scan order — so the result stays a pure function of the fetched sample
  multiset. Under every other policy the emitted SQL is unchanged. `Duplicates::ErrorOnRead` (the
  default) still does not *fail* on a duplicate the way PromQL does; that asymmetry is deliberate and
  documented.

## [0.5.0] - 2026-08-05

### Added

- **A duplicate-timestamp policy for metric ingest and PromQL reads: `Duplicates`.** Two metric
  datapoints sharing a series **and** a timestamp have no PromQL meaning, and imbh said so at *read*
  time — one duplicated point made every PromQL query of that metric fail, over every window, for as
  long as the points stayed within retention, while ingest reported success. The diagnostic named no
  metric, no labels and no timestamp, and nothing could delete the offending points.

  One knob, on `DbBuilder::duplicates` and as `IMBH_DUPLICATES` on `imbhd`, with three answers.
  `ErrorOnRead` (the default) keeps the historical behavior, but the new
  `SemanticError::DuplicateTimestamp` names the metric, the label set and the instant, and reaches
  clients as a `400` carrying `dto::KIND_DUPLICATE_TIMESTAMP` — so a head can say "fix the producer"
  rather than suggest the shorter time range that will not isolate it. `LastWins` collapses the
  duplicated instant at read time, degrading one datapoint instead of the whole metric; it is the
  only remedy for points already written, since no ingest-side policy can repair stored data.
  `Reject { recent }` drops the repeat at ingest and finally gives `IngestReceipt::rejected` a
  non-zero value, reported through the HTTP ingest JSON, OTLP/gRPC
  `partial_success.rejected_data_points`, `DbStats::ingest_rejected`, and one `tracing` warning per
  rejecting export.

  The ingest guard is a bounded two-generation `(series_hash_128, timestamp)` set rather than the
  obvious per-series `last_timestamp` map. A `ts <= last_ts` rule is order-sensitive, and because the
  WAL stores the raw OTLP body and replay re-derives the unsealed tail with a guard that starts
  empty, it could reject on replay a point the writer had accepted — data loss. The set rule is
  order-commutative, so the replay guard's key set is always a subset of the writer's and **replay is
  strictly more permissive**: it can never drop a row the writer kept. That also lets the guard sit at
  the decode site rather than under the storage lock, which is the only place the async ingest path
  can report an exact rejection count, and leaves `Storage::ingest_metrics` unchanged. Out-of-order
  and late-arriving points stay accepted, as the storage engine has always allowed. Nothing is
  allocated unless `Reject` is configured; it then costs a fixed ~13 MB at the default lookback, with
  no new dependency in any build (the guard is `std`-only, so the producer-only profile that has no
  DataFusion gains nothing).

- **The Docker log-driver plugin is published, per architecture:
  `ghcr.io/moriyoshi/imbh-log-driver`.** A managed plugin cannot share a tag with an image — its
  manifest points at an `application/vnd.docker.plugin.v1+json` config, so `docker pull` refuses a
  plugin and `docker plugin install` refuses an image — so the plugin gets its own GHCR repository,
  built and pushed by a new `plugin` job in `release.yml`. Managed plugins also have no manifest-list
  support, so each architecture is its own tag (`X.Y.Z-amd64` / `X.Y.Z-arm64`, plus the floating
  `X.Y-<arch>` and `latest-<arch>`); there is deliberately no bare `X.Y.Z`, which would be silently
  wrong for half the users.

### Fixed

- **A metric recorded as both a gauge and a sum made PromQL fail on a well-formed database.** An
  instant selector queries the gauge *and* sum tables and concatenates the results, and the derived
  label set (`service` + `__name__` + the string attributes) does not distinguish the two — so one
  metric name emitted as both instrument kinds produced byte-identical label sets at one timestamp
  and tripped the duplicate-timestamp rejection. No bad producer was required.

### Changed

- **Breaking: `SemanticError` gains a `DuplicateTimestamp(String)` variant and is now
  `#[non_exhaustive]`,** so a future failure mode needing its own payload is additive; match with a
  `_` arm. The existing variants keep their `&'static str` payloads.
- **Breaking: `DbStats` gains `ingest_rejected`,** alongside the existing `ingest_dropped` /
  `ingest_errors` gauges. `imbh_head::dto::Stats` gains the same field as `#[serde(default)]`, which
  keeps the head wire format additive in both directions.

## [0.4.0] - 2026-08-02

### Added

- **The terminal explorer can drive a running `imbhd`: `imbh-tui --url http://host:4318`.** Until now
  the TUI only opened a directory, through `Db::open_read_only` — a view that cannot see the writer's
  *unsealed* buffer, i.e. the most recent telemetry of all. Pointed at a daemon it asks the process
  that owns those rows, so the newest data is on screen; as a side effect the database may now live on
  another machine. `imbh-tui <directory>` is unchanged and still opens in-process.

  The surface is a new published crate, **`imbh-head`** (ARCHITECTURE.md §10.19), sitting below both
  consumers because §12 forbids `imbh-tui` reaching into `imbh-server`. It is three layers:
  `imbh_head::dto` (the wire types — where the facade already has a `serde`-gated type for something,
  `LogQuery` / `LogPage` / `Trace` / `MetricMeta`, the wire *is* that type), `imbh_head::exec` (the
  eleven operations, executed against an open `Db`), and `imbh_head::client` (the HTTP client a remote
  head uses). The load-bearing property is that `exec` is the **single** implementation: `imbhd` calls
  it behind `/api/head/…`, the TUI's local backend calls it in-process, so the query-language
  translation, the evaluation caps, and the trace-window narrowing cannot diverge between the two
  modes. Deliberately *not* folded into `imbh-mcp`: that surface is shaped for a model and lossy by
  design (no paging cursors, no per-sample matrices, no waterfall), and reshaping it for a UI would
  change what every agent sees.

  Row-shaped results (the PromQL/LogQL matrices, the TraceQL matches, a log page, a trace) answer as
  **Arrow IPC**; scalar ones stay JSON. That is soundness, not taste — JSON has no `NaN`/`±Inf` and
  `serde_json` writes all three as `null`, which then fails to read back as an `f64`, and a PromQL
  evaluation produces all three routinely. Anything not row-shaped (paging cursor, scan counters, the
  assembled trace header, the narrowed window start) rides in the IPC schema metadata.

  New public API: the `imbh-head` crate, and `imbh_server::head` with `head::routes() -> Router<Arc<Db>>`
  so a host can mount the head surface alone — a UI's backing daemon with no ingest and no admin
  actions — or leave it out entirely. The endpoints are **read-only** (nothing below `/api/head`
  ingests, flushes, compacts, or applies retention) and, like the rest of `imbhd`, unauthenticated, so
  a real deployment gates the prefix. Footprint is untouched where it is measured: `arrow-ipc` is
  already compiled wherever DataFusion is, and everything `reqwest` pulls (hyper, tower, bytes,
  http-body-util) is already compiled for `imbh-server` — it is new only to the `imbh-tui` binary, and
  neither reaches the `imbh` facade the gate measures. The client is trimmed to plain HTTP/1.1 + JSON:
  no TLS (`imbhd` serves none, so an `https://` URL is refused rather than silently downgraded), no
  http2, no cookies.

- **TUI: Backspace walks up the current screen's drill-down chain**, a separate axis from the `←`/Esc
  visit history. Each screen owns a fixed series — `Metrics catalog → series list → series detail`,
  `Traces → TraceDetail → SpanDetail`, `Logs → LogDetail`, `Overview` alone — and Backspace moves one
  rung outward. The two axes diverge exactly where they should: a trace detail opened by the log→trace
  jump steps *up* to the Traces list, while `←` still returns to the log it came from. The step pushes
  the departed view, so one `←` undoes it.

  `SpanDetail` is the one rung whose parent is not self-contained — it holds a trace id plus a single
  span, not the trace — so the trace is recovered from the retained detail or from a `TraceDetail`
  still on the back stack, and with neither available the step lands on the Traces list rather than an
  empty rung. The waterfall cursor survives `SpanDetail → TraceDetail` so it lands back on the span
  that was open. Intents scoped to the view being left (a pending trace open, the trace focus, the
  metric exemplars) are dropped, so a late waterfall cannot yank you back into the detail you just
  stepped out of. On the Metrics screen the first rung is the **catalog**, which shares
  `Route::Metrics` with the series list and is told apart by an empty query, so the up-step is a
  query-clearing move rather than a route change; the series list used to look like a first view and
  Backspace did nothing there. All four detail hint bars gained a `bksp …` item (and the Metrics
  series list a `bksp catalog`); the global footer did not, since Backspace is inert on list routes
  and advertising a dead key is worse than not advertising it.

- **TUI: a sticky waterfall on the trace detail.** The selected span's scrolled-off ancestors are
  pinned at the top of the pane, so scrolling into a deep trace no longer strands you on a `db.query`
  with no way to see what it hangs off. `s` toggles it (on by default), and the pinned block is capped
  at a third of the viewport, keeping the innermost ancestors. The geometry is a pure function, so it
  is tested without a terminal; it anchors on the *cursor's* ancestor chain rather than the topmost
  visible row, because the latter is not monotone in the pinned count and provably cycles — which
  would flicker as the user holds Down.

  The pinned rows are de-emphasised over three channels rather than one: an explicit `DarkGray`
  foreground (error rows keep `Red`, since a failing ancestor is still worth seeing), a lighter bar
  glyph, and `DIM`. An attribute-only distinction is a bet on terminal support that does not pay off —
  many terminals draw box-drawing characters from a built-in geometry renderer that honours the cell's
  colour but not the faint attribute, so the text dimmed and the bar did not. The last pinned row
  carries an `UNDERLINED` divider, padded to the full pane width (a rule *row* would have cost a
  viewport line out of a pane whose whole problem is being too short).

- **`gen-demo-db --deep-hops N`** (default 5) emits one deep trace per step: a checkout entry chaining
  that many service hops, nested well past what the shallow fixtures reached, with names longer than
  the name column and roughly one in four failing at the innermost `db.query` so an ERROR propagates
  up the pinned ancestor chain.

### Changed

- **Breaking: `imbh_tui::cli::Mode::Tui` carries `source: Source` instead of `path: PathBuf`,** since
  the explorer now takes `--url` as well as a directory. `imbh_tui::run` takes `impl Into<Backend>`
  and still accepts an `Arc<Db>` (via `From<Arc<Db>>`). `imbh-tui`'s command line also accepts `--url`
  wherever it accepted a directory, including under `--mcp-stdio`.

- **TUI: the waterfall's 20-cell name column is flat.** Two removals. Span names are no longer
  indented by depth — nesting is already carried by the pinned ancestor block (walked from the parent
  link, never from a depth counter) and by the bars themselves, so the indent spent name cells saying
  what the pane already said twice over; at depth 8 a name had four readable cells left. And the
  column no longer scrolls horizontally to chase the cursor row: a name that fits is shown whole, one
  that does not is cut with a trailing ellipsis (`...` under `--ascii`, which stays pure ASCII). The
  pane is therefore stateless left-to-right — it renders the same wherever the cursor is, so pinned
  and scrolling rows lay out identically and nothing shifts under the eye on cursor movement. A
  truncated name's tail is one row away in the span summary. The alignment invariant is untouched:
  everything left of the first `|` still sums to a fixed cell count, so the bars line up across rows.

### Fixed

- **The release workflow published an empty, frozen GitHub Release (CD).** `release.yml` did
  `gh release create` and then `gh release upload`, which assumes a mutable Release; GitHub's
  immutable releases freeze one the moment it is *published*, so every upload failed with
  `HTTP 422: Cannot upload assets to an immutable release` — and the obvious recovery, deleting the
  Release and re-tagging, reserves the tag name permanently (see the `[0.3.0]` note below). The job
  now creates a **draft** (mutable, re-runnable, deletable), uploads every archive into it, and only
  then flips `--draft=false`, with an explicit `--latest` so a prerelease cannot displace the stable
  Release. It branches on an existing Release's state — reuse a draft, refuse a published one — and
  annotates a failed create with what a burned tag name means, each message saying plainly not to
  delete anything, because by the time an operator reads it that is the tempting move. The `meta` job
  gained a matching preflight, so an already-published Release aborts the run in seconds rather than
  after five fat-LTO build legs.

- **TUI: Enter on a log detail now opens the trace detail, not the trace list.** The log→trace jump
  (and the metric exemplar→trace jump, which shares the code path) focused the trace and switched to
  the Traces screen, but the intent to open the trace's waterfall was dropped on the way — the screen
  switch and the waterfall fetch both cleared the existing `pending_trace_open` flag — so the jump
  stopped at the correlated list. The intent now rides along with the trace focus and is consumed
  when the waterfall lands. The Traces list the jump routes through is not recorded in the history,
  so one Enter is still undone by one `←`.

## [0.3.0] - 2026-08-01

> **No GitHub Release or `v0.3.0` tag.** The CD run published the Release before uploading its
> assets, which under GitHub's immutable releases froze it empty and reserved the tag name for good;
> the tag was then deleted in an attempt to retry. Everything else shipped normally — all 12 crates
> are on crates.io at 0.3.0 and `ghcr.io/moriyoshi/imbh:0.3.0` is published — but there are no
> downloadable archives for this version, and the heading below links to the release commit rather
> than to a tag. The release workflow now uploads into a draft and publishes last, so this cannot
> recur.

### Added

- **MCP over stdio, from the `imbh-tui` binary.** `imbh-tui --mcp-stdio <db-dir>` serves the same
  Model Context Protocol on stdin/stdout — the transport MCP clients "SHOULD support whenever
  possible", and the one an agent that spawns its own server speaks. No port is bound, nothing needs
  to be running, and the pipe is the authorization:

  ```sh
  claude mcp add imbh -- imbh-tui --mcp-stdio /var/lib/imbh
  ```

  The directory is opened with `Db::open_read_only`, which takes no writer lock, so a session reads
  alongside a live `imbhd` on the same files. What a read-only opener cannot see is that writer's
  unsealed buffer — for which `imbh-tui --mcp-stdio --url 127.0.0.1:4318` forwards each message to
  the daemon's `POST /mcp` instead, synthesizing the header mirror the stateless revision requires
  from the message it is forwarding. The forwarding client is hand-written HTTP/1.1 over
  `std::net::TcpStream`, so it pulls **no HTTP client dependency** into the TUI binary.

  New crate **`imbh-mcp`**, holding the protocol dispatch, the 15 read-only tools, and the stdio
  transport. The module moved there out of `imbh-server` because both transports need it and
  `imbh-tui` may not depend on `imbh-server` (`imbh ← imbh-mcp ← {imbh-server, imbh-tui}`). The
  dispatch is now explicitly transport-aware: `handle(db, bytes, &Transport)`, where the stateless
  revision's `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` header agreement is enforced for
  `Transport::Http` and skipped for `Transport::Stdio`, which has no header channel to agree with.
  No footprint change — `imbh-mcp` is `imbh` plus `serde_json` and `base64`, both already compiled
  under DataFusion (the facade stays at 275 crates).

  `imbh_server::mcp` still resolves (it re-exports the crate), as do
  `imbh_server::{batches_to_json, stats_json, offload}`. `imbh-tui`'s command line also gained
  `--db` (an explicit spelling of the positional directory) and `--help`.

- **`imbhd` serves the Model Context Protocol at `POST /mcp`**, so an agent can search logs, pull
  traces, and query metrics through the same process that ingests them — no Grafana, no datasource
  proxy, no export step. Point a client at `http://127.0.0.1:4318/mcp` (e.g.
  `claude mcp add --transport http imbh http://127.0.0.1:4318/mcp`).

  The 15 tools are **read-only** — `search_logs`, `count_logs`, `log_volume`, `search_traces`,
  `get_trace`, `span_metrics`, `list_metrics`, `metric_series`, `query_metric_range`,
  `query_metric_instant`, `histogram_quantile`, `list_attribute_keys`, `list_attribute_values`,
  `db_stats`, and `query_sql`. Nothing there can ingest, flush, compact, or apply retention.

  Both protocol eras are served: the stateless `2026-07-28` revision (per-request `_meta`,
  `server/discover`, validated `MCP-Protocol-Version`/`Mcp-Method`/`Mcp-Name` header mirror) and the
  `initialize` handshake of `2025-11-25` and earlier. Nothing streams, so responses are single JSON
  bodies and no session id is minted; `GET`/`DELETE /mcp` answer `405`.

  On in the default build, and it adds **no crate** to any dependency graph: it speaks JSON-RPC
  through `serde_json` and Base64 through `base64`, both of which are already compiled under
  DataFusion (via `arrow-json` and `arrow-cast`), so the new direct edges cost nothing — measured
  275 → 275 crates on the `imbh` facade and 293 → 293 on `imbh-server`. Like the rest of `imbhd` the endpoint is
  unauthenticated, but it enforces the transport's DNS-rebinding defence: a browser `Origin` outside
  loopback is refused `403`, widened by the new `IMBH_MCP_ALLOWED_ORIGINS` (comma-separated, or `*`).
  Public API additions: `imbh_server::mcp` and `imbh_server::mcp_allowed_origins`. See
  [`docs/MCP.md`](./docs/MCP.md).

### Changed

- **`imbh-server` now serves HTTP on axum/hyper** instead of its own `std::net`, thread-per-connection
  server. This covers **both** listeners — the TCP server and the Docker logging-driver plugin's Unix
  socket — which share one request path, so body limits, phase deadlines, and `Content-Encoding`
  decoding are identical on both. The crate's hand-rolled HTTP/1.1 parser is gone.

  `imbh-server` is optional and sits *downstream* of the library, so the footprint budget is
  untouched: it is measured on `cargo tree -p imbh`, which stays at **275 crates**. The cost is ~17
  crates in `imbh-server`'s own graph and ~1.4 MiB of `imbhd` binary (31.2 → 32.6 MiB, budget 42 MB).
  `--features grpc` got *cheaper* — tonic 0.14 routes through axum, so hyper/tower/axum used to arrive
  with it; the full-feature graph is unchanged at 310 crates.

  Behaviour that changed, all of it visible to clients:

  - **Keep-alive.** Responses no longer carry `Connection: close`, so an exporter pushing a batch a
    second stops paying a TCP handshake per batch.
  - **A known path with the wrong method is `405`,** not `404`. Unknown paths are still `404`.
  - A header-phase timeout is still answered `408 Request Timeout`; hyper reports that deadline without
    answering it, so the accept loop writes the 408 itself.
  - **`LogDriver.ReadLogs` responses are now `Transfer-Encoding: chunked`** (`docker` feature). The
    plugin used to write frames raw and close the socket. Docker reads this body through Go's
    `net/http`, which un-chunks transparently, so `docker logs` and `docker logs -f` are unaffected; a
    hand-written client that read the old raw stream needs to decode chunked framing. A `docker logs -f`
    whose client stops reading is now abandoned after a bounded stall instead of held open.
  - `IMBH_MAX_CONNECTIONS` defaults to `512`, under the usual 1024 soft `RLIMIT_NOFILE`, so parquet
    and tantivy keep their share of descriptors.

  New public API, additive: `app(db) -> axum::Router` (mount imbh's endpoints in an existing axum
  application), `Limits`, `serve_with_limits_until`, `offload`, `max_body` / `max_connections`, and
  `DEFAULT_MAX_BODY` / `DEFAULT_MAX_CONNECTIONS`. `serve`, `serve_until`, `serve_with_until`, `route`,
  `IoTimeouts`, and the `Shutdown` token keep their signatures.

### Fixed

- **A chunked request body was silently read as empty (`imbh-server`).** The old parser keyed entirely
  off `Content-Length`, so a `Transfer-Encoding: chunked` upload — what Go's `http.Client` sends
  whenever the body is not a sized reader — was read as zero bytes and answered
  `200 {"accepted":0}`: a success status for dropped telemetry. hyper undoes the framing, so the body
  now arrives intact.
- **An unbounded allocation from a forged `Content-Length` (`imbh-server`).** The old parser did
  `vec![0u8; content_length]` straight from the header, before reading a byte, so
  `Content-Length: 10737418240` with no body behind it was a 10 GiB allocation. Bodies are now capped by
  `IMBH_MAX_BODY` (new; default `64MiB`) and an oversized declared length is refused with
  `413 Payload Too Large` without reading the body.
- **Connections were unbounded (`imbh-server`).** The accept loop spawned an OS thread per connection
  with no cap, on both the TCP listener and the plugin socket. Connections are tasks now, bounded by
  `IMBH_MAX_CONNECTIONS` (new; default `512`).

### Added

- **gzip request bodies (`imbh-server`).** `Content-Encoding: gzip` is accepted on every route. The
  OpenTelemetry Collector's `otlphttp` exporter sets `compression: gzip` by default, so a stock
  collector pointed at `imbhd` used to fail every export and had to be reconfigured with
  `compression: none`. The cap in `IMBH_MAX_BODY` is applied to the *inflated* size, so a compression
  bomb is refused on what it expands to rather than on its size on the wire. No new crate: `flate2` was
  already in the graph via parquet.
- **Per-connection deadlines for `imbhd` (`imbh-server`).** A client that connected and said nothing
  held a connection (and a `Db` handle) indefinitely. Two phase deadlines now bound it, with
  deliberately different rules: `IMBH_HEADER_TIMEOUT` (new; default `10s`) caps the request line +
  headers **in total**, and `IMBH_BODY_TIMEOUT` (new; default `30s`) is a **per-read** allowance for the
  body. So a large OTLP body over a slow link still succeeds — the rule is "do not stall", not "do not
  take a while" — while an idle, trickling, or stalled client is answered `408 Request Timeout` and
  disconnected, having ingested nothing. `0` disables either phase.

  New public API, additive: `IoTimeouts` (with `DISABLED`), `io_timeouts`, `DEFAULT_HEADER_TIMEOUT` /
  `DEFAULT_BODY_TIMEOUT`, and `serve_with_until` (`serve` / `serve_until` use `IoTimeouts::default()`).
  The Docker plugin endpoint applies the defaults to its own socket, which also means a `docker logs -f`
  client that vanishes without closing no longer holds its stream open.

- **Signal handling and graceful shutdown for `imbhd` (`imbh-server`).** `SIGINT`/`SIGTERM` (Ctrl-C,
  `docker stop`, systemd, `kill`) now wind the process down instead of killing it: every listener stops
  accepting, in-flight requests get `IMBH_SHUTDOWN_TIMEOUT` (new; default `5s`, `0` to not wait) to
  finish, the Docker plugin's container readers stop and its ingest queue is drained into the DB, and
  `Db::close()` seals the buffer — so `imbhd` exits 0 and the next start replays nothing instead of
  recovering everything since the last seal from the WAL. A **second** signal exits immediately with
  `128 + signum`.

  New public API on the crate, all additive: `imbh_server::Shutdown` (the token — `trigger`, `wait`,
  `is_triggered`, `on_trigger`, `install_signal_handlers`, `drain_timeout`), `serve_until`,
  `docker::serve_plugin_until` / `serve_plugin_with_until`, `grpc::serve_grpc_until` /
  `serve_grpc_blocking_until`, `shutdown_timeout`, and `docker::ingest::Ingestor::shutdown`. The
  existing `serve` / `serve_plugin` / `serve_grpc*` entry points keep their signatures and their
  "serve until the process exits" contract, so a host that drives its own lifecycle can adopt the token
  at its own pace.

  Notes on the implementation: `accept` is **woken**, not polled — each listener registers a waker on
  the token and turns it into a `oneshot` its accept loop selects on, so an idle server costs nothing
  and shutdown is observed immediately. The signal handler does only async-signal-safe work (an atomic
  store plus one byte down a self-pipe); a watcher thread does the rest. Signal handling is Unix-only
  and adds **no crate** to the footprint graph: `libc` (std cannot catch `SIGTERM`) is already there
  via DataFusion, so the gate stays at 275 crates.

## [0.2.0] - 2026-07-30

### Added

- **Prebuilt binaries and a container image on every release (CD).** `imbhd` and `imbh-tui` no longer
  have to be built from source. `.github/workflows/release.yml` now builds both in the release profile
  for five targets — `x86_64`/`aarch64-unknown-linux-gnu` (glibc 2.35 floor, built natively on
  22.04 runners), `aarch64`/`x86_64-apple-darwin`, and `x86_64-pc-windows-msvc` — with the
  `grpc,tracing` feature set (plus `docker` on Linux), smoke-tests each artifact on the runner that
  produced it, and attaches one archive per platform plus a `SHA256SUMS` to the GitHub Release for the
  tag. Each archive carries `LICENSE` and `THIRD-PARTY-NOTICES.txt`. A multi-arch
  (amd64 + arm64) image containing both binaries is published to `ghcr.io/moriyoshi/imbh` as
  `X.Y.Z`, `X.Y`, and `latest`; it copies in the already-built binaries rather than compiling, so the
  arm64 leg costs no emulated fat-LTO build. `workflow_dispatch` runs the whole path as a rehearsal
  that publishes nothing. See README.md "Install the binaries".

- **`docker/Dockerfile` + `scripts/build-image.sh`** for that image, so it is reproducible locally and
  not only in CI: run the script bare and it compiles both binaries for the host architecture with the
  release feature set and builds a single-arch image. The Dockerfile's header states the build-context
  contract that both it and the release workflow satisfy. Distinct from
  `crates/imbh-server/docker-plugin/`, which builds the logging *plugin* rootfs.

- **A flush scheduler with selectable strategies (`FlushPolicy`).** `Maintenance` already chose *who*
  runs the background loop; the new `DbBuilder::flush(FlushPolicy)` chooses *when* it seals the buffer.
  The triggers OR together and are each optional: periodic (`FlushPolicy::periodic(d)`), buffered heap
  (`.at_buffer_bytes(n)`, defaulting to the memory-budget-derived threshold), buffered rows
  (`.at_buffer_rows(n)`), on-disk WAL size (`.at_wal_bytes(n)` — sealing is what lets the WAL be
  reclaimed), and idle (`.after_idle(d)`); `.tick(d)` sets the evaluation cadence and
  `FlushPolicy::manual()` disables automatic sealing entirely. A policy also parses from a spec string
  (`"interval=5s,wal=64MiB"`, or `"manual"`) via `FromStr`. Leaving it unset preserves the previous
  behavior exactly: seal on the `Maintenance` interval and at the byte threshold. See ARCHITECTURE.md
  §5/§10.2.

- **`imbhd` now flushes on its own**, configured by `IMBH_FLUSH` (default `interval=5s`) and
  `IMBH_MAINTENANCE_INTERVAL` (default `60s`, the retention cadence). Previously the reference server
  opened the DB with the library default `Maintenance::Manual`, so **nothing ever sealed** unless an
  operator POSTed `/admin/flush`: rows stayed in the mutable buffer, the WAL was never reclaimed, and
  neither RSS nor disk use was bounded. Both variables are `settable` on the Docker log-driver plugin.
  A malformed spec fails startup rather than silently running a different cadence.

- **`WalMode::Interval(d)` is honored by that scheduler.** It previously fsynced only opportunistically
  on `flush`/`close` (no timer existed), so the default interval mode never delivered its 1s
  durability window on an otherwise idle writer. New `Storage::sync_wal` / `Storage::wal_sync_interval`
  back it; `Storage::flush_gauges` (buffered bytes/rows + idle clock) and `Storage::seal_threshold_bytes`
  expose what the policy's triggers compare against.

- **`imbh-tui`: a full-content trace detail screen, and a per-span drill-down.** The Traces screen
  drew a selected trace's waterfall into a fixed 45% slice of the results area with no scroll offset,
  so any trace deeper than that pane was partly unreachable. Enter on the trace list now opens
  `Route::TraceDetail` — the whole waterfall as a scrolling list with a span cursor, a header
  (trace id, span count, duration, start), and a summary of the cursored span when the area is tall
  enough — and Enter on a waterfall row opens `Route::SpanDetail` with that span's full fields
  (ids/parent, service, kind, status, offset into the trace, the three attribute maps, raw
  events/links). `L` from either correlates Logs by trace id *and* span id, closing the per-span
  drill-down gap. Both follow the existing non-modal detail pattern and cost no extra query: the
  list already materializes the selected trace to draw its preview. The preview pane itself still
  does not scroll, but now reports "Waterfall: N of M spans" instead of silently truncating.

- **`imbh-server`: a Docker logging-driver plugin**, behind the new optional, off-by-default
  `docker` feature (Unix only). `imbhd --features docker` serves the `docker.logdriver/1.0` plugin
  API on a Unix socket, so `docker run --log-driver imbh` writes a container's stdout/stderr
  straight into the embedded database — queryable with SQL, `matches()` full-text search, and the
  typed logs API — while `docker logs` (history, `--tail`, `--since`/`--until`, `-f`) is served back
  out of stored rows. Container identity becomes OTel resource attributes (`container.id`,
  `container.name`, `container.image.*`, `container.runtime`, plus `--log-opt labels=`/`env=`
  selections); stdout/stderr map to configurable severities and the `log.iostream` attribute; lines
  Docker splits are reassembled into one record. The endpoint is inert unless
  `IMBH_DOCKER_PLUGIN_SOCKET` names a socket. Adds **no crate** to the dependency graph. Packaging
  lives in `crates/imbh-server/docker-plugin/`; see
  [docs/DOCKER_LOG_DRIVER.md](./docs/DOCKER_LOG_DRIVER.md) and ARCHITECTURE.md §10.16.

- **`imbhd` listen addresses are configurable by environment**, and individually disableable.
  `IMBH_LISTEN_ADDR` and `IMBH_GRPC_LISTEN_ADDR` back the existing positional arguments (argument >
  environment > default); an **empty** value opens no socket for that transport. This is what lets a
  managed Docker plugin retune its endpoints with `docker plugin set` -- a plugin's entrypoint
  arguments are frozen in its `config.json` -- and what lets an operator run the log driver with no
  network port at all. `main` now runs every configured endpoint on its own thread and parks on all
  of them, so HTTP, gRPC, and the plugin socket are independently optional.

### Fixed

- **`THIRD-PARTY-NOTICES.txt` did not cover the binaries actually distributed.** It was generated for
  `imbh-server` with *default* features, so it attributed none of the tonic/hyper/h2/tower subtree
  that the `grpc` feature links, nothing from `tracing-subscriber`, and nothing of `imbh-tui`'s
  ratatui/crossterm/rand subtree -- while README.md "License" promises those notices ship with every
  binary distribution (Apache-2.0 §4(d)). `scripts/gen-notices.sh` now generates across the whole
  workspace with all features for every published target (267 Apache-2.0 / 94 MIT crates, up from
  210 / 59), and the file ships inside every release archive and in the image at
  `/usr/share/doc/imbh/`.

- **The license gate only ever vetted the host target with default features.** `deny.toml`'s `[graph]`
  now sets `all-features = true` and lists all six shipping targets, so the `grpc`/`docker` subtrees
  and target-specific dependencies (`windows-sys`, `core-foundation`, ...) are covered. This found no
  violations, but the previous configuration could not have found any.

- **Docker log driver: `docker logs -f` dropped the first line.** When the history query came back
  empty, follow mode set its watermark to the wall clock and then asked only for records newer than
  that instant -- but a record's timestamp is when the *container emitted* the line, while ingest
  lands it up to one batch interval later, so lines already emitted and not yet stored were skipped
  permanently. `docker logs -f` on a freshly started container hit this every time. The watermark now
  stays at the request's lower bound until something has actually been written (`--tail 0` still
  jumps to the present, which is that flag's defined semantic). Found by running the plugin against a
  real `dockerd`.

- **The plugin rootfs image could not be built in a working checkout.** Its Dockerfile builds from
  the repository root, so `COPY . .` pulled in `target/` and `.agents-workspace/` -- hundreds of
  gigabytes of build artifacts. Docker transfers the whole context before the first instruction, so
  the build appeared to hang rather than fail. Added a root `.dockerignore`; the context drops from
  614 GB to 4.8 MB.

## [0.1.1] - 2026-07-28

### Changed

- **Every GitHub Actions `uses:` is pinned to a 40-hex commit SHA** (with a trailing `# vX.Y.Z`
  comment) across `ci.yml`, `release.yml`, and `soak.yml`, so a moving tag can no longer change what
  CI — and therefore the release path — executes. The actions were upgraded to their current
  releases at the same time (`actions/checkout` v4 -> v7.0.1, `actions/upload-artifact` v4 -> v7.0.1,
  `Swatinem/rust-cache` v2 -> v2.9.1, `taiki-e/install-action` v2 -> v2.85.0); `dtolnay/rust-toolchain`
  has no usable version tag, so it is pinned to the `stable` branch head with an explicit
  `toolchain: stable` on every step — that freezes the action, not the toolchain.

### Fixed

- **Windows: every on-disk `Db::open` failed** with `storage error: WAL dir fsync: Access is denied.
  (os error 5)`. The durability path fsync'd a directory by opening it as a `File` — a POSIX idiom
  Windows rejects — so no on-disk database could be opened at all on that platform (in-memory was
  unaffected). Both call sites are now compiled out on Windows: the WAL segment create/rotate
  (`imbh-storage`'s `wal.rs`) and the seal/manifest rename (`imbh-storage`'s `lib.rs`), matching what
  SQLite, LMDB, and RocksDB do. File-content durability is unchanged; see ARCHITECTURE.md §7
  "Directory fsync (platform note)" for what is assumed rather than enforced on NTFS. A
  `windows-latest` CI job now guards the on-disk path. ([#3](https://github.com/moriyoshi/imbh/issues/3))

## [0.1.0] - 2026-07-24

### Added

- Initial public workspace: the `imbh` facade plus `imbh-core`, `imbh-otlp`, `imbh-storage`,
  `imbh-index`, `imbh-query`, `imbh-proto`, `imbh-server` (the `imbhd` reference server),
  `imbh-tracing`, `imbh-otel-exporter`, `imbh-lgtm`, and `imbh-tui`. Milestones M0–M6 complete.
  All 12 crates published to crates.io (`imbh-test-support` is dev-only and stays unpublished).

<!-- next-url -->
[0.6.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.6.0
[0.5.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.5.0
[0.4.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.4.0
[0.3.0]: https://github.com/moriyoshi/imbh/commit/07b72dd7e05f2320afcf573e0ff4e4766b9f0ec0
[0.2.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.2.0
[0.1.1]: https://github.com/moriyoshi/imbh/releases/tag/v0.1.1
[0.1.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.1.0
