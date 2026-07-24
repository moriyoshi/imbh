use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, Stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use imbh::{
    AnyValue, Attributes, Db, PageCursor, SeverityNumber, SpanId, Table as DbTable, Timestamp,
    TraceId,
};
use imbh_lgtm::{
    EvalLimits, EvalRange, FetchBounds, ImbhQueryModel, LogFetchRequest, LogFilter,
    LogStreamSchema, LogsSemanticsExt, MetricKind, MetricResolution, MetricsSemanticsExt,
    SemanticError, SpansetExpr, TraceQueryMatch, TracesSemanticsExt, TranslateContext,
    build_log_query, translate_logql, translate_promql, translate_traceql,
};
use rand::rngs::SmallRng;
use rand::{Rng as _, SeedableRng};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph,
    Row, Sparkline, Table, TableState, Wrap,
};
use tokio::sync::mpsc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Selectable relative time ranges: `(label, lookback, step)`. The step is paired with each range to
/// keep the sample count bounded regardless of how wide the window is.
const TIME_RANGES: &[(&str, Duration, Duration)] = &[
    ("5m", Duration::from_secs(5 * 60), Duration::from_secs(5)),
    ("15m", Duration::from_secs(15 * 60), Duration::from_secs(30)),
    ("1h", Duration::from_secs(60 * 60), Duration::from_secs(30)),
    (
        "3h",
        Duration::from_secs(3 * 60 * 60),
        Duration::from_secs(120),
    ),
    (
        "6h",
        Duration::from_secs(6 * 60 * 60),
        Duration::from_secs(300),
    ),
    (
        "12h",
        Duration::from_secs(12 * 60 * 60),
        Duration::from_secs(600),
    ),
    (
        "24h",
        Duration::from_secs(24 * 60 * 60),
        Duration::from_secs(900),
    ),
    (
        "7d",
        Duration::from_secs(7 * 24 * 60 * 60),
        Duration::from_secs(3600),
    ),
];

/// The animated mascot, "Atta": block-glyph art (a little bobbing creature), transcribed verbatim from
/// `mascot.txt`. The four frames are **two facings, two waddle phases each**: indices `0..2` face and
/// step **right** (`mascot.txt` frames `1-a`/`1-b`), indices `2..4` face and step **left** (`2-a`/`2-b`).
/// Within a facing the two phases alternate to waddle; the head/feet whitespace differs so it bobs.
/// [`mascot_art`] selects the pair for a facing, and [`draw_mascot`] blits the chosen frame at the
/// [`Mascot`]'s current position.
const MASCOT_FRAMES: [[&str; 3]; 4] = [
    ["▄████▄", "███▄█▄██", "▐▛▛▛▛▛▛▌"],
    ["▄████▄", "███▄█▄██", "▐▜▜▜▜▜▜▎"],
    ["  ▄████▄", "██▄█▄███", "▐▜▜▜▜▜▜▛"],
    ["  ▄████▄", "██▄█▄███", " ▛▛▛▛▛▛▌"],
];

/// Mascot waddle cadence: advance one waddle phase every this many nanoseconds. Paired with the faster
/// `Normal`-mode wake tick in [`run`] so the animation is redrawn smoothly.
const MASCOT_FRAME_NS: i64 = 1_000_000_000;

/// Rows of art per frame (all frames are 3 rows tall).
const MASCOT_ART_HEIGHT: u16 = 3;
/// Nominal art width (the wider frames are 8 cells); used for horizontal centring/bounds so motions do
/// not have to know the exact per-frame width. [`draw_mascot`] uses the real width when it blits.
const MASCOT_ART_WIDTH: u16 = 8;
/// Rows kept clear at the bottom for the status/hint bar (the idle mascot rests just above it).
const MASCOT_BOTTOM_MARGIN: u16 = 2;
/// How long without a keystroke before the user is considered idle (the wander then starts).
const MASCOT_IDLE_AFTER: Duration = Duration::from_secs(3);

/// The art frame for a facing (`+1` = right, `-1`/`0` = left) and a waddle `phase` (`0`/`1`). Right uses
/// `MASCOT_FRAMES[0..2]`, left uses `[2..4]` — see the [`MASCOT_FRAMES`] doc.
fn mascot_art(facing: i8, phase: usize) -> &'static [&'static str; 3] {
    let base = if facing < 0 { 2 } else { 0 };
    &MASCOT_FRAMES[base + (phase & 1)]
}

/// The waddle phase (`0`/`1`) for an accumulated `phase_ns`, flipping every [`MASCOT_FRAME_NS`].
/// `rem_euclid` keeps it in range for negative accumulators.
fn mascot_phase(phase_ns: i64) -> usize {
    (phase_ns.rem_euclid(MASCOT_FRAME_NS * 2) / MASCOT_FRAME_NS) as usize
}

// ---------------------------------------------------------------------------------------------------
// Mascot motion foundation
//
// The mascot is event-driven and its motions are pluggable. The run loop turns state changes into
// `MascotEvent`s (navigation, idle/active, data refresh) and feeds them, plus a per-frame tick, to a
// `Mascot` controller. Two behaviours ride on this foundation today:
//   * `IdleWander` — the default: intermittent left/right strolling, but ONLY while the user is idle.
//   * `ChartRide`  — an easter egg: on a random, temporary basis, when a metric chart shows a large
//     drop, the mascot jumps onto the high point and slides down the datapoints, then hands back.
// A new motion plugs in by implementing `MascotMotion` (+ a `MascotIgniter` that decides when to start
// it) and pushing the igniter into `Mascot::new`; nothing else changes.
// ---------------------------------------------------------------------------------------------------

/// Discrete signals the mascot's motions react to. Emitted by [`run`] from frame-to-frame state deltas.
#[derive(Debug, Clone, Copy)]
enum MascotEvent {
    /// The current [`Route`] changed; `on_metric_chart` is whether it is now the metric diagram.
    Navigated { on_metric_chart: bool },
    /// The user has gone quiet (no input for a while) — the idle wander may run.
    Idle,
    /// The user is driving the UI again — the mascot stands still.
    Active,
    /// A data query landed; `auto` distinguishes a background auto-refresh from a manual one.
    Refreshed { auto: bool },
}

/// Whether the user is actively driving the UI or has gone quiet. The idle wander advances only while
/// [`Activity::Idle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activity {
    Active,
    Idle,
}

/// The mascot's position (screen-cell space, floats for smooth sub-cell motion), the direction it faces
/// (which art pair to show), and an accumulator that drives the waddle phase.
#[derive(Debug, Clone, Copy)]
struct MascotBody {
    x: f32,
    y: f32,
    /// `+1` faces/steps right, `-1` left.
    facing: i8,
    phase_ns: i64,
}

/// The rendered geometry of the metric time-series chart, published by [`draw_metric_detail`] through
/// [`App::chart_geom`] so [`ChartRide`] can walk the *actual* on-screen datapoints. `cells` holds the
/// terminal cell of each finite datapoint, left-to-right; `graph` is the plotting rectangle.
#[derive(Debug, Clone)]
struct ChartGeometry {
    graph: Rect,
    cells: Vec<(u16, u16)>,
}

/// Read-only context handed to motions/igniters each tick: the area to stay within and the metric
/// chart's geometry when one is on screen.
struct MascotCtx {
    area: Rect,
    chart: Option<ChartGeometry>,
}

/// A transient, pluggable mascot motion. [`tick`](MascotMotion::tick) advances the body by `dt_ns`;
/// returning `false` means the motion is finished and control returns to the idle behaviour.
trait MascotMotion {
    fn tick(&mut self, dt_ns: i64, body: &mut MascotBody, ctx: &MascotCtx) -> bool;
}

/// Decides when to start a motion — the pluggable ignition seam. [`arm`](MascotIgniter::arm) records
/// interest from an event; [`poll`](MascotIgniter::poll) runs every tick and may fire (returning the
/// motion to run). Splitting the two lets a motion that depends on render geometry (like the chart
/// ride) arm on an event yet wait until the geometry it needs actually appears.
trait MascotIgniter {
    fn arm(&mut self, ev: &MascotEvent);
    fn poll(
        &mut self,
        dt_ns: i64,
        ctx: &MascotCtx,
        rng: &mut SmallRng,
    ) -> Option<Box<dyn MascotMotion>>;
}

/// The idle wander: strolls to a random x, dwells, then picks another — but only advances while the
/// user is idle (the controller withholds ticks otherwise), so the mascot rests during active use.
struct IdleWander {
    /// Where it is currently heading (`None` between strolls, while dwelling).
    target_x: Option<f32>,
    /// Remaining dwell before the next stroll.
    dwell_ns: i64,
}

impl IdleWander {
    /// Cells per second while strolling.
    const SPEED: f32 = 12.0;

    fn new() -> Self {
        Self {
            target_x: None,
            dwell_ns: 0,
        }
    }

    /// Advance the stroll. Called by the controller only while [`Activity::Idle`].
    fn tick(&mut self, dt_ns: i64, body: &mut MascotBody, ctx: &MascotCtx, rng: &mut SmallRng) {
        // Rest in the band just above the status/hint bar.
        let band_y = ctx
            .area
            .bottom()
            .saturating_sub(MASCOT_ART_HEIGHT + MASCOT_BOTTOM_MARGIN) as f32;
        body.y = band_y;
        let min_x = ctx.area.left() as f32;
        let max_x = ctx
            .area
            .right()
            .saturating_sub(MASCOT_ART_WIDTH)
            .max(ctx.area.left()) as f32;

        if self.dwell_ns > 0 {
            self.dwell_ns -= dt_ns;
            return;
        }
        let target = match self.target_x {
            Some(t) => t,
            None => {
                // Pick a fresh destination and face toward it.
                let t = if max_x > min_x {
                    rng.random_range(min_x..=max_x)
                } else {
                    min_x
                };
                self.target_x = Some(t);
                body.facing = if t >= body.x { 1 } else { -1 };
                t
            }
        };
        let step = Self::SPEED * (dt_ns as f32 / 1e9);
        if (target - body.x).abs() <= step {
            // Arrived: settle, then dwell a random, "intermittent" beat before the next stroll.
            body.x = target;
            self.target_x = None;
            self.dwell_ns = rng.random_range(400_000_000..=2_200_000_000);
        } else {
            let dir = (target - body.x).signum();
            body.x += dir * step;
            body.facing = if dir >= 0.0 { 1 } else { -1 };
        }
        body.x = body.x.clamp(min_x, max_x);
    }
}

/// Interpolate a sparse polyline of datapoint cells into a dense one-cell-per-step polyline (the longer
/// of |dx|/|dy| sets the step count per segment), so a slide along it glides smoothly no matter how far
/// apart the datapoints are — the difference between a visible descent and a one-frame snap.
fn densify(cells: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for w in cells.windows(2) {
        let (x0, y0) = (w[0].0 as i32, w[0].1 as i32);
        let (x1, y1) = (w[1].0 as i32, w[1].1 as i32);
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for s in 0..steps {
            let x = x0 + (x1 - x0) * s / steps;
            let y = y0 + (y1 - y0) * s / steps;
            out.push((x as u16, y as u16));
        }
    }
    if let Some(&last) = cells.last() {
        out.push(last);
    }
    out
}

/// The chart-ride easter egg: hop onto the peak datapoint, then slide down the descending line. The
/// descent may fall off to the left or the right — the path is ordered peak → trough either way.
struct ChartRide {
    /// Dense body-space positions from the peak down to the trough, one cell apart, so the slide is a
    /// smooth, slow, clearly-visible glide.
    steps: Vec<(f32, f32)>,
    /// Set on the first tick, capturing where the hop launches from.
    launch: Option<(f32, f32)>,
    /// Elapsed time in the hop phase.
    hop_t_ns: i64,
    /// Whether the hop is done and the slide is underway.
    sliding: bool,
    /// Fractional index along `steps` while sliding.
    pos: f32,
    /// Current slide speed in cells/sec, integrated from the local gradient (gravity + friction).
    speed: f32,
    /// The overall horizontal direction of the descent (`+1` right, `-1` left); the mascot holds this
    /// facing for the whole slide so it stays oriented downhill instead of flickering on vertical steps.
    slide_facing: i8,
}

impl ChartRide {
    /// Hop duration onto the peak (kept short so the downhill slide is the star of the show).
    const HOP_NS: i64 = 320_000_000;
    /// Peak height of the hop arc, in rows.
    const ARC_ROWS: f32 = 2.0;
    /// Along-slope gravity (cells/sec²): steeper pitches accelerate the slide harder.
    const SLIDE_GRAVITY: f32 = 54.0;
    /// Drag (1/sec): with gravity this gives a terminal speed of `GRAVITY·sin(θ)/FRICTION`, so the
    /// steady speed tracks the local steepness — the whole point of being faithful to the hill.
    const SLIDE_FRICTION: f32 = 3.0;
    /// Speed bounds (cells/sec): never fully stall, never blur past the datapoints.
    const SLIDE_MIN_SPEED: f32 = 2.0;
    const SLIDE_MAX_SPEED: f32 = 22.0;
    /// A gentle push-off so the glide starts moving rather than creeping from rest.
    const SLIDE_START_SPEED: f32 = 5.0;
    /// Half-window (in dense cells) for the smoothed local gradient, so speed follows the terrain's
    /// disposition rather than the ±1-cell quantisation of the densified path.
    const SLOPE_WINDOW: usize = 3;

    fn new(path: Vec<(u16, u16)>) -> Self {
        let steps: Vec<(f32, f32)> = densify(&path).into_iter().map(Self::body_at).collect();
        // Fix the slide's facing from the net peak → trough direction (right unless it clearly heads left).
        let slide_facing = match (steps.first(), steps.last()) {
            (Some(first), Some(last)) if last.0 < first.0 => -1,
            _ => 1,
        };
        Self {
            steps,
            launch: None,
            hop_t_ns: 0,
            sliding: false,
            pos: 0.0,
            speed: Self::SLIDE_START_SPEED,
            slide_facing,
        }
    }

    /// Smoothed local steepness `sin(θ)` at dense index `i`, over a `±SLOPE_WINDOW` window: `0` flat,
    /// `1` vertical. Drives how fast the slide accelerates here.
    fn local_steepness(&self, i: usize) -> f32 {
        let lo = i.saturating_sub(Self::SLOPE_WINDOW);
        let hi = (i + Self::SLOPE_WINDOW).min(self.steps.len() - 1);
        let (a, b) = (self.steps[lo], self.steps[hi]);
        let dx = (b.0 - a.0).abs();
        let dy = (b.1 - a.1).max(0.0); // downhill only (rows increase downward)
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        (dy / len).clamp(0.0, 1.0)
    }

    /// Body position that places the mascot's base on datapoint cell `c` (art centred over the column).
    fn body_at(c: (u16, u16)) -> (f32, f32) {
        let x = c.0 as f32 - (MASCOT_ART_WIDTH as f32) / 2.0;
        let y = (c.1 as i32 - (MASCOT_ART_HEIGHT as i32 - 1)) as f32;
        (x, y)
    }
}

impl MascotMotion for ChartRide {
    fn tick(&mut self, dt_ns: i64, body: &mut MascotBody, _ctx: &MascotCtx) -> bool {
        if self.steps.len() < 2 {
            return false;
        }
        let launch = *self.launch.get_or_insert((body.x, body.y));
        let peak = self.steps[0];
        if !self.sliding {
            // Hop: lerp horizontally onto the peak with a parabolic arc.
            self.hop_t_ns += dt_ns;
            let u = (self.hop_t_ns as f32 / Self::HOP_NS as f32).clamp(0.0, 1.0);
            body.x = launch.0 + (peak.0 - launch.0) * u;
            let base_y = launch.1 + (peak.1 - launch.1) * u;
            body.y = base_y - Self::ARC_ROWS * 4.0 * (u - u * u);
            body.facing = if peak.0 >= launch.0 { 1 } else { -1 };
            if u >= 1.0 {
                self.sliding = true;
                self.pos = 0.0;
                body.facing = self.slide_facing; // face downhill the instant the slide begins
            }
            return true;
        }
        // Slide: integrate speed from the local gradient (gravity pulls along the slope, drag opposes),
        // so the mascot accelerates down steep pitches and eases over gentle ones — faithful to the hill.
        let dt_s = dt_ns as f32 / 1e9;
        let here = (self.pos.floor() as usize).min(self.steps.len() - 1);
        let steep = self.local_steepness(here);
        self.speed += (Self::SLIDE_GRAVITY * steep - Self::SLIDE_FRICTION * self.speed) * dt_s;
        self.speed = self
            .speed
            .clamp(Self::SLIDE_MIN_SPEED, Self::SLIDE_MAX_SPEED);
        self.pos += self.speed * dt_s;

        let i = self.pos.floor() as usize;
        if i + 1 >= self.steps.len() {
            let end = *self.steps.last().unwrap();
            body.x = end.0;
            body.y = end.1;
            return false; // reached the trough
        }
        let (a, b) = (self.steps[i], self.steps[i + 1]);
        let f = self.pos - i as f32; // within [0, 1)
        body.x = a.0 + (b.0 - a.0) * f;
        body.y = a.1 + (b.1 - a.1) * f;
        // Hold the descent's overall orientation for the whole slide (steady, no per-step flicker).
        body.facing = self.slide_facing;
        true
    }
}

/// A rideable descending run in a chart, as datapoint cells ordered from its peak (a local high point)
/// down to its trough — or `None` when no drop is "great" enough to ride. A peak is a local row-minimum,
/// so a descent can fall off to the **right** (cells in increasing x) or to the **left** (decreasing x);
/// both directions are searched and the path is ordered peak → trough accordingly. A run qualifies when
/// the total fall is at least `max(3, graph.height/3)` rows (rows increase downward on screen). Among the
/// qualifying runs the **gentlest** is chosen (smallest fall-per-column) — a near-vertical cliff reads as
/// falling, not sliding, so the mascot prefers a hill it can actually glide down.
fn great_displacement_path(chart: &ChartGeometry) -> Option<Vec<(u16, u16)>> {
    let cells = &chart.cells;
    if cells.len() < 2 {
        return None;
    }
    let threshold = (chart.graph.height / 3).max(3);
    // (peak index, trough index, slope) of the gentlest qualifying descent, where slope = fall/columns.
    let mut best: Option<(usize, usize, f32)> = None;
    let mut consider = |peak: usize, trough: usize, drop: u16| {
        if drop < threshold {
            return;
        }
        let span = (cells[peak].0 as i32 - cells[trough].0 as i32).unsigned_abs() as u16;
        let slope = drop as f32 / span.max(1) as f32;
        if best.is_none_or(|(_, _, s)| slope < s) {
            best = Some((peak, trough, slope));
        }
    };

    // Rightward descents: maximal runs where the row does not decrease (peak = left end, trough = right).
    let mut i = 0;
    while i + 1 < cells.len() {
        let mut j = i;
        while j + 1 < cells.len() && cells[j + 1].1 >= cells[j].1 {
            j += 1;
        }
        if j > i {
            consider(i, j, cells[j].1 - cells[i].1);
            i = j;
        } else {
            i += 1;
        }
    }
    // Leftward descents: maximal runs where the row does not increase (peak = right end, trough = left).
    let mut i = 0;
    while i + 1 < cells.len() {
        let mut j = i;
        while j + 1 < cells.len() && cells[j + 1].1 <= cells[j].1 {
            j += 1;
        }
        if j > i {
            consider(j, i, cells[i].1 - cells[j].1);
            i = j;
        } else {
            i += 1;
        }
    }

    let (peak, trough, _) = best?;
    let path = if peak <= trough {
        cells[peak..=trough].to_vec()
    } else {
        let mut p = cells[trough..=peak].to_vec();
        p.reverse();
        p
    };
    Some(path)
}

/// Ignition for [`ChartRide`]. Navigating onto the chart or an auto-refresh *arms* it for a short
/// window; once the chart geometry is on screen and shows a rideable descent, it rolls the dice **once**
/// (at the arming event's probability) and then stays quiet until re-armed. Arriving at a chart usually
/// rides; a background refresh only occasionally re-rides — the rarer, "random, temporary" surprise.
struct ChartRideIgniter {
    armed_ns: i64,
    /// Whether the single dice roll for the current arming has been spent.
    rolled: bool,
    /// Probability the current arming actually fires (set by the arming event).
    chance: f64,
}

impl ChartRideIgniter {
    /// How long an arming stays live waiting for the chart geometry to appear.
    const ARMED_NS: i64 = 2_000_000_000;
    /// Arriving at a chart usually rides — occasionally not, so it stays a wink rather than a metronome.
    const NAV_CHANCE: f64 = 0.8;
    /// A background auto-refresh only occasionally re-rides the same view.
    const REFRESH_CHANCE: f64 = 0.25;

    fn new() -> Self {
        Self {
            armed_ns: 0,
            rolled: false,
            chance: 0.0,
        }
    }
}

impl MascotIgniter for ChartRideIgniter {
    fn arm(&mut self, ev: &MascotEvent) {
        // Arm on arriving at the chart, or on a *background* auto-refresh landing new data — the two
        // moments the chart's shape may have just changed while the user is watching. Navigation is the
        // deliberate "I opened this chart" moment, so it rides far more readily than a passive refresh.
        let chance = match ev {
            MascotEvent::Navigated {
                on_metric_chart: true,
            } => Self::NAV_CHANCE,
            MascotEvent::Refreshed { auto: true } => Self::REFRESH_CHANCE,
            _ => return,
        };
        self.armed_ns = Self::ARMED_NS;
        self.rolled = false;
        self.chance = chance;
    }

    fn poll(
        &mut self,
        dt_ns: i64,
        ctx: &MascotCtx,
        rng: &mut SmallRng,
    ) -> Option<Box<dyn MascotMotion>> {
        if self.armed_ns <= 0 {
            return None;
        }
        self.armed_ns -= dt_ns;
        if self.rolled {
            return None;
        }
        // Wait (staying armed) until a real opportunity exists: geometry on screen with a rideable drop.
        let chart = ctx.chart.as_ref()?;
        let path = great_displacement_path(chart)?;
        self.rolled = true;
        if rng.random_bool(self.chance) {
            Some(Box::new(ChartRide::new(path)))
        } else {
            None
        }
    }
}

/// The mascot controller: owns the body, the default idle wander, an optional running transient motion,
/// and the pluggable igniters. Advanced once per redraw by [`Mascot::update`].
struct Mascot {
    body: MascotBody,
    idle: IdleWander,
    active: Option<Box<dyn MascotMotion>>,
    igniters: Vec<Box<dyn MascotIgniter>>,
    activity: Activity,
    rng: SmallRng,
    last_tick: Instant,
    /// Whether the body has been positioned against a real area yet (deferred to the first update, when
    /// the terminal size is known).
    placed: bool,
}

impl Mascot {
    /// Clamp a per-frame delta so a long pause (mascot hidden, a modal open) never teleports it.
    const MAX_DT_NS: i64 = 500_000_000;

    fn new() -> Self {
        Self {
            body: MascotBody {
                x: 0.0,
                y: 0.0,
                facing: -1,
                phase_ns: 0,
            },
            idle: IdleWander::new(),
            active: None,
            igniters: vec![Box::new(ChartRideIgniter::new())],
            activity: Activity::Active,
            // Seed from the wall clock: varied per run, no OS entropy needed, and tests inject a fixed
            // controller when they need determinism.
            rng: SmallRng::seed_from_u64(Timestamp::now().0 as u64),
            last_tick: Instant::now(),
            placed: false,
        }
    }

    /// Whether a scripted motion is currently running (the run loop wakes faster when so).
    fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    /// Advance the mascot: apply `events`, then move by the elapsed time within `ctx`.
    fn update(&mut self, events: &[MascotEvent], ctx: &MascotCtx) {
        let now = Instant::now();
        let dt = (now.duration_since(self.last_tick).as_nanos() as i64).min(Self::MAX_DT_NS);
        self.last_tick = now;

        if !self.placed {
            self.place(ctx.area);
            self.placed = true;
        }

        for ev in events {
            match ev {
                MascotEvent::Active => self.activity = Activity::Active,
                MascotEvent::Idle => self.activity = Activity::Idle,
                _ => {}
            }
            for ig in &mut self.igniters {
                ig.arm(ev);
            }
        }

        self.body.phase_ns += dt; // the waddle bob always animates

        if let Some(motion) = self.active.as_mut() {
            if !motion.tick(dt, &mut self.body, ctx) {
                self.active = None;
            }
            return;
        }
        // No active motion: let an igniter start one, else wander while the user is idle.
        for ig in &mut self.igniters {
            if let Some(motion) = ig.poll(dt, ctx, &mut self.rng) {
                self.active = Some(motion);
                return;
            }
        }
        if self.activity == Activity::Idle {
            self.idle.tick(dt, &mut self.body, ctx, &mut self.rng);
        }
    }

    /// Initial resting spot: the bottom-right band, matching the historical corner overlay.
    fn place(&mut self, area: Rect) {
        self.body.x = area.right().saturating_sub(MASCOT_ART_WIDTH + 1) as f32;
        self.body.y = area
            .bottom()
            .saturating_sub(MASCOT_ART_HEIGHT + MASCOT_BOTTOM_MARGIN) as f32;
    }
}

// --- Metric-chart geometry: reproduce ratatui-widgets 0.3.2's `Chart` layout so the mascot can ride
// the actual rendered line. See `ratatui-widgets/src/{chart.rs,canvas.rs}`.

/// The plotting rectangle ratatui reserves inside a `Chart` widget area: the block border is removed
/// first, then space is taken on the left for y-axis labels (+1 for the axis line) and two rows at the
/// bottom for the x-axis labels + line. `block_inner` is `block.inner(plot_area)`.
fn chart_graph_area(block_inner: Rect, y_labels: &[String], x_first_label: &str) -> Option<Rect> {
    if block_inner.width == 0 || block_inner.height == 0 {
        return None;
    }
    let has_y_axis = !y_labels.is_empty();
    let y_max_w = y_labels.iter().map(|s| s.width() as u16).max().unwrap_or(0);
    // x-axis labels default to Left alignment, so the first one overhangs the y-axis gutter by width-1.
    let x_overhang = (x_first_label.width() as u16).saturating_sub(u16::from(has_y_axis));
    let label_left = y_max_w.max(x_overhang).min(block_inner.width / 3);

    let mut x = block_inner.left();
    let mut y = block_inner.bottom() - 1;
    if y > block_inner.top() {
        y -= 1; // x-axis labels row
    }
    x += label_left;
    if y > block_inner.top() {
        y -= 1; // x-axis line row
    }
    if has_y_axis && x + 1 < block_inner.right() {
        x += 1; // y-axis line column
    }
    let graph_w = block_inner.right().saturating_sub(x);
    let graph_h = y.saturating_sub(block_inner.top()).saturating_add(1);
    if graph_w == 0 || graph_h == 0 {
        return None;
    }
    Some(Rect::new(x, block_inner.top(), graph_w, graph_h))
}

/// The terminal cell a datapoint `(x, y)` renders into within `graph`, using ratatui's Braille canvas
/// mapping (2×4 dots per cell). `None` if the point is out of the axis bounds (ratatui would not plot it).
fn chart_point_cell(
    graph: Rect,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    x: f64,
    y: f64,
) -> Option<(u16, u16)> {
    if x < x_min || x > x_max || y < y_min || y > y_max {
        return None;
    }
    let (w, h) = (x_max - x_min, y_max - y_min);
    if w <= 0.0 || h <= 0.0 || graph.width == 0 || graph.height == 0 {
        return None;
    }
    let res_x = graph.width as f64 * 2.0;
    let res_y = graph.height as f64 * 4.0;
    let dot_x = ((x - x_min) * (res_x - 1.0) / w).round() as u32;
    let dot_y = ((y_max - y) * (res_y - 1.0) / h).round() as u32;
    let col = graph.left() + (dot_x / 2) as u16;
    let row = graph.top() + (dot_y / 4) as u16;
    Some((col, row))
}

/// Transient input overlays on top of the current [`Route`]. Only `Normal` lets background
/// auto-refresh run, and overlays never participate in the navigation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    /// The top menu bar is activated (Midnight-Commander F9): the cursor/Tab move a highlight across
    /// the screen menu and the time-range item, Enter activates, Esc/F9 dismisses.
    Menu,
    Editing,
    TimeRange,
    /// The absolute-time window form (two datetime fields), opened from the time-range dropdown.
    AbsoluteRange,
}

/// The stop the keyboard focus ring is on. `Tab`/`Shift+Tab` cycle it in reading order (the menu bar
/// left-to-right, then the content top-to-bottom): `Menu(0..4)` (the screen items) → `TimeRange` (the
/// menu-bar time selector) → `Query` → `Primary` (main list/table), wrapping. The highlight follows it
/// and `Enter` activates the focused stop. Arrow keys are unaffected — they always drive the main list.
/// `Query` is only reachable on views that have a query pane (see `App::has_query`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// A screen item in the menu bar, indexed into `Screen::ORDER` (`0..Screen::ORDER.len()`). `Enter`
    /// switches to that screen, like the F9 menu.
    Menu(usize),
    /// The menu bar's rightmost item, the time-window selector.
    TimeRange,
    /// The query editor pane.
    Query,
    /// The main list/table (or a detail route's body).
    Primary,
}

/// Structured fields of a log entry, kept alongside the rendered rows so the detail view can show the
/// full record and the trace-id jump has a real id to navigate to.
#[derive(Debug, Clone)]
struct LogRecord {
    time_ns: i64,
    severity: String,
    service: Option<String>,
    body: String,
    trace_id: Option<String>,
    span_id: Option<String>,
    attributes: Vec<(String, String)>,
    resource: Vec<(String, String)>,
    scope: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub refresh_interval: Duration,
    pub lookback: Duration,
    pub step: Duration,
    /// When `Some((start_ns, end_ns))`, the query window is this fixed absolute span instead of the
    /// rolling `now - lookback .. now`; the step is then derived from the span. Set from the TUI's
    /// absolute-time picker (`App::abs_window`).
    pub window: Option<(i64, i64)>,
    pub max_series: usize,
    pub max_rows: usize,
    pub ascii: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(5),
            lookback: Duration::from_secs(15 * 60),
            step: Duration::from_secs(30),
            window: None,
            max_series: 100,
            max_rows: 100,
            ascii: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Overview,
    Metrics,
    Traces,
    Logs,
}

impl Screen {
    /// Left-to-right order of the screen menu (the F9 menu bar navigates this plus a trailing
    /// time-range item).
    const ORDER: [Screen; 4] = [
        Screen::Overview,
        Screen::Metrics,
        Screen::Traces,
        Screen::Logs,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Metrics => "Metrics",
            Self::Traces => "Traces",
            Self::Logs => "Logs",
        }
    }

    fn index(self) -> usize {
        Self::ORDER.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Number of items on the menu bar: the four screens plus the trailing time-range selector. The
/// range item is the last index.
const MENU_LEN: usize = Screen::ORDER.len() + 1;

/// A secondary result pane rendered below the primary list (the Traces screen uses it for the
/// first result's span waterfall). Its own title and lines; it renders from the top and does not
/// take the primary pane's scroll.
#[derive(Debug, Clone)]
struct DetailPane {
    title: String,
    lines: Vec<String>,
    /// When set, a trace waterfall whose bars reflow to fill the pane width at draw time; `lines` is
    /// then unused. Placeholders and errors leave this `None` and fall back to `lines`.
    waterfall: Option<Waterfall>,
}

/// A trace's spans reduced to width-independent pieces, so the bars can be re-rendered to whatever
/// width the detail pane is given at draw time (`render_waterfall`) rather than baked to a fixed size.
#[derive(Debug, Clone)]
struct Waterfall {
    rows: Vec<WaterfallRow>,
    /// The bar glyph (`━`, or `#` in ASCII mode).
    marker: char,
}

/// One waterfall row: a fixed-width prefix and trailing column, plus the bar's position as fractions
/// of the trace duration. `render_waterfall` maps `start`/`frac` onto the available bar cells.
#[derive(Debug, Clone)]
struct WaterfallRow {
    /// Status marker + depth indent + `WATERFALL_NAME_W`-clamped name: the constant-width prefix
    /// before the bar, so every bar starts at the same column regardless of depth.
    prefix: String,
    /// Bar start as a fraction of the trace duration, in `[0, 1)`.
    start: f64,
    /// Bar length as a fraction of the trace duration, in `(0, 1]`.
    frac: f64,
    /// The trailing `  12.345ms OK` (duration + status) column, rendered as-is after the closing bar.
    suffix: String,
}

/// Fixed width (terminal cells) of the waterfall name column; the status marker prepends one more.
const WATERFALL_NAME_W: usize = 20;
/// Cells kept to the right of the bar for the ` 12.345ms STATUS` column, so the bar never crowds it.
const WATERFALL_SUFFIX_W: usize = 20;

/// Floor for the pan/zoom query-window span (1 second): zooming in never collapses the window below it.
const MIN_WINDOW_NS: i64 = 1_000_000_000;
/// Ceiling for the pan/zoom query-window span (~1 year): zooming out never widens past it (and keeps
/// the center ± half-span arithmetic well clear of `i64` overflow).
const MAX_WINDOW_NS: i64 = 366 * 24 * 3_600 * 1_000_000_000;

/// The smallest terminal the full UI lays out in; below either dimension `draw` shows a resize prompt
/// instead (first-release acceptance criterion, TUI_PLAN.md §10). Kept at/under the smallest size the
/// existing render tests exercise so those still paint the real UI.
const MIN_COLS: u16 = 40;
const MIN_ROWS: u16 = 10;

/// Column-aligned tabular data for the primary pane (the Metrics screen). Rendered with a header row
/// and a selectable highlighted row; the selection cursor indexes `rows` directly.
#[derive(Debug, Clone)]
struct TableData {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// A metric in the catalog tree, expandable to its groupable dimensions.
#[derive(Debug, Clone)]
struct MetricNode {
    name: String,
    kind: String,
    unit: String,
    temporality: String,
    expanded: bool,
    /// Selects the whole metric (no per-dimension filter) for visualization. The only way to select a
    /// metric that has no dimensions to check series under; toggled via the `(no dimensions)` row.
    whole_selected: bool,
    /// `None` until the dimensions have been discovered (by running the metric's base query and
    /// reading the returned series' labels); `Some` afterwards, possibly empty.
    dims: Option<Vec<DimNode>>,
    loading: bool,
}

/// A groupable label dimension under a metric (e.g. `by service`), expandable to its distinct values.
#[derive(Debug, Clone)]
struct DimNode {
    label: String,
    values: Vec<String>,
    expanded: bool,
    /// The checked value (index into `values`), or `None`. Exclusive within the axis — at most one
    /// value is selected per dimension.
    selected: Option<usize>,
}

/// Which tree node a flattened catalog row maps to, so a key press acts on the right node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeRowRef {
    Metric(usize),
    Dim(usize, usize),
    Value(usize, usize, usize),
    /// The `(no dimensions)` row under a metric with no dimensions — a checkbox that selects the whole
    /// metric.
    NoDims(usize),
}

/// One PromQL result series retained alongside the rendered table row, so the detailed time-series
/// viewer can plot the selected series' full `(timestamp_ns, value)` history. `series[i]` aligns with
/// the Metrics series table's row `i`.
#[derive(Debug, Clone, Default)]
struct SeriesData {
    labels: String,
    points: Vec<(i64, f64)>,
}

/// The selected series shown in the detailed time-series viewer (the `Route::MetricDetail` content).
/// Cloned on open so it survives background refreshes, like [`LogRecord`] for the log detail view.
#[derive(Debug, Clone)]
struct MetricDetail {
    labels: String,
    query: String,
    points: Vec<(i64, f64)>,
}

/// The current navigable view — the single source of truth for what the content area shows, and the
/// unit the back/forward history moves through. Transient input overlays are [`Mode`], not routes.
/// The detail views carry their own self-contained data so the history captures them for free, and
/// they render as ordinary content beneath the always-visible menu bar (they are not modal).
#[derive(Debug, Clone)]
enum Route {
    Overview,
    /// The Metrics list: the catalog tree (empty query) or the PromQL series table.
    Metrics,
    /// The detailed time-series viewer for one series.
    MetricDetail {
        detail: MetricDetail,
    },
    Traces,
    Logs,
    /// The full detail of one log record.
    LogDetail {
        record: LogRecord,
    },
}

impl Route {
    /// The screen this view belongs to (drives the menu highlight and the per-screen query buffer).
    fn screen(&self) -> Screen {
        match self {
            Route::Overview => Screen::Overview,
            Route::Metrics | Route::MetricDetail { .. } => Screen::Metrics,
            Route::Traces => Screen::Traces,
            Route::Logs | Route::LogDetail { .. } => Screen::Logs,
        }
    }

    /// The list route a screen switch lands on.
    fn list(screen: Screen) -> Route {
        match screen {
            Screen::Overview => Route::Overview,
            Screen::Metrics => Route::Metrics,
            Screen::Traces => Route::Traces,
            Screen::Logs => Route::Logs,
        }
    }

    /// Whether this route renders as full-content detail (no query pane, its own hint bar).
    fn is_detail(&self) -> bool {
        matches!(self, Route::MetricDetail { .. } | Route::LogDetail { .. })
    }
}

#[derive(Debug, Clone, Default)]
struct Snapshot {
    title: String,
    lines: Vec<String>,
    chart: Vec<u64>,
    detail: Option<DetailPane>,
    /// When `Some(n)`, the primary pane is a cursor-navigable list: lines `[0, n)` are header/info
    /// text and lines `[n, len)` are selectable rows. `None` keeps the plain scrolled view.
    list_from: Option<usize>,
    /// Structured log records, aligned to the selectable rows (`log_records[i]` ↔ `lines[list_from+i]`).
    /// Populated only on the Logs screen; drives the detail view and trace-id navigation.
    log_records: Vec<LogRecord>,
    /// When `Some`, the primary pane renders as a selectable table (the Metrics screen). Takes
    /// precedence over `list_from`; the selection cursor then indexes `table.rows`.
    table: Option<TableData>,
    /// PromQL result series aligned to `table.rows` (Metrics screen only); drives the detailed
    /// time-series viewer opened with Enter on a selected series.
    series: Vec<SeriesData>,
    /// Resume cursor for the next (older) log page, echoed from [`imbh::LogPage::next`]. `Some` only on
    /// the Logs screen when a full page was returned (more rows may follow); drives older/newer paging.
    /// `Option<PageCursor>` is `Default` (`None`), so the `#[derive(Default)]` above still holds.
    next_cursor: Option<PageCursor>,
}

/// A trace/span correlation filter applied on top of a Logs query — the target of a trace→log
/// drill-down. Held in [`App`] and captured by the navigation history so Back restores the drill-down.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LogCorrelation {
    /// Lowercase-hex trace id (parsed back to [`imbh::TraceId`] when the query is built).
    trace_id: String,
    /// Optional lowercase-hex span id, narrowing the correlation to a single span.
    span_id: Option<String>,
}

/// One metric exemplar reduced to what the exemplar→trace jump needs: when it was recorded and the
/// trace it links to. Only exemplars carrying a trace id become markers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExemplarMarker {
    time_ns: i64,
    /// Lowercase-hex trace id the exemplar points at.
    trace_id: String,
}

impl Snapshot {
    fn message(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            lines: vec![message.into()],
            chart: Vec::new(),
            detail: None,
            list_from: None,
            log_records: Vec::new(),
            table: None,
            series: Vec::new(),
            next_cursor: None,
        }
    }
}

struct QueryResult {
    generation: u64,
    screen: Screen,
    result: Result<Snapshot, String>,
}

/// Messages delivered to the event loop from background tasks.
enum Update {
    /// A completed (or failed) panel query.
    Query(QueryResult),
    /// Completion vocabulary (metric names) fetched from the catalog.
    Vocabulary(Vec<String>),
    /// The waterfall for the selected trace, fetched on demand. `generation`/`trace_id` guard against
    /// applying a stale result after the selection or query moved on.
    Waterfall {
        generation: u64,
        trace_id: String,
        detail: DetailPane,
    },
    /// The discovered dimensions for a catalog metric, loaded when the metric is first expanded.
    MetricDims { metric: String, dims: Vec<DimNode> },
    /// The discovered log label names (Logs `{…}` selector completion vocabulary), fetched when the
    /// caret first enters a label-name position on the Logs screen.
    LogLabels(Vec<String>),
    /// The discovered distinct values for one log label (Logs quoted-matcher completion vocabulary),
    /// fetched when the caret first enters that label's value position.
    LogLabelValues { label: String, values: Vec<String> },
    /// Exemplar→trace markers for an open metric-detail view, fetched when it opens. `labels`/`query`
    /// identify the series the fetch was issued for, so a stale result (the view moved on) is dropped.
    Exemplars {
        labels: String,
        query: String,
        markers: Vec<ExemplarMarker>,
    },
}

/// What a completion candidate represents, so accepting a function can append its opening paren.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    Function,
    Keyword,
    Metric,
    /// A label/attribute name (inside a `{…}` matcher block).
    Label,
    /// A label value (inside a quoted matcher value).
    LabelValue,
    /// A LogQL line-filter operator hint (`|=` / `!=` / `|~` / `!~` / `|?` / `!?`), offered in
    /// expression position on the Logs screen. Inserted verbatim, like a keyword (no trailing paren).
    Operator,
}

/// Where in the query the caret (always the end of the string) sits, which decides *which* vocabulary
/// is eligible. Metric names and functions only make sense in expression position; inside a `{…}`
/// matcher the eligible items are label names, and inside a quoted value they are that label's values.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionContext {
    /// Expression position: metric names, functions, keywords.
    Expr,
    /// A label-name position inside a matcher block; `metric` is the selector before the `{` (if any).
    LabelName { metric: Option<String> },
    /// A label-value position inside a quoted string; `label` is the key being matched.
    LabelValue {
        metric: Option<String>,
        label: String,
    },
    /// A position where no vocabulary applies (e.g. an unquoted value after `=`), so no popup opens.
    Suppressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    text: String,
    kind: CandidateKind,
}

/// The open completion popup: the ranked candidates for the current token and the highlighted row.
#[derive(Debug, Clone)]
struct Completion {
    candidates: Vec<Candidate>,
    selected: usize,
}

/// What log-completion vocabulary the caret's position wants discovered, mirroring the Metrics
/// `completion_dim_request` flow but for the Logs screen's cross-signal attribute source.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LogCompletionRequest {
    /// The set of log label names (`db.attrs().names()`), for a `{…}` label-name position.
    Labels,
    /// The distinct values of one label (`db.attrs().values(key)`), for a quoted-value position.
    Values(String),
}

/// A snapshot of the navigation-relevant state for the browser-style back/forward history: the
/// [`Route`] (which carries the self-contained detail data) plus the per-screen query buffers and the
/// cursor context. Transient overlays, the time range, and the catalog tree are deliberately excluded
/// so Back/Forward move only through views (the tree is preserved separately, by name).
#[derive(Clone)]
struct NavEntry {
    route: Route,
    query: [String; 4],
    metric_cursor: usize,
    focus_trace_id: Option<String>,
    selected: usize,
    scroll: u16,
    /// The trace→log drill-down active in this view, so Back into a correlated Logs view restores it.
    log_correlation: Option<LogCorrelation>,
}

struct App {
    /// The current view (single source of truth); the `screen` is derived from it.
    route: Route,
    query: [String; 4],
    /// Transient input overlay on top of the route (menu / editing / range picker), or `Normal`.
    mode: Mode,
    range_index: usize,
    /// Highlighted candidate while the time-range picker is open; committed to `range_index` on Enter.
    /// Ranges over `0..=TIME_RANGES.len()`, where the last index is the "Absolute…" row.
    range_cursor: usize,
    /// Highlighted item while the menu bar is active (`Mode::Menu`): `0..Screen::ORDER.len()` are the
    /// screens, the last index (`MENU_LEN - 1`) is the time-range selector.
    menu_cursor: usize,
    /// When `Some`, an absolute query window `(start_ns, end_ns)` overriding the rolling preset; set
    /// from the absolute-time form and cleared by picking any relative preset.
    abs_window: Option<(i64, i64)>,
    /// Editable buffers for the absolute-range form (UTC `YYYY-MM-DD HH:MM:SS`) and which field has
    /// focus (0 = start, 1 = end), plus the last parse error to surface in the form.
    abs_start: String,
    abs_end: String,
    abs_field: usize,
    abs_error: Option<String>,
    /// Background auto-refresh, off by default; toggled with space. Manual/query/switch refreshes
    /// always run regardless.
    auto_refresh: bool,
    loading: bool,
    pending_refresh: bool,
    generation: u64,
    snapshot: Snapshot,
    last_error: Option<String>,
    last_refresh: Instant,
    /// Selected row index (absolute into `snapshot.lines`) when the primary pane is a navigable list.
    selected: usize,
    /// First result row to display; advanced by the scroll keys, clamped in `draw`.
    scroll: u16,
    /// Bounds published by `draw` (which alone knows the viewport geometry) so the key handler can
    /// clamp scrolling without re-deriving the wrapped row count.
    max_scroll: Cell<u16>,
    page_rows: Cell<u16>,
    /// Metric names from the catalog, used as PromQL completion vocabulary. Filled asynchronously.
    metric_names: Vec<String>,
    /// The open completion popup, or `None` when nothing is being suggested.
    completion: Option<Completion>,
    /// Trace id whose waterfall is currently shown in the detail pane (or in flight), so the selected
    /// trace's waterfall is fetched only when the selection actually moves to a different trace.
    detail_trace_id: Option<String>,
    /// The x-axis cursor index into the `Route::MetricDetail` series' points (view context, so it is
    /// captured by the history but lives flat here rather than inside the route).
    metric_cursor: usize,
    /// When navigating from a log's detail to its trace, the trace id to focus the Traces waterfall on
    /// (overrides the selection until the cursor is moved or a matching row is found).
    focus_trace_id: Option<String>,
    /// The Metrics catalog tree (expansion + lazily-loaded dimensions). Rebuilt whenever the flat
    /// catalog snapshot arrives; drives the catalog table rendering.
    metric_tree: Vec<MetricNode>,
    /// Flattened catalog rows aligned to `snapshot.table.rows`, mapping each row to its tree node.
    tree_rows: Vec<TreeRowRef>,
    /// Discovered log label names — the cross-signal attribute keys (`db.attrs().names()`, which
    /// already folds in the promoted `service.name`), used as the label-name completion vocabulary
    /// inside the Logs `{…}` selector. `None` until fetched; `Some` afterwards (possibly empty).
    log_labels: Option<Vec<String>>,
    /// Whether a log-label-name discovery is in flight, so it fires at most once.
    log_labels_loading: bool,
    /// Discovered distinct values per log label (`db.attrs().values(key)`) — the label-value
    /// completion vocabulary inside a quoted Logs matcher. Filled lazily, one key at a time.
    log_label_values: HashMap<String, Vec<String>>,
    /// Log labels whose value discovery is in flight, so each key fires at most once.
    log_label_values_loading: HashSet<String>,
    /// Browser-style navigation history. A forward navigation (Enter/`t`/screen switch) pushes the
    /// view it leaves onto `back` and clears `forward`; `←` pops `back`, `→` pops `forward`.
    back: Vec<NavEntry>,
    forward: Vec<NavEntry>,
    /// The pane the focus ring is on (drives the pane highlight and what `Enter` activates). Transient
    /// view chrome like `mode`, so it is reset on navigation and excluded from the back/forward history.
    focus: Focus,
    /// Whether the animated mascot is shown. Off by default; toggled with `m` (a no-op on `--ascii`
    /// terminals, where the block-glyph art is never rendered).
    show_mascot: bool,
    /// The mascot controller (position, motions, event igniters). Advanced once per redraw in [`run`].
    mascot: Mascot,
    /// The metric chart's rendered geometry, published by `draw_metric_detail` for the mascot's chart
    /// ride and consumed in the run loop. `None` off the chart or when there is nothing to plot.
    chart_geom: RefCell<Option<ChartGeometry>>,
    /// Last-seen route identity, so the run loop can emit a mascot `Navigated` event on a change.
    mascot_route_tag: u64,
    /// Last-seen idle state, so the loop emits `Idle`/`Active` only on a transition.
    mascot_idle: bool,
    /// When the user last pressed a key; drives the idle/active distinction.
    mascot_last_input: Instant,
    /// Set when a query result lands, drained by the loop into a mascot `Refreshed` event.
    mascot_refresh_pending: bool,
    /// Older/newer log paging (Logs screen). `log_cursor_stack` holds the resume cursors used to reach
    /// the current page (empty = page 0, most recent); `log_next_cursor` is the cursor for the *next*
    /// older page, echoed from the last Logs result (`None` when the page was short — no older rows).
    /// `log_paging` marks the single refresh a page move drives, so `request_refresh` keeps the stack
    /// instead of resetting to page 0 (which it does on every other refresh).
    log_cursor_stack: Vec<PageCursor>,
    log_next_cursor: Option<PageCursor>,
    log_paging: bool,
    /// Active trace→log drill-down correlation (set when jumping from a trace to its logs); layered onto
    /// the Logs query until the user leaves Logs or runs a fresh query. Captured by the nav history.
    log_correlation: Option<LogCorrelation>,
    /// Exemplar→trace markers for the open metric-detail view: the metric's exemplars that carry a trace
    /// id and fall within the plotted window. `Enter` jumps to the trace of the marker nearest the chart
    /// cursor. Fetched asynchronously on open; guarded by `generation` so a stale fetch is dropped.
    metric_exemplars: Vec<ExemplarMarker>,
}

impl App {
    fn new() -> Self {
        // Default to the 15m window (index 1), matching the historical default lookback.
        let range_index = 1;
        Self {
            route: Route::Overview,
            query: [
                String::new(),
                String::new(),
                "{}".to_owned(),
                // Logs default: a bare selector matching everything (filtered list + volume sparkline).
                "{}".to_owned(),
            ],
            mode: Mode::Normal,
            range_index,
            range_cursor: range_index,
            menu_cursor: 0,
            abs_window: None,
            abs_start: String::new(),
            abs_end: String::new(),
            abs_field: 0,
            abs_error: None,
            auto_refresh: false,
            loading: false,
            pending_refresh: false,
            generation: 0,
            selected: 0,
            snapshot: Snapshot::message("Overview", "Loading..."),
            last_error: None,
            last_refresh: Instant::now(),
            scroll: 0,
            max_scroll: Cell::new(0),
            page_rows: Cell::new(1),
            metric_names: Vec::new(),
            completion: None,
            detail_trace_id: None,
            metric_cursor: 0,
            focus_trace_id: None,
            metric_tree: Vec::new(),
            tree_rows: Vec::new(),
            log_labels: None,
            log_labels_loading: false,
            log_label_values: HashMap::new(),
            log_label_values_loading: HashSet::new(),
            back: Vec::new(),
            forward: Vec::new(),
            focus: Focus::Primary,
            show_mascot: false,
            mascot: Mascot::new(),
            chart_geom: RefCell::new(None),
            mascot_route_tag: 0,
            mascot_idle: false,
            mascot_last_input: Instant::now(),
            mascot_refresh_pending: false,
            log_cursor_stack: Vec::new(),
            log_next_cursor: None,
            log_paging: false,
            log_correlation: None,
            metric_exemplars: Vec::new(),
        }
    }

    /// A cheap identity of the current view, so a change (screen switch, opening/leaving a detail, or
    /// selecting a different series) reads as a navigation event for the mascot.
    fn mascot_route_tag(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.route.screen().index().hash(&mut h);
        match &self.route {
            Route::MetricDetail { detail } => {
                1u8.hash(&mut h);
                detail.labels.hash(&mut h);
                detail.query.hash(&mut h);
            }
            Route::LogDetail { .. } => 2u8.hash(&mut h),
            _ => 0u8.hash(&mut h),
        }
        h.finish()
    }

    fn lookback(&self) -> Duration {
        TIME_RANGES[self.range_index].1
    }

    fn step(&self) -> Duration {
        TIME_RANGES[self.range_index].2
    }

    fn range_label(&self) -> &'static str {
        TIME_RANGES[self.range_index].0
    }

    /// The screen the current route belongs to.
    fn screen(&self) -> Screen {
        self.route.screen()
    }

    /// The log record if the current route is the log detail view.
    fn route_log_record(&self) -> Option<&LogRecord> {
        match &self.route {
            Route::LogDetail { record } => Some(record),
            _ => None,
        }
    }

    /// The series if the current route is the detailed time-series viewer.
    fn route_metric_detail(&self) -> Option<&MetricDetail> {
        match &self.route {
            Route::MetricDetail { detail } => Some(detail),
            _ => None,
        }
    }

    /// Activate the menu bar (`F9`), starting the highlight on the current screen.
    fn open_menu(&mut self) {
        self.menu_cursor = self.screen().index();
        self.mode = Mode::Menu;
    }

    /// Move the menu highlight by `delta`, wrapping across the screens and the trailing range item.
    fn menu_move(&mut self, delta: isize) {
        self.menu_cursor =
            (self.menu_cursor as isize + delta).rem_euclid(MENU_LEN as isize) as usize;
    }

    /// The screen the menu highlight is on, or `None` when it is on the trailing time-range item.
    fn menu_screen(&self) -> Option<Screen> {
        Screen::ORDER.get(self.menu_cursor).copied()
    }

    /// Whether the current view has a query editor pane (every list screen except Overview; the detail
    /// routes render full-content and have none). Also decides whether `Focus::Query` is reachable.
    fn has_query(&self) -> bool {
        self.screen() != Screen::Overview && !self.route.is_detail()
    }

    /// The focus ring for the current view, in reading order (the four menu-bar screen items, the time
    /// selector, then the content panes). The query stop is present only when the view has a query
    /// pane; `Tab`/`Shift+Tab` cycle this order (with wraparound).
    fn focus_ring(&self) -> &'static [Focus] {
        if self.has_query() {
            &[
                Focus::Menu(0),
                Focus::Menu(1),
                Focus::Menu(2),
                Focus::Menu(3),
                Focus::TimeRange,
                Focus::Query,
                Focus::Primary,
            ]
        } else {
            &[
                Focus::Menu(0),
                Focus::Menu(1),
                Focus::Menu(2),
                Focus::Menu(3),
                Focus::TimeRange,
                Focus::Primary,
            ]
        }
    }

    /// The focus as it actually applies to the current view: a stored `Query` focus snaps to `Primary`
    /// on a view with no query pane, so the highlight and `Enter` never target a pane that is not shown.
    fn effective_focus(&self) -> Focus {
        if self.focus == Focus::Query && !self.has_query() {
            Focus::Primary
        } else {
            self.focus
        }
    }

    /// Advance the focus ring by `delta` (Tab: +1 down, Shift+Tab: -1 up), wrapping. Anchored on the
    /// effective focus so it steps sensibly even when a stale `Query` focus was snapped to `Primary`.
    fn focus_advance(&mut self, delta: isize) {
        let ring = self.focus_ring();
        let current = ring
            .iter()
            .position(|f| *f == self.effective_focus())
            .unwrap_or(ring.len() - 1) as isize;
        let next = (current + delta).rem_euclid(ring.len() as isize) as usize;
        self.focus = ring[next];
    }

    /// Move the focus among the menu-bar items only — the four screen items and the trailing time
    /// selector — wrapping over `MENU_LEN`. Bound to Left/Right while the ring is on the bar (there they
    /// select rather than navigate history). A no-op unless the focus is already on a menu-bar stop (its
    /// natural precondition), mirroring `menu_move`.
    fn menubar_move(&mut self, delta: isize) {
        let current = match self.effective_focus() {
            Focus::Menu(index) => index,
            Focus::TimeRange => MENU_LEN - 1,
            _ => return,
        };
        let next = (current as isize + delta).rem_euclid(MENU_LEN as isize) as usize;
        self.focus = if next == MENU_LEN - 1 {
            Focus::TimeRange
        } else {
            Focus::Menu(next)
        };
    }

    /// Snapshot the current view for the history.
    fn capture_nav(&self) -> NavEntry {
        NavEntry {
            route: self.route.clone(),
            query: self.query.clone(),
            metric_cursor: self.metric_cursor,
            focus_trace_id: self.focus_trace_id.clone(),
            selected: self.selected,
            scroll: self.scroll,
            log_correlation: self.log_correlation.clone(),
        }
    }

    /// Restore a captured view (the data pane is reloaded by the caller's refresh). Any transient
    /// overlay is dropped — history moves between views, not input modes.
    fn restore_nav(&mut self, entry: NavEntry) {
        self.route = entry.route;
        self.query = entry.query;
        self.metric_cursor = entry.metric_cursor;
        self.focus_trace_id = entry.focus_trace_id;
        self.selected = entry.selected;
        self.scroll = entry.scroll;
        self.log_correlation = entry.log_correlation;
        // Exemplar markers are view-specific; a metric detail restored by history refetches them.
        self.metric_exemplars.clear();
        self.mode = Mode::Normal;
        self.completion = None;
        self.focus = Focus::Primary;
    }

    /// Record `entry` as the view a forward navigation departs from, invalidating the Forward stack (a
    /// new branch). Used directly when the caller captures the departing view *before* it knows the
    /// navigation will succeed (so the history is only recorded on success).
    fn push_entry(&mut self, entry: NavEntry) {
        const CAP: usize = 64;
        self.back.push(entry);
        if self.back.len() > CAP {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Record the current view before a forward navigation. Called right before mutating to the
    /// destination view.
    fn push_history(&mut self) {
        let entry = self.capture_nav();
        self.push_entry(entry);
    }

    /// Browser Back: restore the previous view (pushing the current one onto Forward). Returns whether
    /// there was history to move through, so the caller can reload the restored view's data.
    fn go_back(&mut self) -> bool {
        if let Some(entry) = self.back.pop() {
            self.forward.push(self.capture_nav());
            self.restore_nav(entry);
            true
        } else {
            false
        }
    }

    /// Browser Forward: redo a Back (no effect unless a Back was taken).
    fn go_forward(&mut self) -> bool {
        if let Some(entry) = self.forward.pop() {
            self.back.push(self.capture_nav());
            self.restore_nav(entry);
            true
        } else {
            false
        }
    }

    /// The current effective query window `(start_ns, end_ns)`: the committed absolute window if set,
    /// otherwise the rolling `now - lookback .. now` derived from the selected preset.
    fn effective_window(&self) -> (i64, i64) {
        match self.abs_window {
            Some(window) => window,
            None => {
                let end = Timestamp::now().0;
                let start =
                    end.saturating_sub(self.lookback().as_nanos().min(i64::MAX as u128) as i64);
                (start, end)
            }
        }
    }

    /// Human-readable window for the header indicator: the absolute span (with the shared date shown
    /// once for a same-day window) or `last <preset>` for a rolling window.
    fn range_summary(&self, g: &Glyphs) -> String {
        match self.abs_window {
            Some((start, end)) => {
                let (start, end) = (format_datetime_ns(start), format_datetime_ns(end));
                let arrow = g.right;
                if start[..10] == end[..10] {
                    // Same UTC date: collapse it and show `date start_time → end_time`.
                    format!("{start} {arrow} {}", &end[11..])
                } else {
                    format!("{start} {arrow} {end}")
                }
            }
            None => format!("last {}", self.range_label()),
        }
    }

    /// Prefill the absolute-range form from the current effective window and open it.
    fn open_absolute_form(&mut self) {
        let (start, end) = self.effective_window();
        self.abs_start = format_datetime_ns(start);
        self.abs_end = format_datetime_ns(end);
        self.abs_field = 0;
        self.abs_error = None;
        self.mode = Mode::AbsoluteRange;
    }

    /// Parse and commit the absolute-range form. On success sets `abs_window`, returns to Normal, and
    /// returns `true` (the caller triggers a refresh); on failure records `abs_error` and returns
    /// `false`, keeping the form open.
    fn commit_absolute(&mut self) -> bool {
        match (
            parse_datetime(&self.abs_start),
            parse_datetime(&self.abs_end),
        ) {
            (None, _) => {
                self.abs_error = Some("start: expected UTC YYYY-MM-DD HH:MM:SS".to_owned());
                false
            }
            (_, None) => {
                self.abs_error = Some("end: expected UTC YYYY-MM-DD HH:MM:SS".to_owned());
                false
            }
            (Some(start), Some(end)) if start >= end => {
                self.abs_error = Some("start must be before end".to_owned());
                false
            }
            (Some(start), Some(end)) => {
                self.abs_window = Some((start, end));
                self.abs_error = None;
                self.mode = Mode::Normal;
                self.scroll = 0;
                self.selected = 0;
                true
            }
        }
    }

    /// Pan the query window by `fraction` of its span (negative = earlier, positive = later). Freezes
    /// Pan the query window by `fraction` of its span (negative = earlier, positive = later). Freezes
    /// the current effective window into an absolute one so panning is stable against the wall clock —
    /// *except* that panning later far enough to reach `now` resumes the **live rolling window** (rather
    /// than pinning an absolute window frozen at the present instant, which would stop tailing new
    /// data). Returns whether the window changed.
    fn pan_window(&mut self, fraction: f64) -> bool {
        let (start, end) = self.effective_window();
        let span = (end - start).max(1);
        let delta = (span as f64 * fraction) as i64;
        if delta > 0 {
            // Panning later: never cross `now`. Reaching it snaps back to the live rolling window so
            // the view keeps following new data (a pure `[`…`]` round-trip returns you to "live", not a
            // window frozen at the moment you caught up).
            let room = (Timestamp::now().0 - end).max(0);
            if delta >= room {
                if self.abs_window.is_none() {
                    return false; // already live at `now` — nothing to do
                }
                self.abs_window = None;
                self.scroll = 0;
                self.selected = 0;
                return true;
            }
            self.abs_window = Some((start + delta, end + delta));
            self.scroll = 0;
            self.selected = 0;
            return true;
        }
        if delta == 0 {
            return false;
        }
        self.abs_window = Some((start.saturating_add(delta), end.saturating_add(delta)));
        self.scroll = 0;
        self.selected = 0;
        true
    }

    /// Zoom the query window about its center by `factor` (`> 1` widens / zooms out, `< 1` narrows /
    /// zooms in), clamped to `[MIN_WINDOW_NS, MAX_WINDOW_NS]`. Returns whether the window changed.
    fn zoom_window(&mut self, factor: f64) -> bool {
        let (start, end) = self.effective_window();
        let span = (end - start).max(1);
        let new_span = ((span as f64 * factor).round() as i64).clamp(MIN_WINDOW_NS, MAX_WINDOW_NS);
        if new_span == span {
            return false;
        }
        let center = start + span / 2;
        let half = new_span / 2;
        self.abs_window = Some((center - half, center + half));
        self.scroll = 0;
        self.selected = 0;
        true
    }

    /// Page the Logs list one step older (toward earlier records), if the current page reported a
    /// resume cursor. Returns whether a move happened (the caller then refreshes).
    fn logs_page_older(&mut self) -> bool {
        if self.screen() != Screen::Logs || self.route.is_detail() {
            return false;
        }
        let Some(cursor) = self.log_next_cursor else {
            return false;
        };
        self.log_cursor_stack.push(cursor);
        self.log_paging = true;
        self.scroll = 0;
        self.selected = 0;
        true
    }

    /// Page the Logs list one step newer (toward the most recent records), if not already on page 0.
    /// Returns whether a move happened.
    fn logs_page_newer(&mut self) -> bool {
        if self.screen() != Screen::Logs || self.route.is_detail() {
            return false;
        }
        if self.log_cursor_stack.pop().is_none() {
            return false;
        }
        self.log_paging = true;
        self.scroll = 0;
        self.selected = 0;
        true
    }

    /// The trace id of the exemplar marker nearest the metric-detail chart cursor, if any — the target
    /// of the exemplar→trace jump (`Enter` on the metric detail).
    fn nearest_exemplar_trace(&self) -> Option<String> {
        let detail = self.route_metric_detail()?;
        if detail.points.is_empty() || self.metric_exemplars.is_empty() {
            return None;
        }
        let cursor = self.metric_cursor.min(detail.points.len() - 1);
        let cursor_ns = detail.points[cursor].0;
        self.metric_exemplars
            .iter()
            .min_by_key(|marker| marker.time_ns.abs_diff(cursor_ns))
            .map(|marker| marker.trace_id.clone())
    }

    fn query_index(&self) -> usize {
        match self.screen() {
            Screen::Overview => 0,
            Screen::Metrics => 1,
            Screen::Traces => 2,
            Screen::Logs => 3,
        }
    }

    fn active_query(&self) -> &str {
        &self.query[self.query_index()]
    }

    fn active_query_mut(&mut self) -> &mut String {
        let index = self.query_index();
        &mut self.query[index]
    }

    fn apply(&mut self, result: QueryResult) {
        self.loading = false;
        if result.generation != self.generation || result.screen != self.screen() {
            return;
        }
        self.last_refresh = Instant::now();
        match result.result {
            Ok(snapshot) => {
                // Echo the next-older-page cursor (Logs only; `None` elsewhere) so the paging keys know
                // whether an older page exists.
                self.log_next_cursor = snapshot.next_cursor;
                self.snapshot = snapshot;
                self.last_error = None;
                // Keep the row cursor within the new result's selectable range (rows can shrink on
                // refresh); starting from 0 this lands the cursor on the first selectable row.
                if let Some((first, last)) = self.selectable_bounds() {
                    self.selected = self.selected.clamp(first, last);
                }
                // The new snapshot ships a placeholder detail; force the selected trace's waterfall to
                // be (re)fetched by clearing what we think is shown.
                self.detail_trace_id = None;
                // Keep an open metric-detail chart live: re-derive its points from the matching series
                // in the fresh result (matched by label set), so a range change / pan / zoom / auto-
                // refresh actually redraws the plotted window instead of showing the frozen open-time
                // snapshot. An empty match (the series left the window) clears the plot honestly.
                if let Some(labels) = match &self.route {
                    Route::MetricDetail { detail } => Some(detail.labels.clone()),
                    _ => None,
                } {
                    let points = self
                        .snapshot
                        .series
                        .iter()
                        .find(|series| series.labels == labels)
                        .map(|series| series.points.clone())
                        .unwrap_or_default();
                    if let Route::MetricDetail { detail } = &mut self.route {
                        detail.points = points;
                    }
                    self.metric_cursor = self.metric_cursor.min(
                        self.route_metric_detail()
                            .map_or(0, |detail| detail.points.len().saturating_sub(1)),
                    );
                }
            }
            Err(error) => self.last_error = Some(error),
        }
    }

    /// The inclusive `[first, last]` selection-index range the row cursor may occupy, or `None` when
    /// the primary pane is not navigable. For a table the index is into `table.rows` (`first == 0`);
    /// for a list it is an absolute index into `lines` (`first == list_from`). A screen is only ever
    /// one of the two, so `selected` never mixes interpretations.
    fn selectable_bounds(&self) -> Option<(usize, usize)> {
        if let Some(table) = &self.snapshot.table {
            return (!table.rows.is_empty()).then(|| (0, table.rows.len() - 1));
        }
        let first = self.snapshot.list_from?;
        let len = self.snapshot.lines.len();
        (first < len).then(|| (first, len - 1))
    }

    /// The trace id of the currently selected row on the Traces screen, parsed from the leading
    /// whitespace-delimited token of the row (rows are `"{trace_id} selected=…"`). `None` on other
    /// screens or when nothing is selectable.
    fn selected_trace_id(&self) -> Option<String> {
        if self.screen() != Screen::Traces {
            return None;
        }
        let (first, last) = self.selectable_bounds()?;
        let selected = self.selected.clamp(first, last);
        self.snapshot
            .lines
            .get(selected)?
            .split_whitespace()
            .next()
            .map(str::to_owned)
    }

    /// Whether the Metrics catalog tree is the current view (Metrics screen with an empty query).
    fn on_catalog(&self) -> bool {
        self.screen() == Screen::Metrics && self.active_query().trim().is_empty()
    }

    /// (Re)build the catalog tree from a freshly-arrived flat catalog snapshot, then render it.
    /// Per-metric UI state (expansion, discovered dimensions, and the checked series/whole-metric
    /// selection) is carried over by name, so a catalog refresh — including navigating away to the
    /// series list and back — preserves the selection instead of resetting it.
    fn build_metric_tree(&mut self) {
        let Some(table) = &self.snapshot.table else {
            return;
        };
        // Only the catalog table (Metric/Kind/Unit/Temporality) is a tree; the series table is not.
        if table.header.first().map(String::as_str) != Some("Metric") {
            return;
        }
        let mut prior: HashMap<String, MetricNode> = self
            .metric_tree
            .drain(..)
            .map(|node| (node.name.clone(), node))
            .collect();
        self.metric_tree = table
            .rows
            .iter()
            .map(|row| {
                let name = row.first().cloned().unwrap_or_default();
                let kind = row.get(1).cloned().unwrap_or_default();
                let unit = row.get(2).cloned().unwrap_or_default();
                let temporality = row.get(3).cloned().unwrap_or_default();
                match prior.remove(&name) {
                    // Keep expansion/dims/selection; refresh the (possibly changed) static metadata.
                    Some(mut node) => {
                        node.kind = kind;
                        node.unit = unit;
                        node.temporality = temporality;
                        node
                    }
                    None => MetricNode {
                        name,
                        kind,
                        unit,
                        temporality,
                        expanded: false,
                        whole_selected: false,
                        dims: None,
                        loading: false,
                    },
                }
            })
            .collect();
        self.rebuild_catalog_table();
    }

    /// Flatten the catalog tree into `snapshot.table` and the parallel `tree_rows`, preserving the
    /// cursor position within the new row count.
    fn rebuild_catalog_table(&mut self) {
        // Plain ASCII disclosure markers `v`/`>` on every terminal. The small-triangle glyphs (▾/▸)
        // render too small to read as open/closed on many terminals, and the full-size ▼/▶ are
        // East-Asian-width *ambiguous* (they desync the width-aware column sizing), so `v`/`>` it is.
        let (branch_open, branch_closed) = ("v ", "> ");
        let mut rows = Vec::new();
        let mut refs = Vec::new();
        for (mi, metric) in self.metric_tree.iter().enumerate() {
            let marker = if metric.expanded {
                branch_open
            } else {
                branch_closed
            };
            rows.push(vec![
                format!("{marker}{}", metric.name),
                metric.kind.clone(),
                metric.unit.clone(),
                metric.temporality.clone(),
            ]);
            refs.push(TreeRowRef::Metric(mi));
            if !metric.expanded {
                continue;
            }
            match &metric.dims {
                None => {
                    rows.push(vec![
                        "    (loading dimensions...)".to_owned(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ]);
                    refs.push(TreeRowRef::Metric(mi));
                }
                Some(dims) if dims.is_empty() => {
                    // A checkbox to select the whole metric (there are no series to check).
                    let check = if metric.whole_selected { "[x]" } else { "[ ]" };
                    rows.push(vec![
                        format!("    {check} (no dimensions)"),
                        String::new(),
                        String::new(),
                        String::new(),
                    ]);
                    refs.push(TreeRowRef::NoDims(mi));
                }
                Some(dims) => {
                    for (di, dim) in dims.iter().enumerate() {
                        let marker = if dim.expanded {
                            branch_open
                        } else {
                            branch_closed
                        };
                        // Show the checked value (if any) on the dimension row so it stays visible
                        // when the axis is collapsed.
                        let chosen = dim
                            .selected
                            .and_then(|vi| dim.values.get(vi))
                            .map(|value| format!(" = {value}"))
                            .unwrap_or_default();
                        rows.push(vec![
                            format!("  {marker}by {} ({}){chosen}", dim.label, dim.values.len()),
                            String::new(),
                            String::new(),
                            String::new(),
                        ]);
                        refs.push(TreeRowRef::Dim(mi, di));
                        if dim.expanded {
                            for (vi, value) in dim.values.iter().enumerate() {
                                let check = if dim.selected == Some(vi) {
                                    "[x]"
                                } else {
                                    "[ ]"
                                };
                                rows.push(vec![
                                    format!("      {check} {}={}", dim.label, value),
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ]);
                                refs.push(TreeRowRef::Value(mi, di, vi));
                            }
                        }
                    }
                }
            }
        }
        self.tree_rows = refs;
        let header = self
            .snapshot
            .table
            .as_ref()
            .map(|table| table.header.clone())
            .unwrap_or_else(|| {
                vec![
                    "Metric".to_owned(),
                    "Kind".to_owned(),
                    "Unit".to_owned(),
                    "Temporality".to_owned(),
                ]
            });
        self.snapshot.table = Some(TableData { header, rows });
        if let Some((first, last)) = self.selectable_bounds() {
            self.selected = self.selected.clamp(first, last);
        }
    }

    /// The tree node the cursor is on (catalog view only).
    fn selected_tree_row(&self) -> Option<TreeRowRef> {
        if !self.on_catalog() {
            return None;
        }
        let (first, last) = self.selectable_bounds()?;
        self.tree_rows
            .get(self.selected.clamp(first, last))
            .copied()
    }

    /// Handle Space on the selected node: expand/collapse a metric or dimension, or toggle a value's
    /// checkbox (exclusive within its dimension). Returns `Some((name, kind))` when a metric was
    /// expanded for the first time and its dimensions must be fetched.
    fn toggle_node(&mut self) -> Option<(String, String)> {
        let mut to_load = None;
        match self.selected_tree_row()? {
            TreeRowRef::Metric(mi) => {
                let node = &mut self.metric_tree[mi];
                if !node.expanded && node.dims.is_none() && !node.loading {
                    node.loading = true;
                    node.expanded = true;
                    to_load = Some((node.name.clone(), node.kind.clone()));
                } else {
                    node.expanded = !node.expanded;
                }
            }
            TreeRowRef::Dim(mi, di) => {
                if let Some(dims) = self.metric_tree[mi].dims.as_mut() {
                    dims[di].expanded = !dims[di].expanded;
                }
            }
            TreeRowRef::Value(mi, di, vi) => {
                if let Some(dims) = self.metric_tree[mi].dims.as_mut() {
                    // Exclusive within the axis: checking a value replaces any other; checking the
                    // already-checked value clears it.
                    dims[di].selected = if dims[di].selected == Some(vi) {
                        None
                    } else {
                        Some(vi)
                    };
                }
            }
            TreeRowRef::NoDims(mi) => {
                self.metric_tree[mi].whole_selected = !self.metric_tree[mi].whole_selected;
            }
        }
        self.rebuild_catalog_table();
        to_load
    }

    /// Store freshly-discovered dimensions on the matching metric node.
    fn apply_metric_dims(&mut self, metric: &str, dims: Vec<DimNode>) {
        if let Some(node) = self.metric_tree.iter_mut().find(|n| n.name == metric) {
            node.dims = Some(dims);
            node.loading = false;
        }
        if self.on_catalog() {
            self.rebuild_catalog_table();
        }
    }

    /// The PromQL for metric `mi`, filtered by its checked values (one per axis) and optionally grouped
    /// by `group_by` (skipping that axis's matcher as redundant).
    fn metric_node_query(&self, mi: usize, group_by: Option<&str>) -> String {
        let node = &self.metric_tree[mi];
        let dims = node.dims.as_deref().unwrap_or(&[]);
        let matchers = dims
            .iter()
            .filter(|dim| Some(dim.label.as_str()) != group_by)
            .filter_map(|dim| {
                dim.selected
                    .and_then(|vi| dim.values.get(vi))
                    .map(|value| (dim.label.as_str(), value.as_str()))
            })
            .collect::<Vec<_>>();
        build_metric_query(&node.name, &node.kind, &matchers, group_by)
    }

    /// The queries to run when visualizing from the catalog. Checking any series (a dimension value)
    /// under a metric is itself the selection: every metric with at least one checked value is
    /// visualized together, each filtered by its own checked values. When nothing is checked anywhere,
    /// fall back to the single node under the cursor — grouped by the dimension on a `by …` row. Empty
    /// only when nothing is selectable.
    fn visualize_queries(&self) -> Vec<String> {
        let selected = self
            .metric_tree
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.whole_selected
                    || node
                        .dims
                        .as_deref()
                        .is_some_and(|dims| dims.iter().any(|dim| dim.selected.is_some()))
            })
            .map(|(mi, _)| self.metric_node_query(mi, None))
            .collect::<Vec<_>>();
        if !selected.is_empty() {
            return selected;
        }
        let Some(row) = self.selected_tree_row() else {
            return Vec::new();
        };
        let mi = match row {
            TreeRowRef::Metric(mi)
            | TreeRowRef::Dim(mi, _)
            | TreeRowRef::Value(mi, _, _)
            | TreeRowRef::NoDims(mi) => mi,
        };
        let group_by = match row {
            TreeRowRef::Dim(_, di) => self.metric_tree[mi]
                .dims
                .as_deref()
                .and_then(|dims| dims.get(di))
                .map(|dim| dim.label.clone()),
            _ => None,
        };
        vec![self.metric_node_query(mi, group_by.as_deref())]
    }

    /// The structured log record for the currently selected row on the Logs screen.
    fn selected_log_record(&self) -> Option<&LogRecord> {
        if self.screen() != Screen::Logs {
            return None;
        }
        let (first, last) = self.selectable_bounds()?;
        let index = self.selected.clamp(first, last) - first;
        self.snapshot.log_records.get(index)
    }

    /// Open the detailed time-series viewer for the currently selected series (the Metrics result
    /// table). Returns `false` (no-op) when there is no selectable series row (e.g. the catalog, or an
    /// empty result), so the caller records history only on a real navigation. The x-cursor starts at
    /// the latest point.
    fn open_metric_detail(&mut self) -> bool {
        if self.screen() != Screen::Metrics {
            return false;
        }
        let Some((first, last)) = self.selectable_bounds() else {
            return false;
        };
        let index = self.selected.clamp(first, last) - first;
        let Some(series) = self.snapshot.series.get(index) else {
            return false;
        };
        if series.points.is_empty() {
            return false;
        }
        self.metric_cursor = series.points.len() - 1;
        self.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: series.labels.clone(),
                query: self.active_query().to_owned(),
                points: series.points.clone(),
            },
        };
        true
    }

    /// After a fresh Traces result, move the cursor onto the focused trace (if it is in the result
    /// set) and clear the focus so the selection drives the waterfall again. When the focused trace is
    /// not listed, the focus is kept so `request_waterfall` still shows its waterfall.
    fn focus_select_trace(&mut self) {
        let Some(focus) = self.focus_trace_id.clone() else {
            return;
        };
        if self.screen() != Screen::Traces {
            return;
        }
        let Some((first, last)) = self.selectable_bounds() else {
            return;
        };
        for index in first..=last {
            if self.snapshot.lines[index].split_whitespace().next() == Some(focus.as_str()) {
                self.selected = index;
                self.focus_trace_id = None;
                return;
            }
        }
    }

    /// Recompute the completion popup for the identifier token at the end of the active query. Clears
    /// it outside edit mode, when the token is empty, or when the only match is the token itself.
    fn refresh_completion(&mut self) {
        if self.mode != Mode::Editing {
            self.completion = None;
            return;
        }
        let (context, token) = completion_context(self.active_query());
        let token = token.to_owned();
        // Expression position waits for at least one character before popping up (metric/function
        // lists are large); label-name/value position offers its (smaller) vocabulary immediately, so
        // an empty token right after `{` or `"` still lists everything eligible. The Logs screen is an
        // exception in expression position: its vocabulary is the short LogQL line-filter operator-hint
        // list, useful the moment the caret sits after a selector, so it pops even on an empty token.
        let suppress_empty = token.is_empty()
            && match context {
                CompletionContext::Suppressed => true,
                CompletionContext::Expr => self.screen() != Screen::Logs,
                _ => false,
            };
        if suppress_empty {
            self.completion = None;
            return;
        }
        let candidates = completion_candidates(
            self.screen(),
            &self.metric_names,
            &self.metric_tree,
            self.log_labels.as_deref().unwrap_or(&[]),
            &self.log_label_values,
            &context,
            &token,
        );
        // Nothing useful to offer if the sole candidate is exactly what's already typed.
        let redundant = matches!(candidates.as_slice(), [only] if only.text == token);
        self.completion = if candidates.is_empty() || redundant {
            None
        } else {
            let selected = self
                .completion
                .as_ref()
                .map(|c| c.selected.min(candidates.len() - 1))
                .unwrap_or(0);
            Some(Completion {
                candidates,
                selected,
            })
        };
    }

    /// When the caret is in a label position for a known-but-undiscovered metric, mark it loading and
    /// return `(name, kind)` so the caller can fetch its dimensions (the label vocabulary). Returns
    /// `None` once loaded/in-flight, or outside a label context, so it fires at most once per metric.
    fn completion_dim_request(&mut self) -> Option<(String, String)> {
        if self.mode != Mode::Editing || self.screen() != Screen::Metrics {
            return None;
        }
        let metric = match completion_context(self.active_query()).0 {
            CompletionContext::LabelName { metric }
            | CompletionContext::LabelValue { metric, .. } => metric?,
            _ => return None,
        };
        let node = self.metric_tree.iter_mut().find(|n| n.name == metric)?;
        if node.dims.is_none() && !node.loading {
            node.loading = true;
            Some((node.name.clone(), node.kind.clone()))
        } else {
            None
        }
    }

    /// When the caret sits in a `{…}` label position on the Logs screen and the corresponding
    /// vocabulary (label names, or a specific label's values) is not yet discovered, mark it in-flight
    /// and return the request so the caller can fetch it over the `Update` channel. Returns `None` once
    /// loaded/in-flight or outside a Logs label context, so each fetch fires at most once.
    fn completion_log_request(&mut self) -> Option<LogCompletionRequest> {
        if self.mode != Mode::Editing || self.screen() != Screen::Logs {
            return None;
        }
        match completion_context(self.active_query()).0 {
            CompletionContext::LabelName { .. } => {
                if self.log_labels.is_none() && !self.log_labels_loading {
                    self.log_labels_loading = true;
                    Some(LogCompletionRequest::Labels)
                } else {
                    None
                }
            }
            CompletionContext::LabelValue { label, .. } => {
                if !self.log_label_values.contains_key(&label)
                    && !self.log_label_values_loading.contains(&label)
                {
                    self.log_label_values_loading.insert(label.clone());
                    Some(LogCompletionRequest::Values(label))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Replace the token under the caret with the highlighted candidate, appending `(` for functions.
    fn accept_completion(&mut self) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let Some(candidate) = completion.candidates.get(completion.selected) else {
            return;
        };
        let token_len = completion_context(self.active_query()).1.len();
        let replacement = if candidate.kind == CandidateKind::Function {
            format!("{}(", candidate.text)
        } else {
            candidate.text.clone()
        };
        let query = self.active_query_mut();
        query.truncate(query.len() - token_len);
        query.push_str(&replacement);
        // The new trailing token (empty after a `(`, or the full name) may still have suggestions.
        self.refresh_completion();
    }
}

/// Best-effort restore of the terminal to its pre-`enter` state, writing directly to `out`.
///
/// Canonical ordering: first show the cursor and leave the alternate screen, then disable raw
/// mode, so the normal screen and cursor are back before the mode flip. Every step is best-effort
/// (a failing step must not skip the others), which is what makes this safe to call from any of the
/// three teardown sites — the `enter()` error paths, `Drop`, and the panic hook — and idempotent,
/// so running it more than once (panic path *and* `Drop`) is harmless.
fn restore_terminal<W: io::Write>(out: &mut W) {
    let _ = execute!(out, Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// One-shot claim used by `install_panic_hook`: returns `true` exactly once for a given flag, so
/// repeated `run` calls don't stack duplicate panic hooks. Extracted so the idempotency is unit
/// testable without mutating the process-global panic hook.
fn claim_panic_hook_install(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::SeqCst)
}

static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install a panic hook that restores the terminal *before* delegating to the previously-installed
/// hook, so a panic's message lands on the normal screen instead of staying hidden behind the
/// alternate screen. Installed at most once per process (guarded by `PANIC_HOOK_INSTALLED`) so
/// repeated `run` calls don't chain the hook onto itself.
///
/// The hook writes straight to `std::io::stdout()` rather than capturing a `Terminal`/stdout handle
/// a panic may have left in a poisoned state, and the restore is best-effort/idempotent so it does
/// not interfere with the normal `Drop`-based teardown on the non-panic path.
fn install_panic_hook() {
    if !claim_panic_hook_install(&PANIC_HOOK_INSTALLED) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore first (idempotent with `Drop`), then delegate so the message prints on the
        // now-restored normal screen.
        restore_terminal(&mut io::stdout());
        previous(info);
    }));
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            // The two commands run in sequence, so a failure may leave the alternate screen active
            // and/or the cursor hidden (e.g. `EnterAlternateScreen` succeeded but `Hide` failed).
            // Best-effort restore everything before returning so we never strand the terminal in
            // the alternate screen or with a hidden cursor.
            restore_terminal(&mut stdout);
            return Err(error);
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                // `stdout` was moved into the backend above, so restore via a fresh handle.
                restore_terminal(&mut io::stdout());
                return Err(error);
            }
        };
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal(self.terminal.backend_mut());
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(db: Arc<Db>, options: Options) -> io::Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    // Install the panic hook only after we have entered the alternate screen: if a panic unwinds
    // out of the event loop, the hook restores the normal screen (best-effort, idempotent with the
    // guard's `Drop`) before the default hook prints, so the panic message is actually visible.
    install_panic_hook();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let (key_sender, mut key_receiver) = mpsc::unbounded_channel::<KeyEvent>();

    // Terminal input is read on the blocking pool, not on the runtime thread. The event loop below is
    // driven by a single-threaded runtime (`main` is `flavor = "current_thread"`), so the query tasks
    // spawned by `request_refresh` only make progress while this loop is *awaiting*. Blocking on
    // `event::poll`/`event::read` here (as before) starved those tasks: the loop never yielded, so no
    // query result ever arrived and the UI stayed on "Loading..." forever. The reader forwards key
    // presses over a channel and the loop `select!`s over results, keys, and the refresh timer.
    let shutdown = Arc::new(AtomicBool::new(false));
    let reader = {
        let shutdown = Arc::clone(&shutdown);
        tokio::task::spawn_blocking(move || input_reader(&key_sender, &shutdown))
    };

    let sender: mpsc::UnboundedSender<Update> = sender;
    let mut app = App::new();
    // A host (or the CLI `--from/--to`) may seed an initial absolute window; adopt it so the first
    // query and the header indicator reflect it.
    app.abs_window = options.window;
    request_refresh(&mut app, db.clone(), options.clone(), sender.clone());

    let outcome: io::Result<()> = async {
        loop {
            // Drive the mascot from state deltas before drawing (the draw is `&App`, so the controller
            // must be advanced here where we hold `&mut app`). Only while it is actually visible.
            if app.show_mascot && app.mode == Mode::Normal && !options.ascii {
                let mut events: Vec<MascotEvent> = Vec::new();
                let tag = app.mascot_route_tag();
                if tag != app.mascot_route_tag {
                    app.mascot_route_tag = tag;
                    // Drop the previous view's chart geometry so the ride is evaluated only against the
                    // freshly-drawn chart — otherwise a detail→detail move could spend its one roll on
                    // the old chart's line and never try the new one. `draw` repopulates it this frame.
                    app.chart_geom.replace(None);
                    events.push(MascotEvent::Navigated {
                        on_metric_chart: matches!(app.route, Route::MetricDetail { .. }),
                    });
                }
                let idle_now = app.mascot_last_input.elapsed() >= MASCOT_IDLE_AFTER;
                if idle_now != app.mascot_idle {
                    app.mascot_idle = idle_now;
                    events.push(if idle_now {
                        MascotEvent::Idle
                    } else {
                        MascotEvent::Active
                    });
                }
                if app.mascot_refresh_pending {
                    app.mascot_refresh_pending = false;
                    events.push(MascotEvent::Refreshed {
                        auto: app.auto_refresh,
                    });
                }
                // The mascot floats over the whole screen; datapoint cells and the wander band are all
                // in absolute buffer coordinates, so give it the full terminal area.
                let area = terminal
                    .terminal
                    .size()
                    .map(|s| Rect::new(0, 0, s.width, s.height))
                    .unwrap_or_default();
                let ctx = MascotCtx {
                    area,
                    chart: app.chart_geom.borrow().clone(),
                };
                app.mascot.update(&events, &ctx);
            }

            terminal
                .terminal
                .draw(|frame| draw(frame, &app, &options))?;

            // Wake at least once a second so the header clock ticks live; a key or a query result
            // wakes us sooner. When auto-refresh is on we also cap the wait at the remaining interval
            // so the periodic refresh still fires on time. The refresh itself is gated on the elapsed
            // interval in the timer arm, so the 1s clock tick never triggers an early refresh.
            // Wake a few times a second only when the animated mascot is actually visible (shown, in
            // `Normal` mode, and not on an `--ascii` terminal) so it redraws smoothly — faster still
            // while a scripted motion (e.g. the chart ride) is running; otherwise the slower 1s tick
            // (enough for the header clock) keeps the loop mostly idle.
            let tick = if app.show_mascot && app.mode == Mode::Normal && !options.ascii {
                if app.mascot.is_busy() {
                    Duration::from_millis(100)
                } else {
                    Duration::from_millis(200)
                }
            } else {
                Duration::from_secs(1)
            };
            let until_refresh = if app.auto_refresh && app.mode == Mode::Normal {
                options
                    .refresh_interval
                    .saturating_sub(app.last_refresh.elapsed())
                    .min(tick)
            } else {
                tick
            };

            tokio::select! {
                Some(update) = receiver.recv() => {
                    match update {
                        Update::Query(result) => {
                            app.apply(result);
                            // A fresh result: arm the mascot's refresh-triggered easter egg.
                            app.mascot_refresh_pending = true;
                            if app.pending_refresh {
                                app.pending_refresh = false;
                                request_refresh(&mut app, db.clone(), options.clone(), sender.clone());
                            }
                            // A fresh traces result: land on the focused trace (if navigated from a
                            // log) then fetch the selected/focused trace's waterfall.
                            app.focus_select_trace();
                            request_waterfall(&mut app, &db, &sender, options.ascii);
                            // A fresh flat catalog: (re)build the metric tree over it.
                            if app.on_catalog() {
                                app.build_metric_tree();
                            }
                        }
                        Update::Vocabulary(names) => {
                            app.metric_names = names;
                            app.refresh_completion();
                        }
                        Update::MetricDims { metric, dims } => {
                            app.apply_metric_dims(&metric, dims);
                            // Freshly-arrived dimensions are the label-completion vocabulary; refresh
                            // the popup so it fills in if the caret is still in a label position.
                            app.refresh_completion();
                        }
                        Update::LogLabels(names) => {
                            app.log_labels = Some(names);
                            app.log_labels_loading = false;
                            // The label-name vocabulary just arrived; refill the popup if the caret is
                            // still in a Logs `{…}` label-name position.
                            app.refresh_completion();
                        }
                        Update::LogLabelValues { label, values } => {
                            app.log_label_values.insert(label.clone(), values);
                            app.log_label_values_loading.remove(&label);
                            // That label's values just arrived; refill the popup if the caret is still
                            // in the matching quoted-value position.
                            app.refresh_completion();
                        }
                        Update::Exemplars { labels, query, markers } => {
                            // Apply only if the metric detail the fetch was issued for is still shown.
                            if let Some(detail) = app.route_metric_detail()
                                && detail.labels == labels
                                && detail.query == query
                            {
                                app.metric_exemplars = markers;
                            }
                        }
                        Update::Waterfall { generation, trace_id, detail } => {
                            // Apply only if still current: same query generation and the selection has
                            // not moved to a different trace since the fetch was issued.
                            if generation == app.generation
                                && app.detail_trace_id.as_deref() == Some(trace_id.as_str())
                            {
                                app.snapshot.detail = Some(detail);
                            }
                        }
                    }
                }
                key = key_receiver.recv() => {
                    match key {
                        Some(key) => {
                            // Any keystroke marks the user active (drives the mascot idle/active split).
                            app.mascot_last_input = Instant::now();
                            if handle_key(&mut app, key, &db, &options, &sender) == Control::Quit {
                                break;
                            }
                        }
                        // The reader ended (terminal closed); nothing more to read, so exit.
                        None => break,
                    }
                }
                _ = tokio::time::sleep(until_refresh) => {
                    // Fire the auto-refresh only once the interval has actually elapsed; the shorter
                    // 1s wakes above exist solely to redraw the ticking clock.
                    if app.auto_refresh
                        && app.mode == Mode::Normal
                        && app.last_refresh.elapsed() >= options.refresh_interval
                    {
                        request_refresh(&mut app, db.clone(), options.clone(), sender.clone());
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    // Stop the input reader and join it before the terminal guard restores the screen on return.
    shutdown.store(true, Ordering::Relaxed);
    let _ = reader.await;
    outcome
}

/// Whether the event loop should keep running after a key press.
#[derive(PartialEq, Eq)]
enum Control {
    Continue,
    Quit,
}

/// Keys interpreted by the detail *routes* while in `Normal` mode. Returns `Some(Continue)` when the
/// key belongs to the detail (scrolling the log body, moving the chart cursor, or the trace jump) and
/// `None` to let the global handler take it (history nav, screen switch, menu, range) — this is what
/// makes the detail views non-modal. Not a detail route → always `None`.
fn handle_detail_key(
    app: &mut App,
    key: KeyEvent,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) -> Option<Control> {
    if app.route_log_record().is_some() {
        match key.code {
            // Enter is the explicit forward navigation to the trace viewer (when the log has a trace).
            KeyCode::Enter => {
                if let Some(trace_id) = app.route_log_record().and_then(|r| r.trace_id.clone()) {
                    app.push_history();
                    app.focus_trace_id = Some(trace_id);
                    switch_screen(
                        app,
                        Screen::Traces,
                        db.clone(),
                        options.clone(),
                        sender.clone(),
                    );
                }
            }
            // Scroll the (possibly long) detail body.
            KeyCode::Down => app.scroll = (app.scroll + 1).min(app.max_scroll.get()),
            KeyCode::Up => app.scroll = app.scroll.saturating_sub(1),
            KeyCode::PageDown => {
                app.scroll = app
                    .scroll
                    .saturating_add(app.page_rows.get())
                    .min(app.max_scroll.get());
            }
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(app.page_rows.get()),
            KeyCode::Home => app.scroll = 0,
            KeyCode::End => app.scroll = app.max_scroll.get(),
            _ => return None,
        }
        return Some(Control::Continue);
    }
    if let Some(last) = app
        .route_metric_detail()
        .map(|detail| detail.points.len().saturating_sub(1))
    {
        let page = app.page_rows.get().max(1) as usize;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // The chart x-cursor moves with h/l and Shift+←/Shift+→ (the bare arrows are history nav, so
            // Shift is the modal-free way to drive the cursor with the arrow keys); Home/End/PageUp/
            // PageDown jump. Up/Down are swallowed so they do not stir the underlying list selection.
            KeyCode::Char('h') => app.metric_cursor = app.metric_cursor.saturating_sub(1),
            KeyCode::Char('l') => app.metric_cursor = (app.metric_cursor + 1).min(last),
            KeyCode::Left if shift => app.metric_cursor = app.metric_cursor.saturating_sub(1),
            KeyCode::Right if shift => app.metric_cursor = (app.metric_cursor + 1).min(last),
            KeyCode::PageUp => app.metric_cursor = app.metric_cursor.saturating_sub(page),
            KeyCode::PageDown => app.metric_cursor = (app.metric_cursor + page).min(last),
            KeyCode::Home => app.metric_cursor = 0,
            KeyCode::End => app.metric_cursor = last,
            KeyCode::Up | KeyCode::Down => {}
            // Exemplar → trace drill-down: jump to the trace of the exemplar nearest the chart cursor.
            // With no exemplar in view, fall through (Enter is otherwise inert on a metric detail).
            KeyCode::Enter => {
                let trace_id = app.nearest_exemplar_trace()?;
                app.push_history();
                app.focus_trace_id = Some(trace_id);
                switch_screen(
                    app,
                    Screen::Traces,
                    db.clone(),
                    options.clone(),
                    sender.clone(),
                );
            }
            _ => return None,
        }
        return Some(Control::Continue);
    }
    None
}

/// Apply a single key press to the app, dispatching refreshes/screen switches as needed.
fn handle_key(
    app: &mut App,
    key: KeyEvent,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) -> Control {
    if key.kind != KeyEventKind::Press {
        return Control::Continue;
    }
    match app.mode {
        Mode::Editing => {
            match key.code {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    app.completion = None;
                }
                KeyCode::Enter => {
                    app.mode = Mode::Normal;
                    app.completion = None;
                    app.scroll = 0;
                    app.selected = 0;
                    // Explicitly running a Logs query supersedes any trace→log drill-down (the user is
                    // now driving the filter box), so the correlation is cleared.
                    if app.screen() == Screen::Logs {
                        app.log_correlation = None;
                    }
                    // Running the query moves the user's attention to the results, so focus lands there.
                    app.focus = Focus::Primary;
                    request_refresh(app, db.clone(), options.clone(), sender.clone());
                }
                // Tab accepts the highlighted completion; ↑/↓ move within the popup.
                KeyCode::Tab => app.accept_completion(),
                KeyCode::Down => {
                    if let Some(completion) = app.completion.as_mut() {
                        completion.selected =
                            (completion.selected + 1).min(completion.candidates.len() - 1);
                    }
                }
                KeyCode::Up => {
                    if let Some(completion) = app.completion.as_mut() {
                        completion.selected = completion.selected.saturating_sub(1);
                    }
                }
                KeyCode::Backspace => {
                    app.active_query_mut().pop();
                    app.refresh_completion();
                    maybe_discover_label_dims(app, db, options, sender);
                }
                KeyCode::Char(character) => {
                    app.active_query_mut().push(character);
                    app.refresh_completion();
                    maybe_discover_label_dims(app, db, options, sender);
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::TimeRange => {
            match key.code {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Up | KeyCode::Char('k') => {
                    app.range_cursor = app.range_cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    // The extra row past the presets is the "Absolute…" entry.
                    app.range_cursor = (app.range_cursor + 1).min(TIME_RANGES.len());
                }
                KeyCode::Enter => {
                    if app.range_cursor == TIME_RANGES.len() {
                        // "Absolute…": switch into the two-field datetime form.
                        app.open_absolute_form();
                    } else {
                        app.mode = Mode::Normal;
                        // Picking a preset returns to a rolling window; refresh if the effective window
                        // changed (a different preset, or leaving an absolute window).
                        let changed =
                            app.range_cursor != app.range_index || app.abs_window.is_some();
                        app.range_index = app.range_cursor;
                        app.abs_window = None;
                        if changed {
                            app.scroll = 0;
                            app.selected = 0;
                            request_refresh(app, db.clone(), options.clone(), sender.clone());
                        }
                    }
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::AbsoluteRange => {
            match key.code {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Tab => app.abs_field ^= 1,
                KeyCode::Up => app.abs_field = 0,
                KeyCode::Down => app.abs_field = 1,
                KeyCode::Backspace => {
                    if app.abs_field == 0 {
                        app.abs_start.pop();
                    } else {
                        app.abs_end.pop();
                    }
                }
                KeyCode::Char(character) => {
                    if app.abs_field == 0 {
                        app.abs_start.push(character);
                    } else {
                        app.abs_end.push(character);
                    }
                }
                // `commit_absolute` always runs (recording a parse error on failure); the guard only
                // gates the follow-up refresh on a successful commit.
                KeyCode::Enter if app.commit_absolute() => {
                    request_refresh(app, db.clone(), options.clone(), sender.clone());
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::Menu => {
            match key.code {
                KeyCode::Esc | KeyCode::F(9) => app.mode = Mode::Normal,
                KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => app.menu_move(-1),
                KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => app.menu_move(1),
                KeyCode::Enter => match app.menu_screen() {
                    // A screen item: switch to it and dismiss the menu.
                    Some(screen) => {
                        app.mode = Mode::Normal;
                        switch_screen_history(
                            app,
                            screen,
                            db.clone(),
                            options.clone(),
                            sender.clone(),
                        );
                    }
                    // The trailing range item: open the time-range dropdown (also parks the focus ring
                    // on the time selector, so closing the picker leaves it focused there).
                    None => {
                        open_time_range(app);
                    }
                },
                _ => {}
            }
            return Control::Continue;
        }
        Mode::Normal => {}
    }
    // Focus ring (Normal mode): Tab/Shift+Tab move the pane highlight; Enter activates the focused
    // pane. Only the TimeRange/Query stops act here — a Primary focus falls through untouched so the
    // detail routes and the per-route Enter arms behave exactly as before.
    match key.code {
        KeyCode::Tab => {
            app.focus_advance(1);
            return Control::Continue;
        }
        KeyCode::BackTab => {
            app.focus_advance(-1);
            return Control::Continue;
        }
        KeyCode::Enter => match app.effective_focus() {
            Focus::Menu(index) => {
                if let Some(&screen) = Screen::ORDER.get(index) {
                    switch_screen_history(app, screen, db.clone(), options.clone(), sender.clone());
                }
                return Control::Continue;
            }
            Focus::TimeRange => {
                open_time_range(app);
                return Control::Continue;
            }
            Focus::Query => {
                begin_editing(app, db, sender);
                return Control::Continue;
            }
            Focus::Primary => {}
        },
        // Left/Right select among the menu-bar items when the ring is on the bar, returning early so
        // they never fall through to history; on a content pane they instead drive Back/Forward below.
        KeyCode::Left if matches!(app.effective_focus(), Focus::Menu(_) | Focus::TimeRange) => {
            app.menubar_move(-1);
            return Control::Continue;
        }
        KeyCode::Right if matches!(app.effective_focus(), Focus::Menu(_) | Focus::TimeRange) => {
            app.menubar_move(1);
            return Control::Continue;
        }
        _ => {}
    }
    // Detail routes interpret a few keys of their own (scroll, chart cursor, trace jump); everything
    // else — history nav, screen switches, the menu, the range picker — falls through to the global
    // handling below, so the detail views are ordinary content, not modal.
    if let Some(control) = handle_detail_key(app, key, db, options, sender) {
        return control;
    }
    match key.code {
        KeyCode::Char('q') => return Control::Quit,
        // Left/Right are browser Back/Forward through the visited views; Esc is an alias for Back — but
        // only while focus is on a content pane. When the focus ring is on a menu-bar item, Left/Right
        // select among the items instead (handled above, returning early) and never reach history.
        // Forward navigation to a *new* view is always an explicit action (Enter, `t`, a screen key),
        // never `→` — so `→` only redoes a Back and never jumps somewhere unvisited.
        KeyCode::Left | KeyCode::Esc => {
            if app.go_back() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
                // Landing back on a metric detail refetches its exemplar markers (no-op otherwise).
                request_metric_exemplars(app, db, sender);
            }
        }
        KeyCode::Right => {
            if app.go_forward() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
                request_metric_exemplars(app, db, sender);
            }
        }
        // Logs list → log detail (Enter): open the detail for the selected entry.
        KeyCode::Enter if matches!(app.route, Route::Logs) => {
            if let Some(record) = app.selected_log_record().cloned() {
                app.push_history();
                app.route = Route::LogDetail { record };
                app.scroll = 0;
            }
        }
        // Space expands/collapses the selected metric or dimension in the catalog tree, lazily
        // fetching a metric's dimensions on first expand.
        KeyCode::Char(' ') if app.on_catalog() => {
            if let Some((name, kind)) = app.toggle_node() {
                // Discovery spans all metric data (picker-independent); only the series cap matters.
                request_metric_dims(name, kind, db.clone(), options.max_series, sender.clone());
            }
        }
        // Catalog → series list (Enter): build the matching PromQL and visualize it — every metric
        // with a checked series (else the node under the cursor: whole metric / group-by / filter).
        // Multiple queries are joined by newlines and run together (the executor has no `or`). The
        // catalog selection is preserved across a Back (`build_metric_tree` carries state by name).
        KeyCode::Enter if app.on_catalog() => {
            let queries = app.visualize_queries();
            if !queries.is_empty() {
                app.push_history();
                *app.active_query_mut() = queries.join("\n");
                app.selected = 0;
                app.scroll = 0;
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        // Series list → series detail (Enter): open the detailed time-series viewer for the selection.
        // Capture the list view first and only record it in history if a detail actually opens, so a
        // no-op Enter never disturbs the Forward stack.
        KeyCode::Enter if matches!(app.route, Route::Metrics) => {
            let departing = app.capture_nav();
            if app.open_metric_detail() {
                app.push_entry(departing);
                // Load the series' exemplar→trace markers for the just-opened detail.
                request_metric_exemplars(app, db, sender);
            }
        }
        // Time pan/zoom: `[`/`]` pan the window earlier/later by half its span; `-`/`+` (or `=`) zoom
        // out/in about the center. Each freezes the window to an absolute span (shown in the header)
        // and re-queries. No-ops (no window change) skip the refresh.
        KeyCode::Char('[') => {
            if app.pan_window(-0.5) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char(']') => {
            if app.pan_window(0.5) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('-') => {
            if app.zoom_window(2.0) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            if app.zoom_window(0.5) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        // Older/newer log paging (Logs list): `n` = older page, `p` = newer page. No-ops off the Logs
        // list or at the ends (`logs_page_*` guards the screen and the cursor stack).
        KeyCode::Char('n') => {
            if app.logs_page_older() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('p') => {
            if app.logs_page_newer() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        // Trace → logs drill-down: open the Logs screen filtered to the selected trace's records (the
        // symmetric partner of the log-detail Enter→trace jump). `L` (Shift+l) on the Traces list.
        KeyCode::Char('L') if app.screen() == Screen::Traces && !app.route.is_detail() => {
            if let Some(trace_id) = app.selected_trace_id() {
                app.push_history();
                app.log_correlation = Some(LogCorrelation {
                    trace_id,
                    span_id: None,
                });
                switch_screen(
                    app,
                    Screen::Logs,
                    db.clone(),
                    options.clone(),
                    sender.clone(),
                );
            }
        }
        KeyCode::Char('t') => open_time_range(app),
        KeyCode::Char('1') => switch_screen_history(
            app,
            Screen::Overview,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('2') => switch_screen_history(
            app,
            Screen::Metrics,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('3') => switch_screen_history(
            app,
            Screen::Traces,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('4') => switch_screen_history(
            app,
            Screen::Logs,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('e') if app.has_query() => begin_editing(app, db, sender),
        KeyCode::Char('r') => request_refresh(app, db.clone(), options.clone(), sender.clone()),
        // Shift+R (the uppercase char crossterm delivers) toggles background auto-refresh.
        KeyCode::Char('R') => app.auto_refresh = !app.auto_refresh,
        // Toggle the animated mascot (hidden by default). No effect on `--ascii` terminals, where its
        // block-glyph art is never drawn. Reset the motion clock so it does not lurch on first show.
        KeyCode::Char('m') => {
            app.show_mascot = !app.show_mascot;
            if app.show_mascot {
                app.mascot.last_tick = Instant::now();
            }
        }
        // F9 activates the menu bar (Midnight-Commander style); the numbered keys still jump to a
        // screen directly. The cursor/Tab only move between screens once the menu is active.
        KeyCode::F(9) => app.open_menu(),
        // ↑↓ / PageUp / PageDown / Home / End move the row cursor (traces/logs) or scroll the pane.
        KeyCode::Down => move_selection(app, 1),
        KeyCode::Up => move_selection(app, -1),
        KeyCode::PageDown => move_selection(app, app.page_rows.get() as isize),
        KeyCode::PageUp => move_selection(app, -(app.page_rows.get() as isize)),
        KeyCode::Home => {
            app.focus_trace_id = None;
            if let Some((first, _)) = app.selectable_bounds() {
                app.selected = first;
            } else {
                app.scroll = 0;
            }
        }
        KeyCode::End => {
            app.focus_trace_id = None;
            if let Some((_, last)) = app.selectable_bounds() {
                app.selected = last;
            } else {
                app.scroll = app.max_scroll.get();
            }
        }
        _ => {}
    }
    // If the row cursor moved to a different trace, refresh the waterfall pane (no-op otherwise).
    request_waterfall(app, db, sender, options.ascii);
    Control::Continue
}

/// Move the row cursor by `delta` rows when the primary pane is a navigable list (traces/logs),
/// otherwise scroll the plain pane by the same amount. Moving the cursor releases any log→trace
/// focus so the waterfall follows the selection again.
fn move_selection(app: &mut App, delta: isize) {
    app.focus_trace_id = None;
    if let Some((first, last)) = app.selectable_bounds() {
        let current = app.selected.clamp(first, last) as isize;
        app.selected = (current + delta).clamp(first as isize, last as isize) as usize;
    } else if delta >= 0 {
        app.scroll = app
            .scroll
            .saturating_add(delta as u16)
            .min(app.max_scroll.get());
    } else {
        app.scroll = app.scroll.saturating_sub(delta.unsigned_abs() as u16);
    }
}

/// Blocking terminal-input reader, run on the blocking pool so it never stalls the runtime. Polls
/// with a timeout and re-checks `shutdown` each iteration so it exits promptly on quit rather than
/// parking forever in a blocking read.
fn input_reader(keys: &mpsc::UnboundedSender<KeyEvent>, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => {
                    if keys.send(key).is_err() {
                        break; // the event loop dropped the receiver
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    }
}

/// Open the time-range dropdown, seeding the cursor from the current window and moving focus to the
/// menu-bar time selector. Shared by the `t` key and a focus-`Enter` on the time selector.
fn open_time_range(app: &mut App) {
    app.range_cursor = if app.abs_window.is_some() {
        TIME_RANGES.len()
    } else {
        app.range_index
    };
    app.focus = Focus::TimeRange;
    app.mode = Mode::TimeRange;
}

/// Enter query-editing mode, fetching the completion vocabulary on first use and moving focus to the
/// query pane. Shared by the `e` key and a focus-`Enter` on the query pane; the caller guarantees the
/// current view has a query pane.
fn begin_editing(app: &mut App, db: &Arc<Db>, sender: &mpsc::UnboundedSender<Update>) {
    app.mode = Mode::Editing;
    app.focus = Focus::Query;
    if app.screen() == Screen::Metrics && app.metric_names.is_empty() {
        request_vocabulary(app.screen(), db.clone(), sender.clone());
    }
    app.refresh_completion();
}

/// Switch screens as a browser navigation: record the departed view in history first (unless it is
/// the same screen, which is not a navigation). Used by the numbered keys and the menu.
fn switch_screen_history(
    app: &mut App,
    screen: Screen,
    db: Arc<Db>,
    options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    if app.screen() != screen {
        app.push_history();
    }
    switch_screen(app, screen, db, options, sender);
}

fn switch_screen(
    app: &mut App,
    screen: Screen,
    db: Arc<Db>,
    options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    app.route = Route::list(screen);
    app.scroll = 0;
    app.selected = 0;
    app.completion = None;
    app.focus = Focus::Primary;
    // A log→trace jump sets `focus_trace_id` just before switching to Traces; drop a stale focus when
    // switching anywhere else.
    if screen != Screen::Traces {
        app.focus_trace_id = None;
    }
    // A trace→log drill-down sets `log_correlation` just before switching to Logs; drop it when leaving
    // Logs so an unrelated Logs visit is not silently correlated. Exemplar markers belong to a metric
    // detail, so any list switch clears them.
    if screen != Screen::Logs {
        app.log_correlation = None;
    }
    app.metric_exemplars.clear();
    if screen == Screen::Metrics && app.metric_names.is_empty() {
        request_vocabulary(screen, db.clone(), sender.clone());
    }
    app.snapshot = Snapshot::message(screen.title(), "Loading...");
    request_refresh(app, db, options, sender);
}

fn request_refresh(
    app: &mut App,
    db: Arc<Db>,
    mut options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    // Drive the effective window from the interactively selected time range rather than the static
    // launch defaults.
    options.lookback = app.lookback();
    options.step = app.step();
    options.window = app.abs_window;
    if app.loading {
        // Keep `log_paging` intact so the coalesced refresh below still sees the paging intent.
        app.pending_refresh = true;
        return;
    }
    // Log paging is coherent only against a fixed query/window (offset cursors shift otherwise), so any
    // refresh that is *not* an explicit older/newer page move drops back to page 0. The paging keys set
    // `log_paging` to carry the cursor stack across this one refresh.
    if app.log_paging {
        app.log_paging = false;
    } else {
        app.log_cursor_stack.clear();
        app.log_next_cursor = None;
    }
    app.loading = true;
    app.generation = app.generation.wrapping_add(1);
    let generation = app.generation;
    let screen = app.screen();
    let query = app.active_query().to_owned();
    let after = app.log_cursor_stack.last().copied();
    let correlation = app.log_correlation.clone();
    tokio::spawn(async move {
        let result = load_snapshot(db, screen, &query, &options, after, correlation).await;
        let _ = sender.send(Update::Query(QueryResult {
            generation,
            screen,
            result,
        }));
    });
}

/// The `[min, max]` timestamp span across all metric tables, from `db.stats()`. Falls back to a wide
/// window ending at `now` if no metric data has a recorded span. Makes catalog dimension discovery
/// independent of the selected time range.
async fn metric_time_span(db: &Arc<Db>) -> (i64, i64) {
    const WIDE_NS: i64 = 3_600_000_000_000 * 24 * 365 * 30; // ~30 years
    let now = Timestamp::now().0;
    let fallback = (now.saturating_sub(WIDE_NS), now);
    let Ok(stats) = db.stats().await else {
        return fallback;
    };
    let is_metric = |table: DbTable| {
        matches!(
            table,
            DbTable::MetricsGauge
                | DbTable::MetricsSum
                | DbTable::MetricsHistogram
                | DbTable::MetricsExpHistogram
                | DbTable::MetricsSummary
        )
    };
    let min = stats
        .tables
        .iter()
        .filter(|t| is_metric(t.table))
        .filter_map(|t| t.min_time_unix_nano)
        .min();
    let max = stats
        .tables
        .iter()
        .filter(|t| is_metric(t.table))
        .filter_map(|t| t.max_time_unix_nano)
        .max();
    match (min, max) {
        (Some(min), Some(max)) => (min, max),
        _ => fallback,
    }
}

/// Discover a metric's groupable dimensions by evaluating its bare selector as an instant over the
/// metric's whole retained span (picker-independent), collecting the label keys/values from the
/// returned series (labels include the resource `service` and data-point attributes; `__name__`/`le`
/// are internal and excluded). Empty on any failure.
async fn discover_dims(db: &Arc<Db>, name: &str, kind: &str, max_series: usize) -> Vec<DimNode> {
    let (span_start, span_end) = metric_time_span(db).await;
    // One instant just past the last sample, looking back across the whole span.
    let at = span_end.saturating_add(1);
    let eval_range = EvalRange {
        start_ns: at,
        end_ns: at,
        step_ns: 1,
        lookback_ns: (at.saturating_sub(span_start).max(1) as u128).min(u64::MAX as u128) as u64,
    };
    let limits = EvalLimits {
        max_series,
        ..EvalLimits::default()
    };
    let Ok(catalog) = db.metrics().catalog().await else {
        return Vec::new();
    };
    let context = metric_context(&catalog);
    let query = discovery_promql(name, kind);
    let Ok(translated) = translate_promql(&query, &context) else {
        return Vec::new();
    };
    let ImbhQueryModel::Prom(expression) = translated.model else {
        return Vec::new();
    };
    let Ok(series) = db
        .metrics()
        .execute_promql(&expression, eval_range, limits)
        .await
    else {
        return Vec::new();
    };
    let mut by_label: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in &series {
        for (key, value) in item.labels.iter() {
            let (key, value) = (key.to_string(), value.to_string());
            if key == "__name__" || key == "le" {
                continue;
            }
            by_label.entry(key).or_default().insert(value);
        }
    }
    by_label
        .into_iter()
        .map(|(label, values)| DimNode {
            label,
            values: values.into_iter().collect(),
            expanded: false,
            selected: None,
        })
        .collect()
}

/// Fetch a metric's dimensions off the event-loop thread and deliver them as `Update::MetricDims`.
/// Discovery is time-range independent, so only the series cap is threaded through.
fn request_metric_dims(
    name: String,
    kind: String,
    db: Arc<Db>,
    max_series: usize,
    sender: mpsc::UnboundedSender<Update>,
) {
    tokio::spawn(async move {
        let dims = discover_dims(&db, &name, &kind, max_series).await;
        let _ = sender.send(Update::MetricDims { metric: name, dims });
    });
}

/// If the completion caret sits in a label position for a metric whose dimensions (the label
/// vocabulary) are not yet discovered, kick off that discovery so the popup can fill in on arrival
/// (`Update::MetricDims` re-runs `refresh_completion`). Fires at most once per metric.
fn maybe_discover_label_dims(
    app: &mut App,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) {
    if let Some((name, kind)) = app.completion_dim_request() {
        request_metric_dims(name, kind, db.clone(), options.max_series, sender.clone());
    }
    // The Logs screen's `{…}` selector draws its vocabulary from cross-signal attribute discovery
    // rather than a per-metric tree, so it has its own (analogous) fetch path.
    match app.completion_log_request() {
        Some(LogCompletionRequest::Labels) => request_log_labels(db.clone(), sender.clone()),
        Some(LogCompletionRequest::Values(label)) => {
            request_log_label_values(label, db.clone(), sender.clone())
        }
        None => {}
    }
}

/// Fetch the log label names (cross-signal attribute keys) off the event-loop thread and deliver them
/// as `Update::LogLabels`. This is the Logs `{…}` selector's label-name completion vocabulary.
fn request_log_labels(db: Arc<Db>, sender: mpsc::UnboundedSender<Update>) {
    tokio::spawn(async move {
        let names = db.attrs().names().await.unwrap_or_default();
        let _ = sender.send(Update::LogLabels(names));
    });
}

/// Fetch one log label's distinct values off the event-loop thread and deliver them as
/// `Update::LogLabelValues`. This is the Logs quoted-matcher label-value completion vocabulary.
fn request_log_label_values(label: String, db: Arc<Db>, sender: mpsc::UnboundedSender<Update>) {
    tokio::spawn(async move {
        let values = db.attrs().values(&label).await.unwrap_or_default();
        let _ = sender.send(Update::LogLabelValues { label, values });
    });
}

/// Fetch the completion vocabulary for a screen off the event-loop thread. Only the Metrics screen
/// has a dynamic vocabulary (metric names from the catalog); other screens complete against static
/// function/keyword lists and need no fetch.
fn request_vocabulary(screen: Screen, db: Arc<Db>, sender: mpsc::UnboundedSender<Update>) {
    if screen != Screen::Metrics {
        return;
    }
    tokio::spawn(async move {
        if let Ok(catalog) = db.metrics().catalog().await {
            let mut names = catalog
                .iter()
                .map(|metric| metric.metric.clone())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            let _ = sender.send(Update::Vocabulary(names));
        }
    });
}

/// If the selected trace differs from the one shown in the waterfall pane, fetch its waterfall off
/// the event-loop thread and deliver it as an `Update::Waterfall`. No-op on non-Traces screens or
/// when the selected trace is already shown/in flight.
fn request_waterfall(
    app: &mut App,
    db: &Arc<Db>,
    sender: &mpsc::UnboundedSender<Update>,
    ascii: bool,
) {
    // A pending log→trace focus wins over the row selection until the cursor moves or the focused
    // trace is found in the list.
    let Some(trace_id) = app
        .focus_trace_id
        .clone()
        .or_else(|| app.selected_trace_id())
    else {
        return;
    };
    if app.detail_trace_id.as_deref() == Some(trace_id.as_str()) {
        return;
    }
    app.detail_trace_id = Some(trace_id.clone());
    let generation = app.generation;
    let db = db.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        let detail = build_waterfall_detail(&db, &trace_id, ascii).await;
        let _ = sender.send(Update::Waterfall {
            generation,
            trace_id,
            detail,
        });
    });
}

/// Fetch the exemplar→trace markers for the open metric-detail view (the metric's exemplars carrying a
/// trace id, within the plotted window) off the event-loop thread. Clears any previous markers first;
/// a no-op off a metric detail or when the metric name cannot be determined.
fn request_metric_exemplars(app: &mut App, db: &Arc<Db>, sender: &mpsc::UnboundedSender<Update>) {
    app.metric_exemplars.clear();
    let Some(detail) = app.route_metric_detail() else {
        return;
    };
    let Some(name) = metric_name_from_detail(detail) else {
        return;
    };
    let (win_start, win_end) = match (detail.points.first(), detail.points.last()) {
        (Some(first), Some(last)) => (first.0.min(last.0), first.0.max(last.0)),
        _ => return,
    };
    let labels = detail.labels.clone();
    let query = detail.query.clone();
    let db = db.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        let markers: Vec<ExemplarMarker> = match db.metrics().exemplars(&name).await {
            Ok(exemplars) => exemplars
                .into_iter()
                .filter_map(|exemplar| {
                    let trace = exemplar.trace_id?;
                    let time_ns = exemplar.time.0;
                    (win_start..=win_end)
                        .contains(&time_ns)
                        .then(|| ExemplarMarker {
                            time_ns,
                            trace_id: trace.to_hex(),
                        })
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        if !markers.is_empty() {
            let _ = sender.send(Update::Exemplars {
                labels,
                query,
                markers,
            });
        }
    });
}

/// Best-effort OTLP metric name for an exemplar lookup: the series' `__name__` label when present (a
/// bare selector keeps it), else the first non-function identifier in the PromQL query (covers
/// `rate(name[..])`/`sum(name)` where PromQL drops `__name__`).
fn metric_name_from_detail(detail: &MetricDetail) -> Option<String> {
    for pair in detail.labels.split(',') {
        if let Some(value) = pair.strip_prefix("__name__=")
            && !value.is_empty()
        {
            return Some(value.to_owned());
        }
    }
    metric_ident_from_promql(&detail.query)
}

/// PromQL words that are never a metric selector: aggregation operators and the boolean/set keywords.
/// Encountering one means "keep scanning" — the metric name is elsewhere (e.g. inside `rate(…)`).
const PROMQL_RESERVED: &[&str] = &[
    "sum",
    "avg",
    "min",
    "max",
    "count",
    "count_values",
    "stddev",
    "stdvar",
    "group",
    "topk",
    "bottomk",
    "quantile",
    "and",
    "or",
    "unless",
    "bool",
    "offset",
    "atan2",
];
/// Grouping modifiers whose following `(labels…)` list must be skipped whole, so a grouping *label* is
/// never mistaken for the metric name.
const PROMQL_GROUPING: &[&str] = &[
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
];

/// The metric selector in a PromQL string: the first identifier that is not an aggregation
/// operator/keyword, not a grouping label, and not a function call — e.g. `name_bucket` inside
/// `histogram_quantile(0.95, sum by (le) (rate(name_bucket[5m])))`. Best-effort; `None` if none found.
fn metric_ident_from_promql(query: &str) -> Option<String> {
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_ascii_alphabetic() || ch == '_' || ch == ':' {
            let start = i;
            while i < bytes.len() && is_ident_char(bytes[i] as char) {
                i += 1;
            }
            let ident = &query[start..i];
            // Peek past whitespace to see whether a call `(` or grouping list follows.
            let mut j = i;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            let next_paren = bytes.get(j).copied() == Some(b'(');
            if PROMQL_GROUPING.contains(&ident) {
                if next_paren {
                    i = skip_paren_group(bytes, j);
                }
                continue;
            }
            if PROMQL_RESERVED.contains(&ident) || next_paren {
                continue; // a keyword or a function call — the selector is further in
            }
            return Some(ident.to_owned());
        }
        i += 1;
    }
    None
}

/// Return the index just past the `)` matching the `(` at `open`; the end of the slice if unbalanced.
fn skip_paren_group(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Fetch a single trace by hex id and render its waterfall into a detail pane, degrading to a short
/// message on a missing/invalid id or a source error.
async fn build_waterfall_detail(db: &Arc<Db>, trace_id_hex: &str, ascii: bool) -> DetailPane {
    // Success carries a structured `Waterfall` so the bars reflow to the pane width at draw time;
    // the miss/error branches only have a message, delivered through `lines`.
    let (lines, waterfall) = match TraceId::from_hex(trace_id_hex) {
        Some(trace_id) => match db.traces().get(trace_id).await {
            Ok(Some(trace)) => (Vec::new(), Some(build_waterfall(&trace, ascii))),
            Ok(None) => (vec!["trace not found.".to_owned()], None),
            Err(error) => (vec![format!("error: {error}")], None),
        },
        None => (vec!["invalid trace id.".to_owned()], None),
    };
    DetailPane {
        title: format!("Waterfall: {trace_id_hex}"),
        lines,
        waterfall,
    }
}

/// How many progressively narrower windows to try after the full window overflows the trace cap.
const TRACE_NARROW_STEPS: usize = 6;

/// Candidate window starts to try, most-recent-first, after the full `[start, end)` window overflows
/// the trace cap: each halves the span measured back from `end_ns`, so the searched window shrinks
/// toward the present. Returns only the *narrowed* starts — the caller tries the full `start_ns`
/// first — and never reaches `end_ns` (that would be an empty window).
fn narrowing_starts(start_ns: i64, end_ns: i64, steps: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut span = end_ns.saturating_sub(start_ns).max(0);
    for _ in 0..steps {
        span /= 2;
        if span <= 0 {
            break;
        }
        out.push(end_ns.saturating_sub(span));
    }
    out
}

/// Execute a TraceQL query, transparently narrowing the time window toward `end` whenever the trace
/// cap is hit. Returns the matches together with the window start actually used (equal to `start`
/// when no narrowing was needed). A failed attempt costs only the candidate `search` (the cap is
/// checked before complete traces are fetched), so the retries are cheap.
async fn execute_traceql_adaptive(
    db: &Arc<Db>,
    expression: &SpansetExpr,
    start: i64,
    end: i64,
    limits: EvalLimits,
) -> Result<(Vec<TraceQueryMatch>, i64), SemanticError> {
    let mut starts = Vec::with_capacity(TRACE_NARROW_STEPS + 1);
    starts.push(start);
    starts.extend(narrowing_starts(start, end, TRACE_NARROW_STEPS));
    let mut last = SemanticError::LimitExceeded("TraceQL source traces");
    for candidate_start in starts {
        match db
            .traces()
            .execute_traceql(expression, FetchBounds::new(candidate_start, end)?, limits)
            .await
        {
            Ok(matches) => return Ok((matches, candidate_start)),
            Err(error) if matches!(error, SemanticError::LimitExceeded(_)) => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Turn the residual trace-limit error (the window could not be narrowed enough) into actionable
/// guidance; pass other semantic errors through unchanged.
fn trace_limit_message(error: &SemanticError, cap: usize) -> String {
    if matches!(error, SemanticError::LimitExceeded(_)) {
        format!(
            "too many traces even in the most recent sub-window (cap {cap}). Add filters (e.g. \
             status=error, duration>Nms) or pick a shorter time range."
        )
    } else {
        error.to_string()
    }
}

/// The evaluation window `(start_ns, end_ns, range, limits)` for `now - lookback .. now` at the given
/// step and caps. Shared by the panel query and the catalog dimension-discovery task.
fn eval_window(options: &Options) -> (i64, i64, EvalRange, EvalLimits) {
    // An absolute window is a fixed span with a step derived to keep the sample count bounded; a
    // rolling window is `now - lookback .. now` at the preset step.
    let (start, end, step_ns) = match options.window {
        Some((start, end)) => {
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            // ~120 points across the span, whole seconds, at least 1s.
            let span_secs = ((end.saturating_sub(start)).max(0) / 1_000_000_000) as u64;
            let step_secs = (span_secs / 120).max(1);
            (start, end, step_secs.saturating_mul(1_000_000_000))
        }
        None => {
            let end = Timestamp::now().0;
            let start =
                end.saturating_sub(options.lookback.as_nanos().min(i64::MAX as u128) as i64);
            let step_ns = options.step.as_nanos().min(u64::MAX as u128) as u64;
            (start, end, step_ns)
        }
    };
    let eval_range = EvalRange {
        start_ns: start,
        end_ns: end,
        step_ns: step_ns.max(1),
        lookback_ns: 300_000_000_000,
    };
    let span_secs = (end.saturating_sub(start)).max(0) as u64 / 1_000_000_000;
    let step_secs = (step_ns / 1_000_000_000).max(1);
    let limits = EvalLimits {
        max_series: options.max_series,
        max_samples: options
            .max_series
            .saturating_mul(
                usize::try_from(span_secs / step_secs)
                    .unwrap_or(1)
                    .saturating_add(2),
            )
            .max(1),
        max_traces: options.max_rows,
        ..EvalLimits::default()
    };
    (start, end, eval_range, limits)
}

async fn load_snapshot(
    db: Arc<Db>,
    screen: Screen,
    query: &str,
    options: &Options,
    // Older/newer paging cursor for the Logs list (the older page's [`imbh::LogPage::next`], or `None`
    // for page 0). Ignored off the Logs screen.
    after: Option<PageCursor>,
    // Trace/span correlation filter layered onto the Logs query for a trace→log drill-down. Ignored
    // off the Logs screen.
    correlation: Option<LogCorrelation>,
) -> Result<Snapshot, String> {
    let (start, end, eval_range, limits) = eval_window(options);
    // Chrome glyphs woven into snapshot text (titles, the truncation warning) follow `--ascii` too.
    let g = Glyphs::new(options.ascii);
    match screen {
        Screen::Overview => {
            let stats = db.stats().await.map_err(|error| error.to_string())?;
            let mut lines = vec![
                format!("buffer: {} bytes", stats.buffer_bytes),
                format!("WAL: {} bytes", stats.wal_bytes),
                format!("ingest queue: {}", stats.ingest_queue_depth),
            ];
            lines.extend(stats.tables.into_iter().map(|table| {
                format!(
                    "{:<24} rows={}+{} segments={}",
                    table.table.as_str(),
                    table.segment_rows,
                    table.buffer_rows,
                    table.segment_count
                )
            }));
            Ok(Snapshot {
                title: "Database overview".to_owned(),
                lines,
                chart: Vec::new(),
                detail: None,
                list_from: None,
                log_records: Vec::new(),
                table: None,
                series: Vec::new(),
                next_cursor: None,
            })
        }
        Screen::Metrics => {
            let catalog = db
                .metrics()
                .catalog()
                .await
                .map_err(|error| error.to_string())?;
            if query.trim().is_empty() {
                let rows = catalog
                    .iter()
                    .map(|metric| {
                        vec![
                            metric.metric.clone(),
                            metric.kind.clone(),
                            metric.unit.clone(),
                            metric.temporality.clone().unwrap_or_else(|| "-".to_owned()),
                        ]
                    })
                    .collect::<Vec<_>>();
                return Ok(Snapshot {
                    title: format!(
                        "Metric catalog {d} {} metrics (Space: expand/select series {s} Enter: visualize selected {s} e: PromQL)",
                        rows.len(),
                        d = g.dash,
                        s = g.sep,
                    ),
                    lines: Vec::new(),
                    chart: Vec::new(),
                    detail: None,
                    list_from: None,
                    log_records: Vec::new(),
                    table: Some(TableData {
                        header: vec![
                            "Metric".to_owned(),
                            "Kind".to_owned(),
                            "Unit".to_owned(),
                            "Temporality".to_owned(),
                        ],
                        rows,
                    }),
                    series: Vec::new(),
                    next_cursor: None,
                });
            }
            let context = metric_context(&catalog);
            // One or more newline-separated PromQL queries (the catalog joins several when multiple
            // metrics are checked; the executor has no `or`, so each runs on its own). Their result
            // series are concatenated — each keeps its `__name__` label, so they stay distinguishable.
            let sub_queries = query
                .split('\n')
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .collect::<Vec<_>>();
            let mut series = Vec::new();
            for sub in &sub_queries {
                let translated =
                    translate_promql(sub, &context).map_err(|diagnostic| diagnostic.message)?;
                let ImbhQueryModel::Prom(expression) = translated.model else {
                    return Err("translator returned a non-metric model".to_owned());
                };
                let mut result = db
                    .metrics()
                    .execute_promql(&expression, eval_range, limits)
                    .await
                    .map_err(|error| error.to_string())?;
                series.append(&mut result);
            }
            // Build the summary rows and, in the same pass, retain each series' full
            // `(timestamp_ns, value)` history so the detailed viewer can plot the selected one.
            let mut rows = Vec::with_capacity(series.len());
            let mut series_data = Vec::with_capacity(series.len());
            for item in &series {
                let labels = if item.labels.iter().next().is_none() {
                    "{}".to_owned()
                } else {
                    item.labels
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let values = item
                    .samples
                    .iter()
                    .map(|sample| sample.value)
                    .filter(|value| value.is_finite())
                    .collect::<Vec<_>>();
                let latest = item.samples.last().map_or(f64::NAN, |sample| sample.value);
                let (min, max) = if values.is_empty() {
                    (f64::NAN, f64::NAN)
                } else {
                    (
                        values.iter().copied().fold(f64::INFINITY, f64::min),
                        values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    )
                };
                rows.push(vec![
                    labels.clone(),
                    format_metric_value(latest),
                    format_metric_value(min),
                    format_metric_value(max),
                    item.samples.len().to_string(),
                ]);
                series_data.push(SeriesData {
                    labels,
                    points: item
                        .samples
                        .iter()
                        .map(|sample| (sample.timestamp_ns, sample.value))
                        .collect(),
                });
            }
            // Title shows the single query, or a metric count when several are combined.
            let title_query = if sub_queries.len() > 1 {
                format!("{} metrics", sub_queries.len())
            } else {
                sub_queries.first().copied().unwrap_or(query).to_owned()
            };
            Ok(Snapshot {
                title: format!(
                    "PromQL: {title_query} {} {} series (Enter: view series)",
                    g.dash,
                    rows.len()
                ),
                chart: chart_values(
                    series
                        .first()
                        .map_or(&[][..], |series| series.samples.as_slice())
                        .iter()
                        .map(|sample| sample.value),
                ),
                lines: Vec::new(),
                detail: None,
                list_from: None,
                log_records: Vec::new(),
                table: Some(TableData {
                    header: vec![
                        "Series".to_owned(),
                        "Latest".to_owned(),
                        "Min".to_owned(),
                        "Max".to_owned(),
                        "Points".to_owned(),
                    ],
                    rows,
                }),
                series: series_data,
                next_cursor: None,
            })
        }
        Screen::Traces => {
            let translated = translate_traceql(query, &TranslateContext::default())
                .map_err(|diagnostic| diagnostic.message)?;
            let ImbhQueryModel::Trace(expression) = translated.model else {
                return Err("translator returned a non-trace model".to_owned());
            };
            // The trace cap is applied to candidate traces in the time *window*, before the TraceQL
            // predicate runs, so a busy window overflows however selective the query is. Rather than
            // dead-end on "TraceQL source traces limit exceeded", focus on the most recent sub-window
            // that fits and say so loudly.
            let (matches, effective_start) =
                execute_traceql_adaptive(&db, &expression, start, end, limits)
                    .await
                    .map_err(|error| trace_limit_message(&error, limits.max_traces))?;
            let narrowed = effective_start > start;
            let mut lines = Vec::new();
            if narrowed {
                let window_secs = ((end - effective_start).max(0) / 1_000_000_000) as u64;
                lines.push(format!(
                    "{} full range had more than {} traces {} showing the most recent {} ({} .. {}).",
                    g.warn,
                    limits.max_traces,
                    g.dash,
                    humanize_secs(window_secs.max(1)),
                    format_timestamp_ns(effective_start),
                    format_timestamp_ns(end),
                ));
                lines.push(
                    "  add filters (e.g. status=error, duration>Nms) or shorten the time range to \
                     search the whole window."
                        .to_owned(),
                );
                lines.push(String::new());
            }
            lines.push(format!("{} matching traces", matches.len()));
            // Rows below the header count are the selectable trace entries.
            let list_from = lines.len();
            // The trace id stays the leading whitespace-delimited token (`selected_trace_id` /
            // `focus_select_trace` parse it), so the trace's start time is appended after it.
            lines.extend(matches.into_iter().map(|item| {
                format!(
                    "{}  {}  selected={}",
                    item.trace_id,
                    format_timestamp_ns(item.start_time_ns),
                    item.spanset.selected_span_ids.join(",")
                )
            }));
            // The waterfall is fetched on demand for the selected trace (`request_waterfall`); ship a
            // placeholder so the split layout is stable until it arrives.
            let has_rows = list_from < lines.len();
            let detail = Some(DetailPane {
                title: "Waterfall".to_owned(),
                lines: vec![if has_rows {
                    "Loading waterfall...".to_owned()
                } else {
                    "No trace selected.".to_owned()
                }],
                waterfall: None,
            });
            Ok(Snapshot {
                title: if narrowed {
                    format!("TraceQL (narrowed): {query}")
                } else {
                    format!("TraceQL: {query}")
                },
                chart: Vec::new(),
                lines,
                detail,
                list_from: Some(list_from),
                log_records: Vec::new(),
                table: None,
                series: Vec::new(),
                next_cursor: None,
            })
        }
        Screen::Logs => {
            let schema = LogStreamSchema::service_only();
            // The box accepts either a bare LogQL selector (`{service="api"} |? "timeout"`), which
            // filters the list, or a range-aggregation metric expression (`rate({}[5m])`), which also
            // drives the sparkline. Both forms yield a `LogFilter` that filters the displayed list; an
            // empty box means "all logs". `|?`/`!?` (imbh dialect) push down to the Tantivy `.tidx`.
            let (filter, range_expr) = if query.trim().is_empty() {
                (LogFilter::All, None)
            } else {
                let translated = translate_logql(query, &TranslateContext::default())
                    .map_err(|diagnostic| diagnostic.message)?;
                match translated.model {
                    ImbhQueryModel::LogSelector(filter) => (filter, None),
                    ImbhQueryModel::Log(expression) => {
                        (expression.filter.clone(), Some(expression))
                    }
                    _ => return Err("translator returned a non-log model".to_owned()),
                }
            };
            // Filter the list through the shared `LogFilter` → native `LogQuery` bridge, then restore
            // the viewer's most-recent-first ordering and exact page size (the bridge defaults to
            // ascending + one-over for its own paging).
            let bounds = FetchBounds::new(start, end).map_err(|error| error.to_string())?;
            let request = LogFetchRequest {
                bounds,
                filter: filter.clone(),
                max_entries: options.max_rows,
            };
            let mut list_query = build_log_query(&request, &schema)
                .map_err(|error| error.to_string())?
                .direction(imbh::Direction::Backward)
                .limit(options.max_rows);
            // Layer a trace→log drill-down correlation (raw-binary id equality) onto the query. A
            // malformed hex id is ignored rather than failing the whole panel.
            if let Some(correlation) = &correlation {
                if let Some(trace) = TraceId::from_hex(&correlation.trace_id) {
                    list_query = list_query.trace_id(trace);
                }
                if let Some(span) = correlation.span_id.as_deref().and_then(SpanId::from_hex) {
                    list_query = list_query.span_id(span);
                }
            }
            // Older/newer paging: resume past the previous pages' rows. The volume sparkline below is
            // unpaged (it covers the whole window), so it is built from an unpaged clone taken first.
            let volume_query = list_query.clone();
            if let Some(cursor) = after {
                list_query = list_query.after(cursor);
            }
            let page = db
                .logs()
                .query(list_query.clone())
                .await
                .map_err(|error| error.to_string())?;
            let page_next = page.next;
            // The sparkline: the synthesized metric for a range expression, else the log volume of the
            // filtered set over the same window.
            let chart = match &range_expr {
                Some(expression) => {
                    let derived = db
                        .logs()
                        .execute_logql(expression, eval_range, limits, &schema)
                        .await
                        .map_err(|error| error.to_string())?;
                    chart_values(
                        derived
                            .first()
                            .map_or(&[][..], |series| series.samples.as_slice())
                            .iter()
                            .map(|sample| sample.value),
                    )
                }
                None => {
                    let step = Duration::from_nanos(eval_range.step_ns.max(1));
                    let buckets = db
                        .logs()
                        .volume(volume_query, step)
                        .await
                        .map_err(|error| error.to_string())?;
                    chart_values(buckets.iter().map(|bucket| bucket.count as f64))
                }
            };
            let mut lines = vec![format!(
                "viewer rows={} scanned={} bytes={} index={}",
                page.entries.len(),
                page.stats.rows_scanned,
                page.stats.bytes_scanned,
                page.stats.used_index
            )];
            // Rows below the stat header are the selectable log entries. Each row shows a short trace
            // id (or `--------` when absent) so the log↔trace linkage is visible in the list; the full
            // record is kept in `log_records` for the detail view and trace-id navigation.
            let list_from = lines.len();
            let mut log_records = Vec::with_capacity(page.entries.len());
            for entry in page.entries {
                let trace_id = entry.trace_id.map(|id| id.to_hex());
                let short_trace = trace_id.as_deref().map_or_else(
                    || "--------".to_owned(),
                    |hex| hex[..hex.len().min(8)].to_owned(),
                );
                lines.push(format!(
                    "{} {} {:<8} {}",
                    format_timestamp_ns(entry.time.0),
                    short_trace,
                    entry.service.as_deref().unwrap_or("-"),
                    entry.body.replace('\n', " ")
                ));
                log_records.push(LogRecord {
                    time_ns: entry.time.0,
                    severity: entry
                        .severity_text
                        .clone()
                        .unwrap_or_else(|| severity_label(entry.severity_number)),
                    service: entry.service.clone(),
                    body: entry.body.clone(),
                    trace_id,
                    span_id: entry.span_id.map(|id| id.to_hex()),
                    attributes: attrs_to_pairs(&entry.attributes),
                    resource: attrs_to_pairs(&entry.resource),
                    scope: attrs_to_pairs(&entry.scope),
                });
            }
            // The title reflects any active trace→log drill-down and whether an older page is shown; the
            // `n`/`p` keys page older/newer (see `handle_key`).
            let paged = if after.is_some() { " [older]" } else { "" };
            let title = match (&correlation, range_expr.is_some()) {
                (Some(correlation), _) => {
                    let short = &correlation.trace_id[..correlation.trace_id.len().min(8)];
                    let span = correlation
                        .span_id
                        .as_deref()
                        .map(|id| format!(" span {}", &id[..id.len().min(8)]))
                        .unwrap_or_default();
                    format!("Logs for trace {short}{span} {} n/p: page{paged}", g.dash)
                }
                (None, true) => format!("Log search + synthesized metric: {query}{paged}"),
                (None, false) => format!("Log search: {query}{paged}"),
            };
            Ok(Snapshot {
                title,
                chart,
                lines,
                detail: None,
                list_from: Some(list_from),
                log_records,
                table: None,
                series: Vec::new(),
                next_cursor: page_next,
            })
        }
    }
}

/// Reduce a trace to width-independent waterfall rows (see [`WaterfallRow`]). The bars are stored as
/// fractions of the trace duration; [`render_waterfall`] paints them onto the pane's actual width.
fn build_waterfall(trace: &imbh::Trace, ascii: bool) -> Waterfall {
    let parents = trace
        .spans
        .iter()
        .map(|span| {
            (
                span.span_id.to_hex(),
                span.parent_span_id.map(|parent| parent.to_hex()),
            )
        })
        .collect::<HashMap<_, _>>();
    let duration = trace.duration_ns.0.max(1) as f64;
    let rows = trace
        .spans
        .iter()
        .map(|span| {
            let id = span.span_id.to_hex();
            let mut parent = span.parent_span_id.map(|parent| parent.to_hex());
            let mut seen = HashSet::from([id.clone()]);
            let mut depth = 0usize;
            let mut malformed = false;
            while let Some(parent_id) = parent {
                if !seen.insert(parent_id.clone()) {
                    malformed = true;
                    break;
                }
                let Some(next) = parents.get(&parent_id) else {
                    malformed = true;
                    break;
                };
                depth = depth.saturating_add(1).min(16);
                parent = next.clone();
            }
            let relative = span.start_time.0.saturating_sub(trace.start_time.0).max(0) as f64;
            // The bar's position and length as fractions of the trace duration — resolution-free, so
            // the same row renders correctly at any pane width.
            let start = (relative / duration).clamp(0.0, 1.0);
            let frac = (span.duration_ns.0 as f64 / duration).clamp(0.0, 1.0);
            // Fold the depth indent into the name and clamp the pair to WATERFALL_NAME_W (char-aware,
            // with an ellipsis) so the fixed-width prefix keeps every bar starting at the same column.
            let label = clamp_field(
                &format!("{}{}", "  ".repeat(depth), span.name),
                WATERFALL_NAME_W,
            );
            WaterfallRow {
                prefix: format!("{}{label}", if malformed { "!" } else { " " }),
                start,
                frac,
                suffix: format!(
                    "{:>8.3}ms {}",
                    span.duration_ns.0 as f64 / 1_000_000.0,
                    span.status_code
                ),
            }
        })
        .collect();
    Waterfall {
        rows,
        marker: if ascii { '#' } else { '━' },
    }
}

/// Paint a [`Waterfall`] into text lines whose bars span exactly `bar_cells` cells, so both the
/// opening and closing `|` line up in a column and the bars stretch to fill the pane.
fn render_waterfall(waterfall: &Waterfall, bar_cells: usize) -> Vec<String> {
    let cells = bar_cells.max(1);
    waterfall
        .rows
        .iter()
        .map(|row| {
            let start = ((row.start * cells as f64) as usize).min(cells - 1);
            let width = ((row.frac * cells as f64).round() as usize)
                .max(1)
                .min(cells - start);
            let mut bar = String::with_capacity(cells);
            bar.extend(std::iter::repeat_n(' ', start));
            bar.extend(std::iter::repeat_n(waterfall.marker, width));
            bar.extend(std::iter::repeat_n(' ', cells - start - width));
            format!("{}|{}|{}", row.prefix, bar, row.suffix)
        })
        .collect()
}

/// Left-align `text` into a field that is exactly `width` *terminal cells* wide, honoring East Asian
/// width: wide glyphs (CJK, etc.) count as two cells, so the field pads/truncates by display width
/// rather than by `char` count. Short strings are space-padded; long ones are truncated with a
/// trailing `…`. If a wide glyph would straddle the boundary, an extra space keeps the total exact.
/// Used to keep the waterfall's name column a constant width so the `|bar|` axis stays aligned.
fn clamp_field(text: &str, width: usize) -> String {
    let total = UnicodeWidthStr::width(text);
    if total <= width {
        let mut out = String::from(text);
        out.extend(std::iter::repeat_n(' ', width - total));
        return out;
    }
    // Truncate to leave one cell for the ellipsis, stopping before a glyph would overflow.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    // A wide glyph landing on an odd boundary can leave the field one cell short; pad it out.
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(used + 1)));
    out
}

/// Render checked `(label, value)` matchers as a PromQL selector suffix (`{a="1",b="2"}`, or `""`).
fn matcher_braces(matchers: &[(&str, &str)]) -> String {
    if matchers.is_empty() {
        return String::new();
    }
    let inner = matchers
        .iter()
        .map(|(label, value)| format!("{label}=\"{value}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{inner}}}")
}

/// Build the PromQL to visualize a metric of the given OTel kind, restricted to `matchers` and
/// optionally aggregated by `group_by`: gauges plot as-is (avg when grouped), cumulative sums as a
/// per-second rate, histograms as a p95 over the bucket rate (aggregated by `le`).
fn build_metric_query(
    name: &str,
    kind: &str,
    matchers: &[(&str, &str)],
    group_by: Option<&str>,
) -> String {
    let braces = matcher_braces(matchers);
    match (kind, group_by) {
        ("histogram", Some(label)) => format!(
            "histogram_quantile(0.95, sum by ({label}, le) (rate({name}_bucket{braces}[5m])))"
        ),
        ("histogram", None) => {
            format!("histogram_quantile(0.95, sum by (le) (rate({name}_bucket{braces}[5m])))")
        }
        ("sum", Some(label)) => format!("sum by ({label}) (rate({name}{braces}[5m]))"),
        ("sum", None) => format!("rate({name}{braces}[5m])"),
        (_, Some(label)) => format!("avg by ({label}) ({name}{braces})"),
        (_, None) => format!("{name}{braces}"),
    }
}

/// The bare selector used to *discover* a metric's groupable dimensions: evaluated as an instant over
/// the metric's whole retained span, its returned series carry the full label set (data-point
/// attributes plus the resource `service`), which we read to build the tree. A plain selector (not a
/// rate) avoids depending on samples landing in a rate window.
fn discovery_promql(name: &str, kind: &str) -> String {
    match kind {
        "histogram" => format!("{name}_bucket"),
        _ => name.to_owned(),
    }
}

fn metric_context(catalog: &[imbh::MetricMeta]) -> TranslateContext {
    let mut metrics = Vec::new();
    for metric in catalog {
        let kind = match (metric.kind.as_str(), metric.temporality.as_deref()) {
            ("gauge", _) => Some(MetricKind::Gauge),
            ("sum", Some(temporality)) if temporality.eq_ignore_ascii_case("cumulative") => {
                Some(MetricKind::CumulativeCounter)
            }
            ("histogram", Some(temporality)) if temporality.eq_ignore_ascii_case("cumulative") => {
                Some(MetricKind::CumulativeHistogram)
            }
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        metrics.push(MetricResolution {
            query_name: metric.metric.clone(),
            storage_name: metric.metric.clone(),
            kind,
        });
        if kind == MetricKind::CumulativeHistogram {
            metrics.push(MetricResolution {
                query_name: format!("{}_bucket", metric.metric),
                storage_name: metric.metric.clone(),
                kind,
            });
        }
    }
    TranslateContext { metrics }
}

/// Compact display of a metric value: integers without a fractional part, non-integers to 4 dp, and
/// explicit `NaN`/`+Inf`/`-Inf` rather than Rust's default `inf`.
fn format_metric_value(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "+Inf" } else { "-Inf" }.to_owned()
    } else if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.4}")
    }
}

fn chart_values(values: impl Iterator<Item = f64>) -> Vec<u64> {
    let values = values.filter(|value| value.is_finite()).collect::<Vec<_>>();
    let maximum = values
        .iter()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);
    values
        .into_iter()
        .map(|value| ((value.max(0.0) / maximum) * 1_000.0) as u64)
        .collect()
}

/// Border style for a pane: a bold cyan outline when it holds the focus ring, the default (dim) border
/// otherwise. Applied via `Block::border_style` so the focused pane reads at a glance.
fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// Overlay the animated mascot ("Atta") at the [`Mascot`] controller's current position within `area`.
/// A small borderless overlay floating above the content (its cells are cleared first), whose facing
/// picks the art pair and whose waddle phase picks the frame. Position, facing, and phase are advanced
/// each redraw by [`Mascot::update`] in [`run`]; here we only blit. The caller gates visibility (hidden
/// by default, toggled with `m`), and the block-glyph art means it is only drawn on non-`--ascii`
/// terminals.
fn draw_mascot(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let body = app.mascot.body;
    let art = mascot_art(body.facing, mascot_phase(body.phase_ns));
    let width = art
        .iter()
        .map(|row| UnicodeWidthStr::width(*row) as u16)
        .max()
        .unwrap_or(0);
    // Skip on a terminal too small to host the overlay without colliding with the chrome.
    if width == 0
        || area.width < width + 1
        || area.height < MASCOT_ART_HEIGHT + MASCOT_BOTTOM_MARGIN + 1
    {
        return;
    }
    // Clamp the controller's (possibly sub-cell, possibly out-of-band during a ride) position to a cell
    // that keeps the whole overlay on screen.
    let max_x = area.right().saturating_sub(width);
    let max_y = area.bottom().saturating_sub(MASCOT_ART_HEIGHT);
    let x = (body.x.round() as i64).clamp(area.left() as i64, max_x as i64) as u16;
    let y = (body.y.round() as i64).clamp(area.top() as i64, max_y as i64) as u16;
    let overlay = Rect {
        x,
        y,
        width,
        height: MASCOT_ART_HEIGHT,
    };
    let lines: Vec<Line> = art
        .iter()
        .map(|row| {
            Line::from(Span::styled(
                (*row).to_owned(),
                Style::default().fg(Color::Magenta),
            ))
        })
        .collect();
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(lines), overlay);
}

/// Pure-ASCII box-drawing set for `--ascii` mode: `+` corners, `-`/`|` edges. Applied to every
/// bordered block so `--ascii` emits no Unicode line-drawing glyphs.
const ASCII_BORDER: ratatui::symbols::border::Set = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// The chrome glyphs (borders, header icons, hint separators, arrows) the UI draws, swapped to pure
/// ASCII under `--ascii` so the whole interface emits no Unicode. Content (log bodies, labels, values)
/// is never rewritten — only the UI's own decoration. Constructed once per render from [`Options::ascii`].
struct Glyphs {
    ascii: bool,
    logo: &'static str,
    clock: &'static str,
    warn: &'static str,
    dash: &'static str,
    sep: &'static str,
    up: &'static str,
    down: &'static str,
    left: &'static str,
    right: &'static str,
    ellipsis: &'static str,
    vline: &'static str,
}

impl Glyphs {
    fn new(ascii: bool) -> Self {
        if ascii {
            Self {
                ascii,
                logo: "*",
                clock: "",
                warn: "!",
                dash: "-",
                sep: "|",
                up: "^",
                down: "v",
                left: "<",
                right: ">",
                ellipsis: "...",
                vline: "|",
            }
        } else {
            Self {
                ascii,
                logo: "⬤",
                clock: "⏲",
                warn: "⚠",
                dash: "—",
                sep: "·",
                up: "↑",
                down: "↓",
                left: "←",
                right: "→",
                ellipsis: "…",
                vline: "│",
            }
        }
    }

    /// A `Block` with `Borders::ALL`, using the ASCII border set in `--ascii` mode (the default Unicode
    /// set otherwise). All bordered panels route through here so the border style follows the mode.
    fn block(&self) -> Block<'static> {
        let block = Block::default().borders(Borders::ALL);
        if self.ascii {
            block.border_set(ASCII_BORDER)
        } else {
            block
        }
    }

    /// The two-glyph vertical scroll indicator (`↑↓` / `^v`).
    fn scroll(&self) -> String {
        format!("{}{}", self.up, self.down)
    }
}

/// The degraded render for a terminal smaller than [`MIN_COLS`]×[`MIN_ROWS`]: a centered prompt telling
/// the user the required size and the current one. Pure ASCII (no chrome glyphs) so it paints even in
/// the most constrained state, and never overflows the tiny area.
fn draw_too_small(frame: &mut ratatui::Frame<'_>, area: Rect, _ascii: bool) {
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from("Terminal too small"),
        Line::from(format!("Resize to at least {MIN_COLS}x{MIN_ROWS}")),
        Line::from(format!("(now {}x{})", area.width, area.height)),
    ];
    let paragraph = Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });
    // Vertically center the 3-line message when there is room to spare.
    let top = area.height.saturating_sub(3) / 2;
    let inner = Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: area.height.saturating_sub(top),
    };
    frame.render_widget(paragraph, inner);
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App, options: &Options) {
    let area = frame.area();
    // Below the minimum, the paned layout can't be laid out meaningfully (borders overlap, geometry
    // underflows). Show a clear resize prompt instead — the first-release small-terminal criterion
    // (TUI_PLAN.md §10). No chart geometry is published, so the mascot ride has nothing stale to read.
    if area.width < MIN_COLS || area.height < MIN_ROWS {
        app.chart_geom.replace(None);
        draw_too_small(frame, area, options.ascii);
        return;
    }
    // The chart geometry the mascot rides is only valid on the metric diagram; drop it otherwise so a
    // stale line never lingers after navigating away. `draw_metric_detail` repopulates it when shown.
    if !matches!(app.route, Route::MetricDetail { .. }) {
        app.chart_geom.replace(None);
    }
    let g = Glyphs::new(options.ascii);
    // The menu bar is always the top line; the content below it depends on the route (the detail
    // views render as ordinary content there, not as full-screen modal takeovers).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let nav_area = outer[0];
    let content_area = outer[1];
    // Midnight-Commander-style one-line header: a brand + screen menu on the left, the time-range and
    // live-clock selector on the right, on a single coloured bar. The range portion is the on-screen
    // anchor the time-range dropdown drops down from.
    let menu_active = app.mode == Mode::Menu;
    // The focus ring's current stop (a stale Query focus already snapped to Primary); drives the pane
    // highlight, the time selector, and — for the menu items — the whole-bar recolour below.
    let focus = app.effective_focus();
    // Cyan is the focus colour (it also draws the focused-pane borders). The whole bar turns cyan the
    // moment the focus ring lands on any menu-bar item — a screen item, the time selector, the open
    // time-range picker, or the F9 menu — so the user notices focus has moved up here. When focus is
    // elsewhere the bar reverts to the readiness colour: a calm blue once the last query has landed,
    // muted grey while one is in flight.
    let menubar_focused = menu_active
        || app.mode == Mode::TimeRange
        || app.mode == Mode::AbsoluteRange
        || matches!(focus, Focus::Menu(_) | Focus::TimeRange);
    let bar_bg = if menubar_focused {
        Color::Cyan
    } else if app.loading {
        Color::DarkGray
    } else {
        Color::Blue
    };
    let bar = Style::default().bg(bar_bg).fg(Color::Black);
    // Two distinct in-bar chips, so the active screen and the focus position never look alike: a solid
    // light-filled chip marks the *active* screen ("you are here"), while a dark chip with cyan text
    // marks the *focus-ring cursor* ("focus is on this item"). Cyan stays reserved for the focus cursor.
    let active_chip = Style::default().bg(Color::Gray).fg(Color::Black);
    let cursor_chip = Style::default()
        .bg(Color::Black)
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    // The black circle is the black hole imbh is named for; it also marks the brand as a logo rather
    // than a selectable menu item. U+2B24 (not the visually similar U+25CF) is East-Asian-width
    // *unambiguous* (one cell everywhere), so it never desynchronizes the header's width math.
    let mut left: Vec<Span<'static>> = vec![
        // Plain black (no BOLD): bold + black renders as bright-black/grey on many terminals, which
        // washes out on the blue bar — so the brand keeps the same true black as the logo circle.
        Span::styled(" IMBH ", bar),
        Span::styled(format!("{} ", g.logo), bar),
    ];
    for (index, (screen, label)) in [
        (Screen::Overview, "1 Overview"),
        (Screen::Metrics, "2 Metrics"),
        (Screen::Traces, "3 Traces"),
        (Screen::Logs, "4 Logs"),
    ]
    .into_iter()
    .enumerate()
    {
        // The focus-ring cursor (the F9 menu cursor, or the Tab focus parked on this item) takes the
        // cursor chip; the current screen keeps its active chip. Both can show at once on different
        // items, and stay distinct when they coincide (the cursor wins).
        let is_cursor = if menu_active {
            app.menu_cursor == index
        } else {
            matches!(focus, Focus::Menu(focused_index) if focused_index == index)
        };
        let is_active = screen == app.screen();
        let style = if is_cursor {
            cursor_chip
        } else if is_active {
            active_chip
        } else {
            bar
        };
        left.push(Span::styled(format!(" {label} "), style));
        left.push(Span::styled(" ", bar));
    }
    let clock = format_datetime_ns(Timestamp::now().0);
    // Auto-refresh only makes sense for a relative window (an absolute one never moves), so the `!`
    // flag rides right after the range text and only when the window is relative.
    let auto = if app.auto_refresh && app.abs_window.is_none() {
        "!"
    } else {
        ""
    };
    // A timer-clock icon (U+23F2, EAW-unambiguous) prefixes the wall clock; dropped in `--ascii`.
    let clock_icon = if g.clock.is_empty() {
        String::new()
    } else {
        format!("{} ", g.clock)
    };
    let range_text = format!(
        " {}{}  {}{} ",
        app.range_summary(&g),
        auto,
        clock_icon,
        clock
    );
    // The time selector is a focus-ring stop but never an "active screen", so it only ever takes the
    // cursor chip — when the menu cursor is on it, the ring is parked on it, or its dropdown/form is
    // open — otherwise the plain bar.
    let range_focused = if menu_active {
        app.menu_cursor == MENU_LEN - 1
    } else {
        app.mode == Mode::TimeRange || app.mode == Mode::AbsoluteRange || focus == Focus::TimeRange
    };
    let range_style = if range_focused { cursor_chip } else { bar };
    let span_width = |spans: &[Span]| -> usize {
        spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    };
    let left_width = span_width(&left);
    let right_width = UnicodeWidthStr::width(range_text.as_str());
    // Push the range selector to the right edge; a filler of bar-coloured spaces spans the gap.
    let pad = (nav_area.width as usize).saturating_sub(left_width + right_width);
    let mut spans = left;
    spans.push(Span::styled(" ".repeat(pad), bar));
    spans.push(Span::styled(range_text, range_style));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), nav_area);
    // The range selector's on-screen rect (right-aligned on the one-line header) anchors the dropdown.
    let indicator_x = (left_width + pad).min(nav_area.width as usize) as u16;
    let indicator_area = Rect {
        x: nav_area.x + indicator_x,
        y: nav_area.y,
        width: (right_width as u16).min(nav_area.width.saturating_sub(indicator_x)),
        height: 1,
    };

    // Detail routes own the whole content area (their own header/body/hint, no query pane); the
    // range/menu overlays still apply on top since they are global.
    if let Some(record) = app.route_log_record() {
        draw_log_detail(
            frame,
            app,
            record,
            content_area,
            focus == Focus::Primary,
            &g,
        );
        draw_global_overlays(frame, app, indicator_area, area, options.ascii);
        return;
    }
    if let Some(detail) = app.route_metric_detail() {
        draw_metric_detail(
            frame,
            app,
            detail,
            options,
            content_area,
            focus == Focus::Primary,
        );
        draw_global_overlays(frame, app, indicator_area, area, options.ascii);
        return;
    }

    // List views: query pane (except Overview) + main + status, within the content area.
    let has_query = app.screen() != Screen::Overview;
    let rows = if has_query {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(content_area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(2)])
            .split(content_area)
    };
    let (query_area, main_area, status_area) = if has_query {
        (Some(rows[0]), rows[1], rows[2])
    } else {
        (None, rows[0], rows[1])
    };

    if let Some(query_area) = query_area {
        let mut spans = highlight_query(app.screen(), app.active_query(), &g);
        if app.mode == Mode::Editing {
            // A block caret; the global cursor stays hidden so this marks the edit point.
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        }
        let query_title = if app.mode == Mode::Editing {
            format!(
                "Query (Enter: run {s} Tab: complete {s} Esc: cancel)",
                s = g.sep
            )
        } else {
            "Query (e: edit)".to_owned()
        };
        frame.render_widget(
            Paragraph::new(Line::from(spans)).block(
                g.block()
                    .border_style(focus_border(focus == Focus::Query))
                    .title(query_title),
            ),
            query_area,
        );
    }

    let main = if app.snapshot.chart.is_empty() || main_area.height < 9 {
        vec![main_area]
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(6), Constraint::Min(3)])
            .split(main_area)
            .to_vec()
    };
    if !app.snapshot.chart.is_empty() && main.len() == 2 {
        if options.ascii {
            let chart = ascii_chart(
                &app.snapshot.chart,
                main[0].width.saturating_sub(2) as usize,
                main[0].height.saturating_sub(2) as usize,
            );
            frame.render_widget(
                Paragraph::new(chart).block(g.block().title("Series")),
                main[0],
            );
        } else {
            frame.render_widget(
                Sparkline::default()
                    .block(g.block().title("Series"))
                    .data(&app.snapshot.chart),
                main[0],
            );
        }
    }
    let list_area = *main.last().expect("at least one main area");
    // A snapshot with a detail pane (the Traces waterfall) splits the results region vertically:
    // the primary list on top, the detail below. Both keep full width — waterfall bars are wide.
    let (primary_area, detail) = match &app.snapshot.detail {
        Some(detail) => {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(list_area);
            (parts[0], Some((parts[1], detail)))
        }
        None => (list_area, None),
    };

    let viewport = primary_area.height.saturating_sub(2);
    app.page_rows.set(viewport.max(1));
    let primary_focused = focus == Focus::Primary;
    if let Some(table) = &app.snapshot.table {
        draw_metric_table(frame, app, table, primary_area, primary_focused, &g);
    } else if let Some(from) = app.snapshot.list_from {
        // Cursor-navigable list: header lines (< from) are dimmed and unselectable; the selected row
        // is highlighted and the List widget scrolls to keep it in view.
        let selection = app.selectable_bounds().map(|(first, last)| {
            let selected = app.selected.clamp(first, last);
            (selected, first, last)
        });
        let items = app
            .snapshot
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let style = if index < from {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(line.clone(), style))
            })
            .collect::<Vec<_>>();
        let title = match selection {
            Some((selected, first, last)) => format!(
                "{}  [{}/{}]",
                app.snapshot.title,
                selected - first + 1,
                last - first + 1
            ),
            None => app.snapshot.title.clone(),
        };
        let mut state = ListState::default();
        state.select(selection.map(|(selected, ..)| selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    g.block()
                        .border_style(focus_border(primary_focused))
                        .title(title),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            primary_area,
            &mut state,
        );
    } else {
        // Plain scrolled view. Publish the scroll bounds derived from this frame's geometry so the key
        // handler can clamp. `inner_width` subtracts the block's borders.
        let text = if app.snapshot.lines.is_empty() {
            "No data".to_owned()
        } else {
            app.snapshot.lines.join("\n")
        };
        let inner_width = primary_area.width.saturating_sub(2);
        let total_rows: u16 = app
            .snapshot
            .lines
            .iter()
            .map(|line| wrapped_rows(line, inner_width))
            .sum::<u32>()
            .min(u16::MAX as u32) as u16;
        let max_scroll = total_rows.saturating_sub(viewport);
        app.max_scroll.set(max_scroll);
        let scroll = app.scroll.min(max_scroll);
        let list_title = if max_scroll > 0 {
            format!(
                "{}  [{}/{} {}]",
                app.snapshot.title,
                scroll,
                max_scroll,
                g.scroll()
            )
        } else {
            app.snapshot.title.clone()
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(
                    g.block()
                        .border_style(focus_border(primary_focused))
                        .title(list_title),
                ),
            primary_area,
        );
    }

    if let Some((detail_area, detail)) = detail {
        let detail_text = if let Some(waterfall) = &detail.waterfall {
            // The pane has no side borders, so the full width is usable text. Give the bar every cell
            // left after the fixed prefix (marker + name), the two `|`, and the trailing duration
            // column, so the bars stretch to fill the pane instead of a fixed 40 cells.
            let bar_cells = (detail_area.width as usize)
                .saturating_sub(1 + WATERFALL_NAME_W + 2 + WATERFALL_SUFFIX_W)
                .max(1);
            render_waterfall(waterfall, bar_cells).join("\n")
        } else if detail.lines.is_empty() {
            "No data".to_owned()
        } else {
            detail.lines.join("\n")
        };
        frame.render_widget(
            Paragraph::new(detail_text)
                .wrap(Wrap { trim: false })
                // No border box on the waterfall pane: a bare title line keeps the trace id visible
                // while freeing the left/right/bottom edge cells so the bars sit flush against them.
                .block(Block::default().title(detail.title.as_str())),
            detail_area,
        );
    }

    let status = if let Some(error) = &app.last_error {
        format!("error: {error}")
    } else if app.mode == Mode::Menu {
        format!(
            "menu | {l}{r}/tab move {s} enter select {s} esc close",
            l = g.left,
            r = g.right,
            s = g.sep
        )
    } else {
        // Readiness rides the menu-bar colour, the range/auto-refresh state ride the header, so the
        // footer is purely the key legend now.
        let sep = g.sep;
        let detail_hint = match app.screen() {
            Screen::Logs => format!(" {sep} enter detail"),
            Screen::Metrics if app.active_query().trim().is_empty() => {
                format!(" {sep} space expand/select series {sep} enter visualize")
            }
            Screen::Metrics => format!(" {sep} enter series detail {sep} {}/esc back", g.left),
            _ => String::new(),
        };
        // The mascot toggle is only advertised on terminals that can render it.
        let mascot_hint = if options.ascii {
            String::new()
        } else {
            format!(" {sep} m mascot")
        };
        format!(
            "q quit {sep} F9 menu {sep} 1-4 screen {sep} tab focus {sep} {l}{r} back/fwd {sep} {scroll} move{detail_hint} {sep} r refresh {sep} R auto-refresh {sep} t range {sep} e edit{mascot_hint}{ascii}",
            l = g.left,
            r = g.right,
            scroll = g.scroll(),
            ascii = if options.ascii {
                format!(" {sep} ASCII")
            } else {
                String::new()
            },
        )
    };
    frame.render_widget(
        Paragraph::new(status).wrap(Wrap { trim: true }),
        status_area,
    );

    // Overlays render last so they sit above the panels.
    draw_global_overlays(frame, app, indicator_area, area, options.ascii);
    if app.mode == Mode::Editing
        && let Some(query_area) = query_area
        && let Some(completion) = app.completion.as_ref()
    {
        draw_completion_popup(frame, completion, query_area, area, &g);
    }
}

/// The route-independent overlays: the time-range dropdown and the absolute-range form, both anchored
/// to the menu bar's range selector, so they can appear over any route (including the detail views).
fn draw_global_overlays(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    indicator_area: Rect,
    area: Rect,
    ascii: bool,
) {
    let g = Glyphs::new(ascii);
    // The mascot is opt-in (toggle `m`) and never shown on `--ascii` terminals since its art is block
    // glyphs. It floats above the content but below the pickers, so a dropdown/form is never hidden.
    if app.show_mascot && !ascii {
        draw_mascot(frame, app, area);
    }
    if app.mode == Mode::TimeRange {
        draw_time_range_picker(frame, app, indicator_area, area, &g);
    }
    if app.mode == Mode::AbsoluteRange {
        draw_absolute_range(frame, app, indicator_area, area, &g);
    }
}
fn ascii_chart(values: &[u64], width: usize, height: usize) -> String {
    if values.is_empty() || width == 0 || height == 0 {
        return String::new();
    }
    let columns = (0..width)
        .map(|column| {
            let index = column.saturating_mul(values.len()) / width;
            values[index.min(values.len() - 1)].min(1_000)
        })
        .collect::<Vec<_>>();
    (0..height)
        .rev()
        .map(|row| {
            let threshold = ((row + 1) as u64 * 1_000).div_ceil(height as u64);
            columns
                .iter()
                .map(|value| if *value >= threshold { '#' } else { ' ' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format nanoseconds-since-the-Unix-epoch as a UTC `YYYY-MM-DD HH:MM:SS.mmm` string. Hand-rolled to
/// avoid pulling a datetime crate into the terminal graph (footprint is a first-class constraint);
/// the civil-date conversion is Howard Hinnant's `civil_from_days` algorithm, valid for any i64.
fn format_timestamp_ns(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let millis = ns.rem_euclid(1_000_000_000) / 1_000_000;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hours, minutes, seconds) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Just the `HH:MM:SS` (UTC) time-of-day — compact axis tick labels for the time-series viewer.
fn clock_hms_ns(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let tod = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Wall-clock `YYYY-MM-DD HH:MM:SS` (UTC), no sub-second part — used for the header clock.
fn format_datetime_ns(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hours, minutes, seconds) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

/// Parse a UTC `YYYY-MM-DD[ HH:MM[:SS]]` (space or `T` separator) into nanoseconds since the Unix
/// epoch. The time part is optional (missing minute/second default to 0), but every field is range-
/// checked; returns `None` on any malformed or out-of-range field so the form can report it. Public
/// so a host can build [`Options::window`] from the same textual format the picker accepts.
pub fn parse_datetime(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once([' ', 'T']) {
        Some((date, time)) => (date, time.trim()),
        None => (text, ""),
    };
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.trim().parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (mut hour, mut minute, mut second) = (0u32, 0u32, 0u32);
    if !time.is_empty() {
        let mut time_parts = time.split(':');
        hour = time_parts.next()?.parse().ok()?;
        if let Some(part) = time_parts.next() {
            minute = part.parse().ok()?;
        }
        if let Some(part) = time_parts.next() {
            second = part.parse().ok()?;
        }
        if time_parts.next().is_some() {
            return None;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let secs = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour as i64 * 3600 + minute as i64 * 60 + second as i64)?;
    secs.checked_mul(1_000_000_000)
}

/// Inverse of [`civil_from_days`]: days since 1970-01-01 for a proleptic-Gregorian UTC date (Howard
/// Hinnant's algorithm, matching the constants used by `civil_from_days`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Convert a count of days since 1970-01-01 into `(year, month, day)` (proleptic Gregorian, UTC).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// OTel severity number to a `BAND (n)` label (e.g. `INFO (9)`).
fn severity_label(severity: SeverityNumber) -> String {
    let band = match severity.0 {
        0 => "UNSET",
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        _ => "FATAL",
    };
    format!("{band} ({})", severity.0)
}

/// Render an attribute value as a single display string (arrays/maps compacted, bytes as hex).
fn render_value(value: &AnyValue) -> String {
    match value {
        AnyValue::Null => "null".to_owned(),
        AnyValue::Str(text) => text.clone(),
        AnyValue::Int(int) => int.to_string(),
        AnyValue::Double(double) => double.to_string(),
        AnyValue::Bool(boolean) => boolean.to_string(),
        AnyValue::Bytes(bytes) => {
            let mut out = String::with_capacity(2 + bytes.len() * 2);
            out.push_str("0x");
            for byte in bytes {
                out.push_str(&format!("{byte:02x}"));
            }
            out
        }
        AnyValue::Array(items) => {
            let inner = items
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        AnyValue::Map(entries) => {
            let inner = entries
                .iter()
                .map(|(key, value)| format!("{key}: {}", render_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

/// Flatten an attribute map into displayable `(key, value)` pairs.
fn attrs_to_pairs(attributes: &Attributes) -> Vec<(String, String)> {
    attributes
        .iter()
        .map(|(key, value)| (key.to_owned(), render_value(value)))
        .collect()
}

/// The lines of the log-entry detail view (header fields, body, then attribute sections).
fn log_detail_lines(record: &LogRecord) -> Vec<String> {
    let mut lines = vec![
        format!("Time      {}", format_timestamp_ns(record.time_ns)),
        format!("Severity  {}", record.severity),
        format!("Service   {}", record.service.as_deref().unwrap_or("-")),
        format!(
            "Trace ID  {}",
            record.trace_id.as_deref().unwrap_or("(none)")
        ),
        format!(
            "Span ID   {}",
            record.span_id.as_deref().unwrap_or("(none)")
        ),
        String::new(),
        "Body".to_owned(),
    ];
    if record.body.is_empty() {
        lines.push("  (empty)".to_owned());
    } else {
        lines.extend(record.body.lines().map(|line| format!("  {line}")));
    }
    push_attr_section(&mut lines, "Attributes", &record.attributes);
    push_attr_section(&mut lines, "Resource", &record.resource);
    push_attr_section(&mut lines, "Scope", &record.scope);
    lines
}

/// Append a titled attribute section (`Title (n)` then `  key = value` rows) when non-empty.
fn push_attr_section(lines: &mut Vec<String>, title: &str, pairs: &[(String, String)]) {
    if pairs.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push(format!("{title} ({})", pairs.len()));
    for (key, value) in pairs {
        lines.push(format!("  {key} = {value}"));
    }
}

/// Non-function keywords worth colouring per query language. Anything immediately followed by `(` is
/// treated as a function regardless of this set, so only the "bare word" operators need listing.
/// The LogQL line-filter operators the Logs search box accepts, offered as expression-position
/// completion hints on the Logs screen. `|?` / `!?` are the imbh dialect's Tantivy-accelerated term
/// operators; `|=` / `!=` (substring) and `|~` / `!~` (regex) are standard LogQL. Kept in sync with
/// the parser in `imbh-lgtm` (`syntax/logql.rs`).
const LOGQL_LINE_FILTERS: &[&str] = &["|=", "!=", "|~", "!~", "|?", "!?"];

fn keywords_for(screen: Screen) -> &'static [&'static str] {
    match screen {
        Screen::Metrics => &[
            "by",
            "without",
            "on",
            "ignoring",
            "group_left",
            "group_right",
            "offset",
            "bool",
            "and",
            "or",
            "unless",
            "inf",
            "nan",
        ],
        Screen::Logs => &[
            "by",
            "without",
            "unwrap",
            "json",
            "logfmt",
            "regexp",
            "pattern",
            "line_format",
            "label_format",
            "ip",
            "and",
            "or",
        ],
        Screen::Traces => &[
            "by",
            "select",
            "and",
            "or",
            "duration",
            "status",
            "name",
            "kind",
            "rootName",
            "rootServiceName",
        ],
        Screen::Overview => &[],
    }
}

/// Call-like functions worth completing per language (those that take a `(`). Accepting one appends
/// the opening paren. Bare keywords come from [`keywords_for`]; metric names are dynamic.
fn functions_for(screen: Screen) -> &'static [&'static str] {
    match screen {
        Screen::Metrics => &[
            "abs",
            "absent",
            "absent_over_time",
            "avg",
            "avg_over_time",
            "bottomk",
            "ceil",
            "changes",
            "clamp",
            "clamp_max",
            "clamp_min",
            "count",
            "count_over_time",
            "count_values",
            "delta",
            "deriv",
            "exp",
            "floor",
            "histogram_quantile",
            "increase",
            "irate",
            "label_join",
            "label_replace",
            "last_over_time",
            "ln",
            "log10",
            "log2",
            "max",
            "max_over_time",
            "min",
            "min_over_time",
            "predict_linear",
            "present_over_time",
            "quantile",
            "quantile_over_time",
            "rate",
            "resets",
            "round",
            "scalar",
            "sort",
            "sort_desc",
            "sqrt",
            "stddev",
            "stddev_over_time",
            "stdvar",
            "sum",
            "sum_over_time",
            "time",
            "timestamp",
            "topk",
            "vector",
        ],
        Screen::Logs => &[
            "avg",
            "avg_over_time",
            "bottomk",
            "bytes_over_time",
            "bytes_rate",
            "count",
            "count_over_time",
            "first_over_time",
            "last_over_time",
            "max",
            "max_over_time",
            "min",
            "min_over_time",
            "quantile_over_time",
            "rate",
            "stddev_over_time",
            "stdvar_over_time",
            "sum",
            "sum_over_time",
            "topk",
        ],
        Screen::Traces => &["avg", "count", "histogram", "max", "min", "quantile", "sum"],
        Screen::Overview => &[],
    }
}

/// The identifier token at the very end of the query — the run of `[A-Za-z0-9_:.]` the editor's caret
/// (always the end of the string) currently sits on. This is what completion suggests against.
fn current_token(query: &str) -> &str {
    let mut start = query.len();
    for (index, ch) in query.char_indices().rev() {
        if ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.') {
            start = index;
        } else {
            break;
        }
    }
    &query[start..]
}

/// Whether `ch` can appear in a metric/label identifier (the run [`current_token`] recognises).
fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.')
}

/// Classify the caret position (always the end of `query`) into a [`CompletionContext`] and return the
/// partial token being written there — the metric/label identifier run in expression/label-name
/// position, or the raw text after the opening `"` in value position. The parser is deliberately
/// lightweight (it counts quotes and scans for the last unbalanced `{`) rather than a full grammar; it
/// only decides which vocabulary is eligible, never rejects input.
fn completion_context(query: &str) -> (CompletionContext, &str) {
    // An odd number of `"` means the caret sits inside an open quoted value. Everything after the last
    // `"` is the partial value; the label is the identifier just before the operator before the quote.
    if query.bytes().filter(|&b| b == b'"').count() % 2 == 1 {
        let open = query.rfind('"').expect("odd quote count implies a quote");
        let before = &query[..open];
        let label =
            trailing_ident(before.trim_end_matches(['=', '!', '~', '<', '>', ' '])).to_owned();
        let metric = before
            .rfind('{')
            .map(|brace| trailing_ident(&before[..brace]))
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        return (
            CompletionContext::LabelValue { metric, label },
            &query[open + 1..],
        );
    }
    // Not in a quote: are we inside an open `{…}` matcher block (last `{` after last `}`)?
    let open_brace = match (query.rfind('{'), query.rfind('}')) {
        (Some(open), close) if close.is_none_or(|c| open > c) => Some(open),
        _ => None,
    };
    if let Some(open) = open_brace {
        let token = current_token(query);
        // The significant character before the token decides key vs. value position: a label name
        // follows `{`, `,`, or whitespace; anything after an operator (`=` etc.) is a value, which
        // PromQL requires to be quoted, so an unquoted value position offers nothing.
        let before_token = &query[..query.len() - token.len()];
        let prev = before_token.trim_end().chars().next_back();
        match prev {
            Some('{') | Some(',') | None => {
                let metric = trailing_ident(&query[..open]);
                let metric = (!metric.is_empty()).then(|| metric.to_owned());
                (CompletionContext::LabelName { metric }, token)
            }
            _ => (CompletionContext::Suppressed, token),
        }
    } else {
        (CompletionContext::Expr, current_token(query))
    }
}

/// The trailing identifier run of `s` (like [`current_token`] but usable on an arbitrary slice).
fn trailing_ident(s: &str) -> &str {
    let mut start = s.len();
    for (index, ch) in s.char_indices().rev() {
        if is_ident_char(ch) {
            start = index;
        } else {
            break;
        }
    }
    &s[start..]
}

/// Rank completion candidates whose name starts with `token` (case-insensitive), filtered by the
/// caret `context`. In expression position: metric names first (Metrics screen), then functions, then
/// keywords. In a `{…}` matcher: the label names of the referenced metric (or the union across all
/// discovered metrics for a bare selector). In a quoted value: that label's known values. Each group
/// is sorted and deduplicated.
fn completion_candidates(
    screen: Screen,
    metric_names: &[String],
    metric_tree: &[MetricNode],
    log_labels: &[String],
    log_label_values: &HashMap<String, Vec<String>>,
    context: &CompletionContext,
    token: &str,
) -> Vec<Candidate> {
    const MAX_CANDIDATES: usize = 50;
    let lower = token.to_ascii_lowercase();
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push_group = |mut names: Vec<String>, kind: CandidateKind, out: &mut Vec<Candidate>| {
        names.sort();
        for name in names {
            if name.to_ascii_lowercase().starts_with(&lower) && seen.insert(name.clone()) {
                out.push(Candidate { text: name, kind });
            }
        }
    };

    match context {
        CompletionContext::Suppressed => {}
        CompletionContext::Expr if screen == Screen::Logs => {
            // The Logs box is a LogQL selector + line-filter box, not a metric-query box, so its
            // expression vocabulary is the imbh line-filter operator hints (not the PromQL/range
            // function list) plus the LogQL pipeline keywords.
            let operators = LOGQL_LINE_FILTERS.iter().map(|s| (*s).to_owned()).collect();
            push_group(operators, CandidateKind::Operator, &mut out);
            let keywords = keywords_for(screen)
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            push_group(keywords, CandidateKind::Keyword, &mut out);
        }
        CompletionContext::Expr => {
            if screen == Screen::Metrics {
                push_group(metric_names.to_vec(), CandidateKind::Metric, &mut out);
            }
            let functions = functions_for(screen)
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            push_group(functions, CandidateKind::Function, &mut out);
            let keywords = keywords_for(screen)
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            push_group(keywords, CandidateKind::Keyword, &mut out);
        }
        CompletionContext::LabelName { metric } => {
            if screen == Screen::Logs {
                // The Logs selector's label names come from cross-signal attribute discovery, not a
                // per-metric dimension tree (there is no metric here).
                push_group(log_labels.to_vec(), CandidateKind::Label, &mut out);
            } else {
                // Label keys for the named metric, or the union across all discovered metrics for a
                // bare selector or an as-yet-undiscovered metric.
                let keys = dims_for(metric_tree, metric.as_deref())
                    .flat_map(|dims| dims.iter().map(|dim| dim.label.clone()))
                    .collect();
                push_group(keys, CandidateKind::Label, &mut out);
            }
        }
        CompletionContext::LabelValue { metric, label } => {
            if screen == Screen::Logs {
                // That label's distinct values, discovered per key (empty until they arrive).
                let values = log_label_values.get(label).cloned().unwrap_or_default();
                push_group(values, CandidateKind::LabelValue, &mut out);
            } else {
                let values = dims_for(metric_tree, metric.as_deref())
                    .flat_map(|dims| dims.iter())
                    .filter(|dim| &dim.label == label)
                    .flat_map(|dim| dim.values.iter().cloned())
                    .collect();
                push_group(values, CandidateKind::LabelValue, &mut out);
            }
        }
    }

    out.truncate(MAX_CANDIDATES);
    out
}

/// The discovered dimension lists in scope for label completion: just the named metric's (when known
/// and loaded), otherwise every metric's — so a bare `{…}` selector still offers the full label
/// vocabulary. Only metrics whose dimensions have been discovered contribute.
fn dims_for<'a>(
    metric_tree: &'a [MetricNode],
    metric: Option<&'a str>,
) -> Box<dyn Iterator<Item = &'a [DimNode]> + 'a> {
    if let Some(name) = metric
        && let Some(node) = metric_tree.iter().find(|n| n.name == name)
    {
        // A known metric contributes only once its dimensions are loaded; while `None`, fall through
        // to nothing (the caller triggers discovery) rather than the whole-catalog union.
        return Box::new(node.dims.as_deref().into_iter());
    }
    Box::new(metric_tree.iter().filter_map(|n| n.dims.as_deref()))
}

/// Tokenize a query into coloured spans for the input bar. Deliberately a lightweight lexer shared by
/// all three languages (strings, numbers/durations, identifiers/functions, operators, punctuation)
/// rather than a per-grammar parser; it is presentation only and never rejects input.
fn highlight_query(screen: Screen, query: &str, g: &Glyphs) -> Vec<Span<'static>> {
    // `?` is included so the imbh LogQL dialect's `|?` / `!?` term operators highlight as a unit.
    const OPERATORS: &[char] = &[
        '=', '!', '~', '<', '>', '|', '&', '+', '-', '*', '/', '^', '?',
    ];
    let keywords = keywords_for(screen);
    let chars: Vec<char> = query.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let take = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            // Several queries are stored newline-joined (multi-metric visualization); show each break
            // as a visible separator so the single-line bar stays readable.
            spans.push(Span::styled(
                format!(" {} ", g.vline),
                Style::default().fg(Color::DarkGray),
            ));
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            spans.push(Span::raw(take(start, i)));
        } else if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && quote != '`' {
                    i = (i + 2).min(chars.len());
                    continue;
                }
                let ch = chars[i];
                i += 1;
                if ch == quote {
                    break;
                }
            }
            spans.push(Span::styled(
                take(start, i),
                Style::default().fg(Color::Green),
            ));
        } else if c.is_ascii_digit() {
            // Numbers and durations (e.g. `5m`, `1h30m`, `0.5`) — trailing unit letters included.
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            spans.push(Span::styled(
                take(start, i),
                Style::default().fg(Color::Magenta),
            ));
        } else if c.is_alphabetic() || c == '_' || c == ':' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | ':' | '.'))
            {
                i += 1;
            }
            let word = take(start, i);
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_call = j < chars.len() && chars[j] == '(';
            let style = if is_call || keywords.contains(&word.as_str()) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            spans.push(Span::styled(word, style));
        } else if OPERATORS.contains(&c) {
            let start = i;
            while i < chars.len() && OPERATORS.contains(&chars[i]) {
                i += 1;
            }
            spans.push(Span::styled(
                take(start, i),
                Style::default().fg(Color::Yellow),
            ));
        } else if matches!(c, '{' | '}' | '[' | ']' | '(' | ')' | ',') {
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            i += 1;
        } else {
            spans.push(Span::raw(c.to_string()));
            i += 1;
        }
    }
    spans
}

/// Approximate the number of terminal rows a logical line occupies once wrapped to `width` columns.
/// Uses the character count (not display width), which is an adequate estimate for clamping the
/// result-pane scroll; an off-by-a-row on unusually wide glyphs is harmless.
fn wrapped_rows(line: &str, width: u16) -> u32 {
    if width == 0 {
        return 1;
    }
    (line.chars().count().max(1) as u32).div_ceil(width as u32)
}

/// Render the primary pane as a selectable table with a header row and column-aligned cells. The
/// selection cursor (`app.selected`) indexes `table.rows`; `TableState` scrolls to keep it in view.
fn draw_metric_table(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    table: &TableData,
    area: Rect,
    focused: bool,
    g: &Glyphs,
) {
    // Column widths from the widest cell (header included), each capped so one wide column cannot
    // crowd out the rest; the final column absorbs remaining space.
    // Measure by display width, not code-point count, so full-width (CJK) glyphs and other wide
    // characters in names/labels/values size their column to the cells they actually occupy instead of
    // being under-measured and truncated. Consistent with the width-aware header and waterfall.
    let column_count = table.header.len();
    let mut widths = table
        .header
        .iter()
        .map(|cell| UnicodeWidthStr::width(cell.as_str()))
        .collect::<Vec<_>>();
    for row in &table.rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
    }
    let constraints = widths
        .iter()
        .enumerate()
        .map(|(index, &width)| {
            if index + 1 == column_count {
                Constraint::Min(width.clamp(6, 60) as u16)
            } else {
                Constraint::Length(width.clamp(4, 48) as u16)
            }
        })
        .collect::<Vec<_>>();

    let header = Row::new(table.header.iter().cloned()).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    );
    // In the catalog tree the first column carries the branch marker (`v `/`> `). Split that leading
    // marker into a dark-grey span so it reads as chrome rather than content. Only the tree rows carry
    // a marker prefix — checkbox/loading rows and the (non-catalog) series table never match, so they
    // render unchanged.
    let on_catalog = app.on_catalog();
    const BRANCH_MARKERS: [&str; 2] = ["v ", "> "];
    let rows = table
        .rows
        .iter()
        .map(|row| {
            let cells = row
                .iter()
                .enumerate()
                .map(|(col, text)| {
                    if col == 0 && on_catalog {
                        let indent = text.len() - text.trim_start().len();
                        if let Some(marker) = BRANCH_MARKERS
                            .iter()
                            .find(|m| text[indent..].starts_with(**m))
                        {
                            let (head, tail) = text.split_at(indent + marker.len());
                            return Line::from(vec![
                                Span::styled(head.to_owned(), Style::default().fg(Color::DarkGray)),
                                Span::raw(tail.to_owned()),
                            ]);
                        }
                    }
                    Line::from(text.clone())
                })
                .collect::<Vec<_>>();
            Row::new(cells)
        })
        .collect::<Vec<_>>();

    let selection = app
        .selectable_bounds()
        .map(|(first, last)| app.selected.clamp(first, last));
    let title = match selection {
        Some(selected) if !table.rows.is_empty() => {
            format!(
                "{}  [{}/{}]",
                app.snapshot.title,
                selected + 1,
                table.rows.len()
            )
        }
        _ => app.snapshot.title.clone(),
    };
    let mut state = TableState::default();
    state.select(selection);
    frame.render_stateful_widget(
        Table::new(rows, constraints)
            .header(header)
            .column_spacing(2)
            .block(g.block().border_style(focus_border(focused)).title(title))
            // No highlight symbol: the tree rows already begin with `v `/`> ` markers, so a `>` cursor
            // would be ambiguous. The row-highlight style alone marks the selection.
            .row_highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

/// Render the log-entry detail view (a title bar, the scrollable record body, and a hint bar) into the
/// content area beneath the menu bar. Publishes the scroll bounds (via `App`'s cells) so the key
/// handler can clamp `↑/↓/PageDown`.
fn draw_log_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    record: &LogRecord,
    area: Rect,
    focused: bool,
    g: &Glyphs,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Log entry detail ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .block(g.block().title("Logs")),
        rows[0],
    );

    let lines = log_detail_lines(record);
    let body_area = rows[1];
    let inner_width = body_area.width.saturating_sub(2);
    let viewport = body_area.height.saturating_sub(2);
    let total_rows: u16 = lines
        .iter()
        .map(|line| wrapped_rows(line, inner_width))
        .sum::<u32>()
        .min(u16::MAX as u32) as u16;
    let max_scroll = total_rows.saturating_sub(viewport);
    app.max_scroll.set(max_scroll);
    app.page_rows.set(viewport.max(1));
    let scroll = app.scroll.min(max_scroll);
    let title = if max_scroll > 0 {
        format!("Detail  [{scroll}/{max_scroll} {}]", g.scroll())
    } else {
        "Detail".to_owned()
    };
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(g.block().border_style(focus_border(focused)).title(title)),
        body_area,
    );

    let (sep, left, right, scroll_hint) = (g.sep, g.left, g.right, g.scroll());
    let hint = if record.trace_id.is_some() {
        format!(
            "esc/{left} back {sep} enter open trace {sep} {right} fwd {sep} {scroll_hint} scroll"
        )
    } else {
        format!("esc/{left} back {sep} (no trace id) {sep} {scroll_hint} scroll")
    };
    frame.render_widget(Paragraph::new(hint).wrap(Wrap { trim: true }), rows[2]);
}

/// Render the detailed time-series viewer for one selected metric series into the content area beneath
/// the menu bar: a header, a line chart of the series over the query window (with a movable vertical
/// cursor), and a readout of the point under the cursor plus summary stats. ASCII fallback via
/// `--ascii`.
fn draw_metric_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    detail: &MetricDetail,
    options: &Options,
    area: Rect,
    focused: bool,
) {
    let g = Glyphs::new(options.ascii);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(area);

    // Header: the series labels and the source query.
    let labels = if detail.labels.is_empty() {
        "{}"
    } else {
        detail.labels.as_str()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Series ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {labels}")),
        ]))
        .block(
            g.block()
                .title(format!("Metric series {} {}", g.dash, detail.query)),
        ),
        rows[0],
    );

    let plot_area = rows[1];
    // Page the x-cursor by roughly a screenful of plot columns.
    app.page_rows.set(plot_area.width.saturating_sub(2).max(1));
    let cursor = app.metric_cursor.min(detail.points.len().saturating_sub(1));

    // Finite samples drive the plot and the stats; a cursor may still land on a gap (NaN) sample.
    let finite: Vec<(f64, f64)> = detail
        .points
        .iter()
        .filter(|(_, value)| value.is_finite())
        .map(|(time_ns, value)| (*time_ns as f64 / 1e9, *value))
        .collect();

    if options.ascii || finite.len() < 2 {
        // ASCII fallback (or too few points for a line): the hand-rolled chart, cursor shown in the
        // readout rather than on the plot.
        let inner_w = plot_area.width.saturating_sub(2) as usize;
        let inner_h = plot_area.height.saturating_sub(2) as usize;
        let body = if finite.is_empty() {
            "no finite samples in this window".to_owned()
        } else {
            ascii_chart(
                &chart_values(finite.iter().map(|(_, value)| *value)),
                inner_w,
                inner_h,
            )
        };
        frame.render_widget(
            Paragraph::new(body)
                .block(g.block().border_style(focus_border(focused)).title("Chart")),
            plot_area,
        );
        // No ratatui line chart here, so nothing for the mascot to ride.
        app.chart_geom.replace(None);
    } else {
        let (x_min, x_max) = (finite.first().unwrap().0, finite.last().unwrap().0);
        let (mut y_min, mut y_max) = finite
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| {
                (a.0.min(p.1), a.1.max(p.1))
            });
        // Pad the y-range (and widen a flat line) so the plot is not glued to the border.
        if (y_max - y_min).abs() < f64::EPSILON {
            y_min -= 1.0;
            y_max += 1.0;
        } else {
            let pad = (y_max - y_min) * 0.05;
            y_min -= pad;
            y_max += pad;
        }
        // A vertical line at the cursor's timestamp, spanning the y-range.
        let cursor_x = detail.points[cursor].0 as f64 / 1e9;
        let cursor_line = [(cursor_x, y_min), (cursor_x, y_max)];
        // Exemplar → trace markers as magenta dots along the plot floor, at each exemplar's timestamp.
        let exemplar_points: Vec<(f64, f64)> = app
            .metric_exemplars
            .iter()
            .map(|marker| (marker.time_ns as f64 / 1e9, y_min))
            .filter(|(x, _)| *x >= x_min && *x <= x_max)
            .collect();
        let mut datasets = vec![
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(&finite),
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Yellow))
                .data(&cursor_line),
        ];
        if !exemplar_points.is_empty() {
            datasets.push(
                Dataset::default()
                    .marker(Marker::Dot)
                    .graph_type(GraphType::Scatter)
                    .style(Style::default().fg(Color::Magenta))
                    .data(&exemplar_points),
            );
        }
        let x_labels = vec![
            Line::from(clock_hms_ns(detail.points.first().unwrap().0)),
            Line::from(clock_hms_ns(detail.points[cursor].0)),
            Line::from(clock_hms_ns(detail.points.last().unwrap().0)),
        ];
        let y_labels = vec![
            Line::from(format_metric_value(y_min)),
            Line::from(format_metric_value((y_min + y_max) / 2.0)),
            Line::from(format_metric_value(y_max)),
        ];
        let chart = Chart::new(datasets)
            .block(g.block().border_style(focus_border(focused)).title("Chart"))
            .x_axis(
                Axis::default()
                    .style(Style::default().fg(Color::DarkGray))
                    .bounds([x_min, x_max])
                    .labels(x_labels),
            )
            .y_axis(
                Axis::default()
                    .style(Style::default().fg(Color::DarkGray))
                    .bounds([y_min, y_max])
                    .labels(y_labels),
            );
        frame.render_widget(chart, plot_area);

        // Publish where each datapoint actually landed on screen, so the mascot's chart ride can walk
        // the rendered line (see `ChartRide`). Reproduces ratatui's `Chart` graph-area layout exactly.
        let y_label_strs = [
            format_metric_value(y_min),
            format_metric_value((y_min + y_max) / 2.0),
            format_metric_value(y_max),
        ];
        let x_first = clock_hms_ns(detail.points.first().unwrap().0);
        let block_inner = g.block().inner(plot_area);
        let geom = chart_graph_area(block_inner, &y_label_strs, &x_first).map(|graph| {
            let cells = finite
                .iter()
                .filter_map(|&(x, y)| chart_point_cell(graph, x_min, x_max, y_min, y_max, x, y))
                .collect();
            ChartGeometry { graph, cells }
        });
        app.chart_geom.replace(geom);
    }

    // Readout: the point under the cursor, then summary stats over the finite samples. The window can
    // hold no points at all (e.g. after panning/zooming to an empty range), so the cursor line degrades
    // to a placeholder rather than indexing an empty series.
    let cursor_line = match detail.points.get(cursor) {
        Some(&(cursor_ns, cursor_val)) => {
            let cursor_value = if cursor_val.is_finite() {
                format_metric_value(cursor_val)
            } else {
                "n/a".to_owned()
            };
            Line::from(vec![
                Span::styled(
                    format!("cursor [{}/{}] ", cursor + 1, detail.points.len()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(format!(
                    "{}  =  {cursor_value}",
                    format_datetime_ns(cursor_ns)
                )),
            ])
        }
        None => Line::from(Span::styled(
            "no samples in this window",
            Style::default().fg(Color::Yellow),
        )),
    };
    let values: Vec<f64> = finite.iter().map(|(_, value)| *value).collect();
    let stats = if values.is_empty() {
        "no finite samples".to_owned()
    } else {
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let avg = values.iter().copied().sum::<f64>() / values.len() as f64;
        let latest = *values.last().unwrap();
        format!(
            "min {} {s} max {} {s} avg {} {s} latest {} {s} {} pts",
            format_metric_value(min),
            format_metric_value(max),
            format_metric_value(avg),
            format_metric_value(latest),
            detail.points.len(),
            s = g.sep,
        )
    };
    // Surface the exemplar→trace markers (magenta dots on the plot floor): count + the Enter action.
    let stats = if app.metric_exemplars.is_empty() {
        stats
    } else {
        format!(
            "{stats} {s} {} exemplars (enter: nearest trace)",
            app.metric_exemplars.len(),
            s = g.sep,
        )
    };
    let readout = vec![
        cursor_line,
        Line::from(Span::styled(stats, Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!(
                "esc/{l} back {s} {r} fwd {s} h/l or shift+{l}{r} move cursor {s} home/end ends {s} pgup/pgdn page",
                l = g.left,
                r = g.right,
                s = g.sep
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(readout).block(g.block()), rows[2]);
}

fn draw_time_range_picker(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    anchor: Rect,
    area: Rect,
    g: &Glyphs,
) {
    // Drop straight down from the indicator box, right-aligned to its right edge so a wide dropdown
    // does not spill past the frame; clamp on every side to stay within the terminal.
    let width = 36u16.min(area.width);
    // One row per preset plus the trailing "Absolute…" row, and the two borders.
    let height = (TIME_RANGES.len() as u16 + 3).min(area.height);
    let x = anchor
        .right()
        .saturating_sub(width)
        .min(area.right().saturating_sub(width))
        .max(area.x);
    let y = anchor.bottom().min(area.bottom().saturating_sub(height));
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let mut items = TIME_RANGES
        .iter()
        .map(|(label, lookback, step)| {
            ListItem::new(format!(
                "{label:<4} window {:>5}  step {}s",
                humanize_secs(lookback.as_secs()),
                step.as_secs()
            ))
        })
        .collect::<Vec<_>>();
    items.push(ListItem::new(format!(
        "Absolute{}  set explicit start / end",
        g.ellipsis
    )));
    let mut state = ListState::default();
    state.select(Some(app.range_cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(g.block().title("Time range (Enter: apply, Esc: cancel)"))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        popup,
        &mut state,
    );
}

/// Render the absolute-time window form as a dropdown under the indicator box: two labeled datetime
/// fields (the focused one highlighted with a caret) and a hint/parse-error line.
fn draw_absolute_range(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    anchor: Rect,
    area: Rect,
    g: &Glyphs,
) {
    let width = 48u16.min(area.width);
    // Two borders + two field lines + one hint line.
    let height = 5u16.min(area.height);
    let x = anchor
        .right()
        .saturating_sub(width)
        .min(area.right().saturating_sub(width))
        .max(area.x);
    let y = anchor.bottom().min(area.bottom().saturating_sub(height));
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let field_line = |label: &str, value: &str, focused: bool| {
        let value_style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(format!(" {label}  "), Style::default().fg(Color::DarkGray)),
            Span::styled(value.to_owned(), value_style),
        ];
        if focused {
            // A block caret marks the edit point (the global terminal cursor stays hidden).
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        }
        Line::from(spans)
    };
    let hint = match &app.abs_error {
        Some(error) => Span::styled(format!(" {error}"), Style::default().fg(Color::Red)),
        None => Span::styled(
            format!(
                " UTC {s} YYYY-MM-DD HH:MM:SS {s} Tab: field {s} Enter: apply",
                s = g.sep
            ),
            Style::default().fg(Color::DarkGray),
        ),
    };
    let text = vec![
        field_line("start", &app.abs_start, app.abs_field == 0),
        field_line("end  ", &app.abs_end, app.abs_field == 1),
        Line::from(hint),
    ];
    frame.render_widget(
        Paragraph::new(text).block(g.block().title("Absolute range (Esc: cancel)")),
        popup,
    );
}

/// Render the completion popup anchored just below the query bar. Candidates are coloured by kind
/// (metric/function/keyword) and the highlighted row tracks `completion.selected`.
fn draw_completion_popup(
    frame: &mut ratatui::Frame<'_>,
    completion: &Completion,
    query_area: Rect,
    frame_area: Rect,
    g: &Glyphs,
) {
    const MAX_VISIBLE: usize = 8;
    let longest = completion
        .candidates
        .iter()
        .map(|candidate| candidate.text.chars().count())
        .max()
        .unwrap_or(8);
    // border (2) + highlight symbol "▶ " (2) around the widest candidate.
    let desired_width = longest.clamp(8, 40) as u16 + 4;
    let height = completion.candidates.len().min(MAX_VISIBLE) as u16 + 2;
    // Anchor one cell in from the query box's left edge, directly beneath it.
    let x = query_area.x + 2;
    let y = query_area.y + query_area.height;
    let width = desired_width.min(frame_area.right().saturating_sub(x).max(1));
    let height = height.min(frame_area.bottom().saturating_sub(y).max(1));
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let items = completion
        .candidates
        .iter()
        .map(|candidate| {
            let style = match candidate.kind {
                CandidateKind::Function => Style::default().fg(Color::Cyan),
                CandidateKind::Keyword => Style::default().fg(Color::Yellow),
                CandidateKind::Metric => Style::default(),
                CandidateKind::Label => Style::default().fg(Color::Green),
                CandidateKind::LabelValue => Style::default().fg(Color::Magenta),
                CandidateKind::Operator => Style::default().fg(Color::Blue),
            };
            ListItem::new(Span::styled(candidate.text.clone(), style))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(completion.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(g.block().title("Tab: complete"))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        popup,
        &mut state,
    );
}

/// Compact `Ns`/`Nm`/`Nh`/`Nd` rendering of a whole-second duration for the picker rows.
fn humanize_secs(secs: u64) -> String {
    if secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs.is_multiple_of(3_600) {
        format!("{}h", secs / 3_600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_terminal_emits_leave_alt_screen_and_show() {
        // Guards gap #1: the restore path must always leave the alternate screen (and show the
        // cursor), never just disable raw mode. `disable_raw_mode` acts on the real terminal fd, not
        // our buffer, so the buffer captures exactly the `execute!(Show, LeaveAlternateScreen)`
        // output — compare it against the same command sequence rendered independently.
        let mut buf: Vec<u8> = Vec::new();
        restore_terminal(&mut buf);

        let mut expected: Vec<u8> = Vec::new();
        let _ = execute!(expected, Show, LeaveAlternateScreen);
        assert_eq!(buf, expected);
        // Sanity: something was actually written (the alt-screen leave is not a no-op).
        assert!(!buf.is_empty());
    }

    #[test]
    fn panic_hook_install_claim_is_idempotent() {
        // Guards gap #2's double-install guard without touching the process-global panic hook.
        let flag = AtomicBool::new(false);
        assert!(claim_panic_hook_install(&flag), "first claim wins");
        assert!(!claim_panic_hook_install(&flag), "second claim is refused");
        assert!(
            !claim_panic_hook_install(&flag),
            "further claims stay refused"
        );
    }

    #[test]
    fn stale_query_results_are_discarded() {
        let mut app = App::new();
        app.generation = 2;
        app.route = Route::Logs;
        app.apply(QueryResult {
            generation: 1,
            screen: Screen::Logs,
            result: Ok(Snapshot::message("old", "old")),
        });
        assert_ne!(app.snapshot.title, "old");
    }
    #[test]
    fn ascii_chart_uses_only_ascii_and_requested_dimensions() {
        let rendered = ascii_chart(&[0, 500, 1_000], 6, 3);
        assert!(rendered.is_ascii());
        let rows = rendered.lines().collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.len() == 6));
    }

    #[test]
    fn mascot_is_hidden_by_default() {
        assert!(!App::new().show_mascot);
    }

    #[test]
    fn mascot_art_selects_the_facing_pair_and_waddle_phase() {
        // Rightward facing uses the first pair (1-a/1-b), leftward the second (2-a/2-b).
        assert_eq!(mascot_art(1, 0), &MASCOT_FRAMES[0]);
        assert_eq!(mascot_art(1, 1), &MASCOT_FRAMES[1]);
        assert_eq!(mascot_art(-1, 0), &MASCOT_FRAMES[2]);
        assert_eq!(mascot_art(-1, 1), &MASCOT_FRAMES[3]);
        // The waddle phase flips every frame period and stays in range for negative accumulators.
        assert_eq!(mascot_phase(0), 0);
        assert_eq!(mascot_phase(MASCOT_FRAME_NS), 1);
        assert_eq!(mascot_phase(2 * MASCOT_FRAME_NS), 0);
        assert_eq!(mascot_phase(-1), 1);
    }

    #[test]
    fn ascii_mode_renders_only_ascii_across_the_ui() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // Render `draw` in `--ascii` mode across the states that own the UI chrome (borders, header
        // logo/clock, hint separators, arrows, pickers, detail views) and assert every emitted cell is
        // pure ASCII — the guarantee `--ascii` makes. Content is never rewritten, so the fixtures below
        // deliberately use ASCII-only titles/bodies; a non-ASCII cell can then only be leaked chrome.
        let options = Options {
            ascii: true,
            ..Options::default()
        };

        let log_record = LogRecord {
            time_ns: 0,
            severity: "INFO".to_owned(),
            service: Some("api".to_owned()),
            body: "hello".to_owned(),
            trace_id: Some("abcdef01".to_owned()),
            span_id: Some("0123".to_owned()),
            attributes: vec![("k".to_owned(), "v".to_owned())],
            resource: Vec::new(),
            scope: Vec::new(),
        };
        let metric_detail = MetricDetail {
            labels: "service=api".to_owned(),
            query: "up".to_owned(),
            points: vec![(0, 1.0), (1_000_000_000, 2.0), (2_000_000_000, 3.0)],
        };

        // The states that own the UI chrome, each a fresh App tweaked into the state under test.
        let states: Vec<(&str, App)> = vec![
            ("overview", App::new()),
            ("menu", {
                let mut app = App::new();
                app.mode = Mode::Menu;
                app
            }),
            ("time-range picker", {
                let mut app = App::new();
                app.mode = Mode::TimeRange;
                app
            }),
            ("absolute-range form", {
                let mut app = App::new();
                app.open_absolute_form();
                app.abs_error = Some("start must be before end".to_owned());
                app
            }),
            ("scrolled list", {
                // Many lines in a short terminal forces the `[n/m ^v]` scroll title to render.
                let mut app = App::new();
                app.snapshot.lines = (0..40).map(|i| format!("row {i}")).collect();
                app.scroll = 5;
                app
            }),
            ("metrics query + completion", {
                let mut app = App::new();
                app.route = Route::Metrics;
                app.query[1] = "rate(".to_owned();
                app.mode = Mode::Editing;
                app.completion = Some(Completion {
                    candidates: vec![Candidate {
                        text: "http_requests".to_owned(),
                        kind: CandidateKind::Metric,
                    }],
                    selected: 0,
                });
                app
            }),
            ("log detail", {
                let mut app = App::new();
                app.route = Route::LogDetail {
                    record: log_record.clone(),
                };
                app
            }),
            ("metric detail", {
                let mut app = App::new();
                app.route = Route::MetricDetail {
                    detail: metric_detail.clone(),
                };
                app
            }),
        ];

        for (label, app) in &states {
            let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
            terminal.draw(|frame| draw(frame, app, &options)).unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    let sym = buffer[(x, y)].symbol();
                    assert!(
                        sym.is_ascii(),
                        "non-ASCII cell {sym:?} at ({x},{y}) in --ascii state {label:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn mascot_overlay_renders_at_its_resting_position() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new();
        let area = Rect::new(0, 0, 40, 12);
        // Place the mascot against a real area (its initial bottom-right resting band), Active so it
        // does not wander off it.
        app.mascot.update(&[], &MascotCtx { area, chart: None });

        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| draw_mascot(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let symbol = |x: u16, y: u16| buffer[(x, y)].symbol().to_owned();

        // place(): x = right - (ART_WIDTH+1) = 31, y = bottom - (ART_HEIGHT+BOTTOM_MARGIN) = 7, so the
        // 8-wide middle art row sits at y=8, spanning cols 31..=38 with a one-column right margin.
        let row: String = (0..buffer.area.width).map(|x| symbol(x, 8)).collect();
        assert!(
            row.contains("▄█▄"),
            "middle art row expected at y=8, got {row:?}"
        );
        assert_ne!(symbol(31, 8), " "); // left edge of the 8-wide art
        assert_ne!(symbol(38, 8), " "); // flush right
        assert_eq!(symbol(39, 8), " "); // one-column right margin
        // The bottom two rows (where the status/hint bar lives) stay clear beneath the mascot.
        assert_eq!(symbol(38, 10), " ");
        assert_eq!(symbol(38, 11), " ");
    }

    #[test]
    fn mascot_overlay_is_skipped_on_a_tiny_terminal() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let app = App::new();
        // Too short to fit the 3 art rows above the 2-row status bar: nothing is drawn (no panic).
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|frame| draw_mascot(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let blank = (0..buffer.area.width)
            .flat_map(|x| (0..buffer.area.height).map(move |y| (x, y)))
            .all(|(x, y)| buffer[(x, y)].symbol() == " ");
        assert!(blank, "mascot must not draw when the terminal is too small");
    }

    #[test]
    fn mascot_rests_while_the_user_is_active() {
        // With no idle event, the controller withholds wander ticks and no motion runs, so the mascot
        // stays exactly where it was placed no matter how many redraws pass.
        let mut m = Mascot::new();
        let ctx = MascotCtx {
            area: Rect::new(0, 0, 80, 24),
            chart: None,
        };
        m.update(&[], &ctx); // first update places it
        let resting = m.body.x;
        for _ in 0..16 {
            m.update(&[], &ctx);
        }
        assert_eq!(m.body.x, resting, "active mascot must not wander");
    }

    #[test]
    fn idle_wander_stays_in_bounds_and_in_the_band() {
        let ctx = MascotCtx {
            area: Rect::new(0, 0, 80, 24),
            chart: None,
        };
        let band = ctx
            .area
            .bottom()
            .saturating_sub(MASCOT_ART_HEIGHT + MASCOT_BOTTOM_MARGIN) as f32;
        let lo = ctx.area.left() as f32;
        let hi = ctx.area.right().saturating_sub(MASCOT_ART_WIDTH) as f32;

        let mut w = IdleWander::new();
        let mut rng = SmallRng::seed_from_u64(7);
        let mut body = MascotBody {
            x: 40.0,
            y: 0.0,
            facing: 1,
            phase_ns: 0,
        };
        for _ in 0..600 {
            w.tick(100_000_000, &mut body, &ctx, &mut rng);
            assert!(
                body.x >= lo - 0.01 && body.x <= hi + 0.01,
                "x out of bounds: {}",
                body.x
            );
            assert!(
                (body.y - band).abs() < 0.5,
                "y left the resting band: {}",
                body.y
            );
            assert!(body.facing == 1 || body.facing == -1);
        }
    }

    #[test]
    fn great_displacement_path_finds_a_rideable_descent() {
        // Rows increase downward: a high point (row 2) then a descent to row 9 is the run to ride.
        let chart = ChartGeometry {
            graph: Rect::new(0, 0, 40, 12),
            cells: vec![(0, 8), (1, 8), (2, 2), (3, 3), (4, 7), (5, 9)],
        };
        let path = great_displacement_path(&chart).expect("a big drop should be found");
        assert_eq!(path.first(), Some(&(2, 2)), "ride starts at the peak");
        assert_eq!(path.last(), Some(&(5, 9)), "ride ends at the trough");
        assert_eq!(path.len(), 4);
    }

    #[test]
    fn great_displacement_path_can_descend_leftward() {
        // Peak on the RIGHT (row 1) with the descent falling off to the LEFT: the ride slides left, so
        // the path starts at the right-side peak and its x decreases toward the left-side trough.
        let chart = ChartGeometry {
            graph: Rect::new(0, 0, 40, 12),
            cells: vec![(0, 9), (1, 7), (2, 4), (3, 1)],
        };
        let path = great_displacement_path(&chart).expect("a leftward drop should be found");
        assert_eq!(path.first(), Some(&(3, 1)), "starts at the right-side peak");
        assert_eq!(path.last(), Some(&(0, 9)), "ends at the left-side trough");
        assert!(
            path.windows(2).all(|w| w[1].0 < w[0].0),
            "x must decrease (sliding left): {path:?}"
        );
    }

    #[test]
    fn great_displacement_path_prefers_the_gentler_slope() {
        // Two qualifying drops: a steep cliff (row 1→9 over one column) and a long gentle grade
        // (row 2→7 over five columns). The gentler grade is the one to slide down.
        let chart = ChartGeometry {
            graph: Rect::new(0, 0, 40, 12),
            cells: vec![
                (0, 1),
                (1, 9),
                (2, 2),
                (3, 3),
                (4, 4),
                (5, 5),
                (6, 6),
                (7, 7),
            ],
        };
        let path = great_displacement_path(&chart).expect("a rideable descent should be found");
        assert_eq!(
            path.first(),
            Some(&(2, 2)),
            "avoids the cliff, takes the gentle grade"
        );
        assert_eq!(path.last(), Some(&(7, 7)));
        assert_eq!(path.len(), 6);
    }

    #[test]
    fn great_displacement_path_ignores_a_flat_chart() {
        let chart = ChartGeometry {
            graph: Rect::new(0, 0, 40, 12),
            cells: vec![(0, 5), (1, 5), (2, 6), (3, 5), (4, 6)],
        };
        assert!(great_displacement_path(&chart).is_none());
    }

    #[test]
    fn chart_ride_jumps_then_slides_to_the_trough() {
        let path = vec![(10, 2), (11, 4), (12, 6), (13, 9)];
        let mut ride = ChartRide::new(path.clone());
        let ctx = MascotCtx {
            area: Rect::new(0, 0, 40, 12),
            chart: None,
        };
        let mut body = MascotBody {
            x: 5.0,
            y: 5.0,
            facing: 1,
            phase_ns: 0,
        };
        // Drive to completion (returns false at the trough).
        let mut alive = true;
        let mut guard = 0;
        while alive && guard < 10_000 {
            alive = ride.tick(20_000_000, &mut body, &ctx);
            guard += 1;
        }
        assert!(!alive, "the ride must finish");
        let (tx, ty) = ChartRide::body_at((13, 9));
        assert!((body.x - tx).abs() < 1.0, "ends near the trough column");
        assert!((body.y - ty).abs() < 1.0, "ends near the trough row");
    }

    #[test]
    fn chart_ride_holds_a_steady_downhill_facing() {
        let ctx = MascotCtx {
            area: Rect::new(0, 0, 60, 20),
            chart: None,
        };
        // A leftward descent (x decreases peak → trough): the facing must stay left for the whole slide,
        // never flipping on the vertical portions of the densified path.
        let mut ride = ChartRide::new(vec![(30, 1), (27, 4), (24, 7), (21, 10)]);
        let mut body = MascotBody {
            x: 5.0,
            y: 15.0,
            facing: 1,
            phase_ns: 0,
        };
        let mut alive = true;
        let mut saw_slide = false;
        let mut guard = 0;
        while alive && guard < 10_000 {
            alive = ride.tick(20_000_000, &mut body, &ctx);
            if ride.sliding {
                assert_eq!(body.facing, -1, "facing must stay left while sliding");
                saw_slide = true;
            }
            guard += 1;
        }
        assert!(saw_slide, "the slide phase must run");
    }

    #[test]
    fn chart_ride_slide_speed_tracks_steepness() {
        // The peak slide speed should be markedly higher on a steep pitch than on a gentle grade of the
        // same fall — the velocity follows the hill's disposition, not a fixed rate.
        let peak_speed = |path: Vec<(u16, u16)>| -> f32 {
            let ctx = MascotCtx {
                area: Rect::new(0, 0, 80, 40),
                chart: None,
            };
            let mut ride = ChartRide::new(path);
            let mut body = MascotBody {
                x: 0.0,
                y: 30.0,
                facing: 1,
                phase_ns: 0,
            };
            let mut max_v = 0.0f32;
            let mut alive = true;
            let mut guard = 0;
            while alive && guard < 100_000 {
                alive = ride.tick(16_000_000, &mut body, &ctx);
                if ride.sliding {
                    max_v = max_v.max(ride.speed);
                }
                guard += 1;
            }
            max_v
        };
        // Both fall 12 rows: the steep one over ~2 columns, the gentle one spread over ~24.
        let steep = peak_speed(vec![(10, 0), (12, 12)]);
        let gentle = peak_speed(vec![(0, 0), (24, 12)]);
        assert!(
            steep > gentle * 1.5,
            "steep slide ({steep}) should clearly outrun the gentle one ({gentle})"
        );
    }

    #[test]
    fn chart_ride_igniter_spends_one_roll_per_opportunity() {
        let chart = ChartGeometry {
            graph: Rect::new(0, 0, 40, 12),
            cells: vec![(2, 2), (3, 3), (4, 6), (5, 9)],
        };
        let ctx_none = MascotCtx {
            area: Rect::new(0, 0, 40, 20),
            chart: None,
        };
        let ctx_chart = MascotCtx {
            area: Rect::new(0, 0, 40, 20),
            chart: Some(chart),
        };
        let mut rng = SmallRng::seed_from_u64(1);
        let mut ig = ChartRideIgniter::new();

        // Unarmed: never fires, even with a rideable chart on screen.
        assert!(ig.poll(16_000_000, &ctx_chart, &mut rng).is_none());

        // Navigation arms it at the high (navigation) chance.
        ig.arm(&MascotEvent::Navigated {
            on_metric_chart: true,
        });
        assert!((ig.chance - ChartRideIgniter::NAV_CHANCE).abs() < 1e-9);

        // While the new view's geometry has not been drawn yet, the roll is NOT spent — it waits,
        // instead of firing (or fizzling) against a stale/absent chart.
        let _ = ig.poll(16_000_000, &ctx_none, &mut rng);
        assert!(!ig.rolled, "no geometry yet must not consume the roll");

        // Once the geometry is present the single roll is spent, and later polls stay quiet until re-armed.
        let _ = ig.poll(16_000_000, &ctx_chart, &mut rng);
        assert!(ig.rolled, "a real opportunity spends the roll");
        assert!(
            ig.poll(16_000_000, &ctx_chart, &mut rng).is_none(),
            "only one roll per arming"
        );

        // A background auto-refresh arms at the lower chance; non-qualifying events do not arm at all.
        let mut ig2 = ChartRideIgniter::new();
        ig2.arm(&MascotEvent::Refreshed { auto: true });
        assert!((ig2.chance - ChartRideIgniter::REFRESH_CHANCE).abs() < 1e-9);
        let mut ig3 = ChartRideIgniter::new();
        ig3.arm(&MascotEvent::Navigated {
            on_metric_chart: false,
        });
        ig3.arm(&MascotEvent::Refreshed { auto: false });
        ig3.arm(&MascotEvent::Idle);
        assert_eq!(
            ig3.armed_ns, 0,
            "only chart-nav / auto-refresh arm the ride"
        );
    }

    #[test]
    fn chart_ride_needs_at_least_two_points() {
        let mut ride = ChartRide::new(vec![(1, 1)]);
        let ctx = MascotCtx {
            area: Rect::new(0, 0, 40, 12),
            chart: None,
        };
        let mut body = MascotBody {
            x: 0.0,
            y: 0.0,
            facing: 1,
            phase_ns: 0,
        };
        assert!(!ride.tick(10_000_000, &mut body, &ctx));
    }

    #[test]
    fn chart_point_cell_matches_ratatui_corners() {
        // A block-inner region; `chart_graph_area` reserves the axis gutters inside it.
        let y_labels = ["0".to_owned(), "5".to_owned(), "10".to_owned()];
        let graph = chart_graph_area(Rect::new(0, 0, 40, 12), &y_labels, "12:00:00").unwrap();
        assert!(graph.width > 0 && graph.height > 0);
        assert!(graph.left() >= 1 && graph.bottom() <= 12);

        // Top-left of the data (x_min, y_max) maps to the graph origin; bottom-right (x_max, y_min) to
        // the far corner cell — the Braille 2×4 mapping ratatui uses.
        let tl = chart_point_cell(graph, 0.0, 10.0, 0.0, 10.0, 0.0, 10.0).unwrap();
        assert_eq!(tl, (graph.left(), graph.top()));
        let br = chart_point_cell(graph, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0).unwrap();
        assert_eq!(br, (graph.right() - 1, graph.bottom() - 1));
        // Out of the axis bounds → not plotted.
        assert!(chart_point_cell(graph, 0.0, 10.0, 0.0, 10.0, 20.0, 5.0).is_none());
    }

    #[test]
    fn chart_scaling_is_bounded() {
        assert_eq!(
            chart_values([0.0, 5.0, 10.0].into_iter()),
            vec![0, 500, 1000]
        );
    }

    #[test]
    fn timestamps_format_as_utc() {
        // 0 ns is the Unix epoch; a known instant checks the civil-date math and sub-second field.
        assert_eq!(format_timestamp_ns(0), "1970-01-01 00:00:00.000");
        // 2021-01-01T00:00:00.123Z == 1_609_459_200 s.
        assert_eq!(
            format_timestamp_ns(1_609_459_200_123_000_000),
            "2021-01-01 00:00:00.123"
        );
        // Before the epoch must not panic and must borrow correctly across the day boundary.
        assert_eq!(format_timestamp_ns(-1), "1969-12-31 23:59:59.999");
    }

    #[test]
    fn header_clock_drops_the_sub_second_field() {
        // The header clock is the same civil-date math without the millis suffix.
        assert_eq!(format_datetime_ns(0), "1970-01-01 00:00:00");
        assert_eq!(
            format_datetime_ns(1_609_459_200_123_000_000),
            "2021-01-01 00:00:00"
        );
    }

    #[test]
    fn clamp_field_pads_and_truncates_by_display_width() {
        // ASCII: padded to the exact cell count.
        assert_eq!(clamp_field("ab", 5), "ab   ");
        assert_eq!(UnicodeWidthStr::width(clamp_field("ab", 5).as_str()), 5);
        // Wide (CJK) glyphs count as two cells each: 3 chars == 6 cells, padded to 8.
        let jp = clamp_field("あいう", 8);
        assert_eq!(UnicodeWidthStr::width(jp.as_str()), 8);
        assert!(jp.starts_with("あいう"));
        // Truncation keeps the field exactly `width` cells including the ellipsis, never over. A wide
        // glyph straddling the boundary is dropped and the leftover cell is space-padded, so the
        // ellipsis is present but may be followed by a pad space rather than ending the string.
        let cut = clamp_field("あいうえお", 6);
        assert_eq!(UnicodeWidthStr::width(cut.as_str()), 6);
        assert!(cut.contains('…'));
        // An odd width leaves room for the ellipsis right at the end (no straddle).
        let cut_odd = clamp_field("あいうえお", 5);
        assert_eq!(UnicodeWidthStr::width(cut_odd.as_str()), 5);
        assert!(cut_odd.ends_with('…'));
    }

    fn waterfall_span(
        id: u8,
        parent: Option<u8>,
        name: &str,
        start_ns: i64,
        dur_ns: u64,
    ) -> imbh::Span {
        imbh::Span {
            trace_id: TraceId([0xaa; 16]),
            span_id: imbh::SpanId([id; 8]),
            parent_span_id: parent.map(|p| imbh::SpanId([p; 8])),
            name: name.to_owned(),
            kind: "internal".to_owned(),
            start_time: Timestamp(start_ns),
            duration_ns: imbh::DurationNs(dur_ns),
            status_code: "OK".to_owned(),
            status_message: None,
            service: None,
            attributes: Attributes::new(),
            resource: Attributes::new(),
            scope: Attributes::new(),
            events: None,
            links: None,
            trace_state: None,
            flags: 0,
        }
    }

    #[test]
    fn waterfall_bars_align_regardless_of_depth_or_wide_names() {
        // A root plus a nested child with a CJK name: the child indents, but the `|bar|` axis must
        // start at the same terminal column on both rows.
        let trace = imbh::Trace {
            trace_id: TraceId([0xaa; 16]),
            root_service: None,
            root_name: Some("root".to_owned()),
            start_time: Timestamp(0),
            duration_ns: imbh::DurationNs(1_000_000),
            spans: vec![
                waterfall_span(1, None, "root", 0, 1_000_000),
                waterfall_span(2, Some(1), "データベース照会", 200_000, 400_000),
            ],
        };
        // Render the width-independent rows at two different bar widths: alignment must hold at any
        // size, and the bar must actually stretch to the width it is given.
        let waterfall = build_waterfall(&trace, true);
        for cells in [40usize, 77] {
            let lines = render_waterfall(&waterfall, cells);
            assert_eq!(lines.len(), 2);
            // Everything before the first `|` is a constant width across rows, so the bars line up:
            // marker (1) + name field (WATERFALL_NAME_W == 20).
            let axis = |line: &str| UnicodeWidthStr::width(line.split('|').next().unwrap());
            assert_eq!(axis(&lines[0]), axis(&lines[1]));
            assert_eq!(axis(&lines[0]), 1 + 20);
            // The bar (between the two `|`) fills exactly `cells` cells, so the closing `|` and the
            // trailing duration column also line up.
            let bar = |line: &str| UnicodeWidthStr::width(line.split('|').nth(1).unwrap());
            assert_eq!(bar(&lines[0]), cells);
            assert_eq!(bar(&lines[1]), cells);
        }
    }

    #[test]
    fn highlight_reconstructs_the_input_and_colours_calls() {
        let query = "rate({job=\"api\"}[5m])";
        let spans = highlight_query(Screen::Metrics, query, &Glyphs::new(false));
        // Highlighting is presentation-only: concatenating the spans must reproduce the input exactly.
        let rebuilt = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rebuilt, query);
        // The leading `rate` is a function call (followed by `(`) and must be coloured.
        assert_eq!(spans[0].content.as_ref(), "rate");
        assert_eq!(spans[0].style.fg, Some(Color::Cyan));
        // The quoted value is a string span.
        assert!(
            spans
                .iter()
                .any(|span| span.content.as_ref() == "\"api\""
                    && span.style.fg == Some(Color::Green))
        );
    }

    #[test]
    fn menu_cursor_wraps_over_screens_and_the_range_item() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.open_menu();
        assert_eq!(app.mode, Mode::Menu);
        // Starts on the current screen.
        assert_eq!(app.menu_cursor, Screen::Traces.index());
        assert_eq!(app.menu_screen(), Some(Screen::Traces));
        // Right past Logs reaches the trailing range item, then wraps to Overview.
        app.menu_move(1);
        assert_eq!(app.menu_screen(), Some(Screen::Logs));
        app.menu_move(1);
        assert_eq!(app.menu_screen(), None); // the range item
        app.menu_move(1);
        assert_eq!(app.menu_screen(), Some(Screen::Overview));
        // Left from Overview wraps back to the range item.
        app.menu_move(-1);
        assert_eq!(app.menu_screen(), None);
    }

    #[test]
    fn focus_ring_cycles_menu_items_time_selector_and_panes_on_a_list_screen() {
        let mut app = App::new();
        app.route = Route::Metrics; // has a query pane -> the full ring incl. the four menu items
        assert_eq!(app.focus, Focus::Primary);
        // Tab (delta +1) walks reading order and wraps: Primary -> Menu(0..4) -> TimeRange -> Query.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));
        for expected in 1..Screen::ORDER.len() {
            app.focus_advance(1);
            assert_eq!(app.focus, Focus::Menu(expected));
        }
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::TimeRange);
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Query);
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Primary);
        // Shift+Tab (delta -1) walks the other way: Primary -> Query -> TimeRange -> last menu item.
        app.focus_advance(-1);
        assert_eq!(app.focus, Focus::Query);
        app.focus_advance(-1);
        assert_eq!(app.focus, Focus::TimeRange);
        app.focus_advance(-1);
        assert_eq!(app.focus, Focus::Menu(Screen::ORDER.len() - 1));
    }

    #[test]
    fn focus_ring_omits_the_query_stop_without_a_query_pane() {
        // Overview has no query pane: the ring is the menu items, the time selector, then Primary.
        let mut app = App::new();
        assert_eq!(app.route.screen(), Screen::Overview);
        assert!(!app.has_query());
        // Step to the time selector, then one more lands on Primary (no Query stop in between).
        app.focus = Focus::TimeRange;
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Primary);
        // Wrapping forward from Primary reaches the first menu item.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));

        // A detail route also drops the query stop, and a stale Query focus reads as Primary so the
        // highlight and Enter never target a pane that is not shown.
        app.route = Route::LogDetail {
            record: sample_log_record(None),
        };
        app.focus = Focus::Query;
        assert!(!app.has_query());
        assert_eq!(app.effective_focus(), Focus::Primary);
        // Advancing anchors on the effective focus, so it steps to the first menu item, not Query.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));
    }

    #[test]
    fn menubar_move_cycles_the_screen_items_and_time_selector() {
        let mut app = App::new();
        app.focus = Focus::Menu(0);
        // Right walks the four screen items, then the trailing time selector, then wraps.
        for expected in 1..Screen::ORDER.len() {
            app.menubar_move(1);
            assert_eq!(app.focus, Focus::Menu(expected));
        }
        app.menubar_move(1);
        assert_eq!(app.focus, Focus::TimeRange);
        app.menubar_move(1);
        assert_eq!(app.focus, Focus::Menu(0));
        // Left from the first item wraps back to the time selector.
        app.menubar_move(-1);
        assert_eq!(app.focus, Focus::TimeRange);

        // Inert unless the ring is on a menu-bar stop: on a pane it leaves the focus untouched.
        app.focus = Focus::Primary;
        app.menubar_move(1);
        assert_eq!(app.focus, Focus::Primary);
    }

    #[test]
    fn navigation_resets_focus_to_the_primary_pane() {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.focus = Focus::TimeRange;
        // restore_nav (back/forward) drops transient chrome, including a non-Primary focus.
        let entry = app.capture_nav();
        app.restore_nav(entry);
        assert_eq!(app.focus, Focus::Primary);
    }

    #[test]
    fn back_forward_history_moves_through_visited_views() {
        let mut app = App::new();
        app.route = Route::Metrics; // view A: catalog (empty query)
        assert!(!app.go_back(), "no history yet");

        // Forward A -> B (series list): record A, then mutate to B.
        app.push_history();
        app.query[1] = "up".to_owned();
        app.selected = 3;
        // Forward B -> C (series detail): record B, then mutate to C.
        app.push_history();
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "svc=a".into(),
                query: "up".into(),
                points: vec![(1, 1.0)],
            },
        };

        // Back C -> B -> A.
        assert!(app.go_back());
        assert!(matches!(app.route, Route::Metrics));
        assert_eq!(app.active_query(), "up");
        assert_eq!(app.selected, 3);
        assert!(app.route_metric_detail().is_none());
        assert!(app.go_back());
        assert_eq!(app.active_query(), "");
        assert!(!app.go_back(), "at the oldest view");

        // Forward A -> B -> C redoes the Backs.
        assert!(app.go_forward());
        assert_eq!(app.active_query(), "up");
        assert!(app.go_forward());
        assert_eq!(
            app.route_metric_detail().map(|d| d.labels.as_str()),
            Some("svc=a")
        );
        assert!(!app.go_forward(), "at the newest view");

        // A fresh forward navigation invalidates the Forward stack (a new branch).
        assert!(app.go_back()); // back to B
        app.push_history();
        app.route = Route::Metrics;
        assert!(
            !app.go_forward(),
            "a new navigation clears the redo history"
        );
    }

    #[test]
    fn route_maps_to_screen_and_reports_detail() {
        assert_eq!(Route::Overview.screen(), Screen::Overview);
        assert_eq!(Route::Metrics.screen(), Screen::Metrics);
        assert_eq!(Route::Traces.screen(), Screen::Traces);
        assert_eq!(Route::Logs.screen(), Screen::Logs);
        assert!(!Route::Metrics.is_detail());
        assert!(!Route::Logs.is_detail());

        // Detail routes belong to their parent screen and report `is_detail`.
        let md = Route::MetricDetail {
            detail: MetricDetail {
                labels: "svc=a".into(),
                query: "up".into(),
                points: vec![(1, 1.0)],
            },
        };
        assert_eq!(md.screen(), Screen::Metrics);
        assert!(md.is_detail());
        let ld = Route::LogDetail {
            record: sample_log_record(None),
        };
        assert_eq!(ld.screen(), Screen::Logs);
        assert!(ld.is_detail());

        // `list` round-trips through `screen`.
        for screen in [
            Screen::Overview,
            Screen::Metrics,
            Screen::Traces,
            Screen::Logs,
        ] {
            assert_eq!(Route::list(screen).screen(), screen);
        }
    }

    #[test]
    fn auto_refresh_is_off_by_default() {
        assert!(!App::new().auto_refresh);
    }

    #[test]
    fn selectable_bounds_track_the_list_region() {
        let mut app = App::new();
        // Not a list -> no selection, keys scroll instead.
        app.snapshot.list_from = None;
        assert_eq!(app.selectable_bounds(), None);

        // Header line 0, three selectable rows at 1..=3.
        app.snapshot.lines = vec!["header".into(), "a".into(), "b".into(), "c".into()];
        app.snapshot.list_from = Some(1);
        assert_eq!(app.selectable_bounds(), Some((1, 3)));

        // Header-only list (0 matches) has no selectable rows.
        app.snapshot.lines = vec!["header".into()];
        app.snapshot.list_from = Some(1);
        assert_eq!(app.selectable_bounds(), None);
    }

    #[test]
    fn apply_lands_the_cursor_on_the_first_selectable_row() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.generation = 7;
        app.selected = 0;
        let snapshot = Snapshot {
            lines: vec!["header".into(), "a".into(), "b".into(), "c".into()],
            list_from: Some(1),
            ..Default::default()
        };
        app.apply(QueryResult {
            generation: 7,
            screen: Screen::Traces,
            result: Ok(snapshot),
        });
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn cursor_moves_within_bounds_and_falls_back_to_scroll() {
        let mut app = App::new();
        app.snapshot.lines = vec!["header".into(), "a".into(), "b".into(), "c".into()];
        app.snapshot.list_from = Some(1);
        app.selected = 1; // as `apply` would have placed it (first selectable row)

        move_selection(&mut app, 1);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, 10); // saturates at the last row
        assert_eq!(app.selected, 3);
        move_selection(&mut app, -1);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, -10); // saturates at the first row
        assert_eq!(app.selected, 1);

        // Without a list, movement scrolls instead of selecting.
        app.snapshot.list_from = None;
        app.max_scroll.set(5);
        app.scroll = 0;
        move_selection(&mut app, 3);
        assert_eq!(app.scroll, 3);
        move_selection(&mut app, 9); // clamped to max_scroll
        assert_eq!(app.scroll, 5);
    }

    fn sample_log_record(trace: Option<&str>) -> LogRecord {
        LogRecord {
            time_ns: 1_609_459_200_000_000_000,
            severity: "INFO (9)".into(),
            service: Some("api".into()),
            body: "hello\nworld".into(),
            trace_id: trace.map(str::to_owned),
            span_id: Some("aabbccdd11223344".into()),
            attributes: vec![("http.method".into(), "GET".into())],
            resource: vec![("service.name".into(), "api".into())],
            scope: vec![],
        }
    }

    #[test]
    fn table_selection_bounds_index_the_rows() {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.snapshot.table = Some(TableData {
            header: vec!["Metric".into(), "Kind".into()],
            rows: vec![
                vec!["a".into(), "gauge".into()],
                vec!["b".into(), "sum".into()],
                vec!["c".into(), "sum".into()],
            ],
        });
        // Table rows are indexed from 0 (not offset by a header line as lists are).
        assert_eq!(app.selectable_bounds(), Some((0, 2)));

        app.selected = 0;
        move_selection(&mut app, 2);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, 5); // saturates at the last row
        assert_eq!(app.selected, 2);

        // An empty table has no selectable rows.
        app.snapshot.table = Some(TableData {
            header: vec!["Metric".into()],
            rows: vec![],
        });
        assert_eq!(app.selectable_bounds(), None);
    }

    fn catalog_app() -> App {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.query[1] = String::new(); // empty query -> catalog is showing
        app.snapshot.table = Some(TableData {
            header: vec![
                "Metric".into(),
                "Kind".into(),
                "Unit".into(),
                "Temporality".into(),
            ],
            rows: vec![
                vec!["cpu".into(), "gauge".into(), "1".into(), "-".into()],
                vec!["reqs".into(), "sum".into(), "1".into(), "cumulative".into()],
            ],
        });
        app.build_metric_tree();
        app
    }

    #[test]
    fn catalog_tree_expands_metrics_and_dimensions() {
        let mut app = catalog_app();
        assert_eq!(app.snapshot.table.as_ref().unwrap().rows.len(), 2);
        assert_eq!(
            app.tree_rows,
            vec![TreeRowRef::Metric(0), TreeRowRef::Metric(1)]
        );

        // Expand the second metric (reqs); dims not loaded yet -> a placeholder row appears.
        app.selected = 1;
        let load = app.toggle_node();
        assert_eq!(load, Some(("reqs".to_owned(), "sum".to_owned())));
        assert_eq!(app.snapshot.table.as_ref().unwrap().rows.len(), 3);

        // Dimensions arrive; the placeholder becomes a dimension row.
        app.apply_metric_dims(
            "reqs",
            vec![DimNode {
                label: "service".into(),
                values: vec!["cart".into(), "checkout".into()],
                expanded: false,
                selected: None,
            }],
        );
        assert_eq!(
            app.tree_rows,
            vec![
                TreeRowRef::Metric(0),
                TreeRowRef::Metric(1),
                TreeRowRef::Dim(1, 0),
            ]
        );

        // Expand the dimension to reveal its two values.
        app.selected = 2;
        assert_eq!(app.toggle_node(), None); // dim expand needs no fetch
        assert_eq!(
            app.tree_rows,
            vec![
                TreeRowRef::Metric(0),
                TreeRowRef::Metric(1),
                TreeRowRef::Dim(1, 0),
                TreeRowRef::Value(1, 0, 0),
                TreeRowRef::Value(1, 0, 1),
            ]
        );
    }

    #[test]
    fn value_checkbox_is_exclusive_and_drives_the_query() {
        let mut app = catalog_app();
        app.apply_metric_dims(
            "reqs",
            vec![DimNode {
                label: "service".into(),
                values: vec!["cart".into(), "checkout".into()],
                expanded: true,
                selected: None,
            }],
        );
        app.metric_tree[1].expanded = true;
        app.rebuild_catalog_table();
        // rows: [cpu, reqs, by service, service=cart, service=checkout]

        // Nothing checked: the metric row visualizes the whole metric.
        app.selected = 1;
        assert_eq!(app.visualize_queries(), vec!["rate(reqs[5m])".to_owned()]);

        // Nothing checked, cursor on the "by service" row: group by service.
        app.selected = 2;
        assert_eq!(
            app.visualize_queries(),
            vec!["sum by (service) (rate(reqs[5m]))".to_owned()]
        );

        // Check service=cart (Space on that value row).
        app.selected = 3;
        assert_eq!(app.toggle_node(), None);
        assert_eq!(
            app.metric_tree[1].dims.as_ref().unwrap()[0].selected,
            Some(0)
        );
        // A checked series *is* the selection: it drives the visualization regardless of where the
        // cursor sits (here the "by service" row), filtered by the checked value.
        app.selected = 2;
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );

        // Checking a different value in the same axis replaces the first (exclusive).
        app.selected = 4;
        app.toggle_node();
        assert_eq!(
            app.metric_tree[1].dims.as_ref().unwrap()[0].selected,
            Some(1)
        );

        // Toggling the checked value off clears it, so the cursor drives it again.
        app.toggle_node();
        assert_eq!(app.metric_tree[1].dims.as_ref().unwrap()[0].selected, None);
        app.selected = 1;
        assert_eq!(app.visualize_queries(), vec!["rate(reqs[5m])".to_owned()]);
    }

    #[test]
    fn checked_series_visualize_their_metrics_together() {
        let mut app = catalog_app(); // cpu (gauge), reqs (sum)

        // Nothing checked: visualize only the node under the cursor.
        app.selected = 0;
        assert_eq!(app.visualize_queries(), vec!["cpu".to_owned()]);

        // Mark a series under each metric (a checked value = an implicit metric selection).
        app.metric_tree[0].dims = Some(vec![DimNode {
            label: "host".into(),
            values: vec!["node-a".into()],
            expanded: false,
            selected: Some(0),
        }]);
        app.metric_tree[1].dims = Some(vec![DimNode {
            label: "service".into(),
            values: vec!["cart".into()],
            expanded: false,
            selected: Some(0),
        }]);

        // Both metrics are visualized together, each filtered by its own checked series and using its
        // own kind-appropriate transform (gauge as-is, sum as a rate). Cursor position is irrelevant.
        app.selected = 0;
        assert_eq!(
            app.visualize_queries(),
            vec![
                "cpu{host=\"node-a\"}".to_owned(),
                "rate(reqs{service=\"cart\"}[5m])".to_owned(),
            ]
        );

        // Clearing cpu's series drops it from the selection.
        app.metric_tree[0].dims.as_mut().unwrap()[0].selected = None;
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );
    }

    #[test]
    fn no_dimensions_checkbox_selects_the_whole_metric() {
        let mut app = catalog_app(); // cpu (gauge), reqs (sum)
        // cpu has no dimensions; expanding it shows the "(no dimensions)" checkbox row.
        app.metric_tree[0].dims = Some(vec![]);
        app.metric_tree[0].expanded = true;
        app.rebuild_catalog_table();
        assert_eq!(
            app.tree_rows,
            vec![
                TreeRowRef::Metric(0),
                TreeRowRef::NoDims(0),
                TreeRowRef::Metric(1),
            ]
        );
        assert!(
            app.snapshot.table.as_ref().unwrap().rows[1][0].contains("[ ] (no dimensions)"),
            "unchecked box shown alongside the message"
        );

        // Space on that row checks the whole metric.
        app.selected = 1;
        assert_eq!(app.toggle_node(), None);
        assert!(app.metric_tree[0].whole_selected);
        assert!(app.snapshot.table.as_ref().unwrap().rows[1][0].contains("[x]"));

        // The whole-metric checkbox drives the visualization regardless of the cursor.
        app.selected = 2; // cursor on reqs
        assert_eq!(app.visualize_queries(), vec!["cpu".to_owned()]);

        // Unchecking it returns to the node under the cursor.
        app.selected = 1;
        app.toggle_node();
        assert!(!app.metric_tree[0].whole_selected);
        app.selected = 2;
        assert_eq!(app.visualize_queries(), vec!["rate(reqs[5m])".to_owned()]);
    }

    #[test]
    fn catalog_selection_survives_navigating_away_and_back() {
        let mut app = catalog_app(); // cpu (gauge), reqs (sum)
        // Discover reqs' dimensions, check a series, and expand it (the state before visualizing).
        app.apply_metric_dims(
            "reqs",
            vec![DimNode {
                label: "service".into(),
                values: vec!["cart".into(), "checkout".into()],
                expanded: true,
                selected: Some(0),
            }],
        );
        app.metric_tree[1].expanded = true;
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );

        // Navigate away to the series list and back: a fresh catalog snapshot arrives (raw rows) and
        // the tree is rebuilt. Expansion, discovered dims, and the checked series must all survive.
        app.snapshot.table = Some(TableData {
            header: vec![
                "Metric".into(),
                "Kind".into(),
                "Unit".into(),
                "Temporality".into(),
            ],
            rows: vec![
                vec!["cpu".into(), "gauge".into(), "1".into(), "-".into()],
                vec!["reqs".into(), "sum".into(), "1".into(), "cumulative".into()],
            ],
        });
        app.build_metric_tree();

        assert!(app.metric_tree[1].expanded, "expansion preserved");
        let dims = app.metric_tree[1].dims.as_ref().expect("dims preserved");
        assert_eq!(dims[0].selected, Some(0), "checked series preserved");
        assert_eq!(
            app.visualize_queries(),
            vec!["rate(reqs{service=\"cart\"}[5m])".to_owned()]
        );
    }

    #[test]
    fn build_metric_query_covers_kinds_matchers_and_group_by() {
        // Whole metric (no matchers, no group-by) reproduces the kind's base expression.
        assert_eq!(
            build_metric_query("temperature", "gauge", &[], None),
            "temperature"
        );
        assert_eq!(
            build_metric_query("reqs", "sum", &[], None),
            "rate(reqs[5m])"
        );
        // Group-by.
        assert_eq!(
            build_metric_query("cpu", "gauge", &[], Some("host")),
            "avg by (host) (cpu)"
        );
        assert_eq!(
            build_metric_query("lat", "histogram", &[], Some("service")),
            "histogram_quantile(0.95, sum by (service, le) (rate(lat_bucket[5m])))"
        );
        // Matchers combine across axes.
        assert_eq!(
            build_metric_query("cpu", "gauge", &[("service", "cart"), ("host", "a")], None),
            "cpu{service=\"cart\",host=\"a\"}"
        );
    }

    #[test]
    fn metric_values_format_compactly() {
        assert_eq!(format_metric_value(42.0), "42");
        assert_eq!(format_metric_value(2.53125), "2.5312");
        assert_eq!(format_metric_value(f64::NAN), "NaN");
        assert_eq!(format_metric_value(f64::INFINITY), "+Inf");
        assert_eq!(format_metric_value(f64::NEG_INFINITY), "-Inf");
    }

    #[test]
    fn selected_log_record_indexes_by_row() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.snapshot.lines = vec!["header".into(), "row a".into(), "row b".into()];
        app.snapshot.list_from = Some(1);
        app.snapshot.log_records = vec![sample_log_record(Some("dead")), sample_log_record(None)];

        app.selected = 1;
        assert_eq!(
            app.selected_log_record().map(|r| r.trace_id.clone()),
            Some(Some("dead".to_owned()))
        );
        app.selected = 2;
        assert_eq!(
            app.selected_log_record().map(|r| r.trace_id.clone()),
            Some(None)
        );

        // Not the Logs screen -> no record.
        app.route = Route::Traces;
        assert!(app.selected_log_record().is_none());
    }

    fn metrics_app_with_series() -> App {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.query[1] = "up".to_owned(); // non-empty query -> series view (not the catalog)
        app.snapshot.table = Some(TableData {
            header: vec!["Series".into(), "Latest".into()],
            rows: vec![vec!["a".into()], vec!["b".into()]],
        });
        app.snapshot.series = vec![
            SeriesData {
                labels: "svc=a".into(),
                points: vec![(10, 1.0), (20, 2.0)],
            },
            SeriesData {
                labels: "svc=b".into(),
                points: vec![(10, 3.0), (20, 4.0), (30, 5.0)],
            },
        ];
        app
    }

    #[test]
    fn open_metric_detail_selects_the_row_and_starts_at_latest() {
        let mut app = metrics_app_with_series();
        app.selected = 1; // the second series
        assert!(app.open_metric_detail());
        let detail = app.route_metric_detail().expect("detail route");
        assert_eq!(detail.labels, "svc=b");
        assert_eq!(detail.points.len(), 3);
        assert_eq!(detail.query, "up");
        assert_eq!(app.metric_cursor, 2, "cursor starts at the latest point");
    }

    #[test]
    fn open_metric_detail_is_a_noop_without_a_series() {
        // The catalog view (empty query, no retained series) must not open the viewer.
        let mut app = App::new();
        app.route = Route::Metrics;
        app.snapshot.table = Some(TableData {
            header: vec!["Metric".into()],
            rows: vec![vec!["http.requests".into()]],
        });
        app.selected = 0;
        assert!(!app.open_metric_detail());
        assert!(app.route_metric_detail().is_none());
    }

    #[test]
    fn clock_hms_formats_time_of_day() {
        assert_eq!(clock_hms_ns(0), "00:00:00");
        // 2021-01-01T00:00:00Z + 1h1m1s.
        assert_eq!(
            clock_hms_ns(1_609_459_200_000_000_000 + 3_661_000_000_000),
            "01:01:01"
        );
    }

    #[test]
    fn log_detail_lines_show_the_trace_and_body() {
        let lines = log_detail_lines(&sample_log_record(Some("abc123")));
        let text = lines.join("\n");
        assert!(text.contains("Trace ID  abc123"));
        assert!(text.contains("Span ID   aabbccdd11223344"));
        assert!(text.contains("Severity  INFO (9)"));
        // Body split across lines and indented.
        assert!(lines.iter().any(|l| l == "  hello"));
        assert!(lines.iter().any(|l| l == "  world"));
        // Attribute section present.
        assert!(text.contains("Attributes (1)"));
        assert!(text.contains("  http.method = GET"));

        // No trace id renders "(none)".
        let none = log_detail_lines(&sample_log_record(None)).join("\n");
        assert!(none.contains("Trace ID  (none)"));
    }

    #[test]
    fn focus_select_trace_lands_on_the_matching_row_and_clears_focus() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.snapshot.lines = vec![
            "2 matching traces".into(),
            "aaaa selected=1".into(),
            "bbbb selected=2".into(),
        ];
        app.snapshot.list_from = Some(1);

        app.focus_trace_id = Some("bbbb".into());
        app.selected = 1;
        app.focus_select_trace();
        assert_eq!(app.selected, 2);
        assert_eq!(app.focus_trace_id, None);

        // A focus not present in the list is kept (waterfall still shows it) and selection unchanged.
        app.focus_trace_id = Some("zzzz".into());
        app.selected = 1;
        app.focus_select_trace();
        assert_eq!(app.selected, 1);
        assert_eq!(app.focus_trace_id.as_deref(), Some("zzzz"));
    }

    #[test]
    fn severity_labels_map_number_bands() {
        assert_eq!(severity_label(SeverityNumber(9)), "INFO (9)");
        assert_eq!(severity_label(SeverityNumber(17)), "ERROR (17)");
        assert_eq!(severity_label(SeverityNumber(0)), "UNSET (0)");
    }

    #[test]
    fn selected_trace_id_reads_the_highlighted_row() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.snapshot.lines = vec![
            "2 matching traces".into(),
            "aabbccdd selected=1,2".into(),
            "eeff0011 selected=3".into(),
        ];
        app.snapshot.list_from = Some(1);

        app.selected = 1;
        assert_eq!(app.selected_trace_id().as_deref(), Some("aabbccdd"));
        app.selected = 2;
        assert_eq!(app.selected_trace_id().as_deref(), Some("eeff0011"));

        // Only the Traces screen resolves a trace id from the selected row.
        app.route = Route::Logs;
        assert_eq!(app.selected_trace_id(), None);
    }

    #[test]
    fn narrowing_starts_shrink_the_window_toward_the_end() {
        // Each step halves the span from `end`, so starts increase monotonically toward `end`.
        assert_eq!(narrowing_starts(0, 800, 4), vec![400, 600, 700, 750]);
        // A zero-width window yields nothing to try.
        assert_eq!(narrowing_starts(100, 100, 4), Vec::<i64>::new());
        // Steps stop once the span rounds down to zero rather than emitting `end` (an empty window).
        assert_eq!(narrowing_starts(0, 4, 8), vec![2, 3]);
    }

    #[test]
    fn wrapped_rows_accounts_for_width() {
        assert_eq!(wrapped_rows("", 10), 1); // empty line still occupies a row
        assert_eq!(wrapped_rows("hello", 10), 1);
        assert_eq!(wrapped_rows("0123456789abc", 10), 2); // 13 chars over 10 cols -> 2 rows
        assert_eq!(wrapped_rows("anything", 0), 1); // zero width degrades gracefully
    }

    #[test]
    fn time_range_selection_updates_lookback_and_step() {
        let mut app = App::new();
        app.range_index = 0; // 5m
        assert_eq!(app.lookback(), Duration::from_secs(300));
        assert_eq!(app.step(), Duration::from_secs(5));
        assert_eq!(app.range_label(), "5m");
    }

    #[test]
    fn parse_datetime_round_trips_and_validates() {
        // Round-trips the header formatter for whole-second instants (epoch and a known date).
        for ns in [0i64, 1_609_459_200_000_000_000, 1_763_000_000_000_000_000] {
            assert_eq!(parse_datetime(&format_datetime_ns(ns)), Some(ns));
        }
        // The time part is optional (midnight) and `T` is accepted as the separator.
        assert_eq!(parse_datetime("1970-01-01"), Some(0));
        assert_eq!(
            parse_datetime("2021-01-01T00:00:00"),
            Some(1_609_459_200_000_000_000)
        );
        assert_eq!(
            parse_datetime("2021-01-01 00:01"),
            Some(1_609_459_260_000_000_000)
        );
        // Malformed or out-of-range fields are rejected.
        assert_eq!(parse_datetime(""), None);
        assert_eq!(parse_datetime("2021-13-01 00:00:00"), None); // month 13
        assert_eq!(parse_datetime("2021-01-01 24:00:00"), None); // hour 24
        assert_eq!(parse_datetime("2021-01-01 00:60:00"), None); // minute 60
        assert_eq!(parse_datetime("not-a-date"), None);
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for days in [-40_000i64, -719_468, -1, 0, 1, 18_628, 50_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn absolute_window_overrides_rolling_and_summarizes() {
        let mut app = App::new();
        // Rolling by default: the summary reads "last <preset>".
        assert!(app.abs_window.is_none());
        let g = Glyphs::new(false);
        assert_eq!(app.range_summary(&g), "last 15m");
        // A committed absolute window drives the effective window and a same-day summary collapses the
        // shared date.
        let start = parse_datetime("2026-07-21 14:00:00").unwrap();
        let end = parse_datetime("2026-07-21 14:30:00").unwrap();
        app.abs_window = Some((start, end));
        assert_eq!(app.effective_window(), (start, end));
        assert_eq!(app.range_summary(&g), "2026-07-21 14:00:00 → 14:30:00");
    }

    #[test]
    fn commit_absolute_requires_a_valid_ordered_window() {
        let mut app = App::new();
        app.mode = Mode::AbsoluteRange;
        // start >= end is rejected and keeps the form open with an error.
        app.abs_start = "2026-07-21 15:00:00".to_owned();
        app.abs_end = "2026-07-21 14:00:00".to_owned();
        assert!(!app.commit_absolute());
        assert_eq!(app.mode, Mode::AbsoluteRange);
        assert!(app.abs_error.is_some());
        assert!(app.abs_window.is_none());
        // A well-formed ordered window commits, clears the error, and returns to Normal.
        app.abs_end = "2026-07-21 16:00:00".to_owned();
        assert!(app.commit_absolute());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.abs_error.is_none());
        assert_eq!(
            app.abs_window,
            Some((
                parse_datetime("2026-07-21 15:00:00").unwrap(),
                parse_datetime("2026-07-21 16:00:00").unwrap()
            ))
        );
    }

    #[test]
    fn eval_window_honors_the_absolute_window() {
        let start = parse_datetime("2026-07-21 00:00:00").unwrap();
        let end = parse_datetime("2026-07-21 02:00:00").unwrap();
        let options = Options {
            window: Some((start, end)),
            ..Options::default()
        };
        let (got_start, got_end, range, _limits) = eval_window(&options);
        assert_eq!((got_start, got_end), (start, end));
        assert_eq!(range.start_ns, start);
        assert_eq!(range.end_ns, end);
        // 2h span -> ~120 points -> 60s step, and never below 1s.
        assert_eq!(range.step_ns, 60 * 1_000_000_000);
    }

    #[test]
    fn current_token_is_the_trailing_identifier() {
        assert_eq!(current_token(""), "");
        assert_eq!(current_token("rate(htt"), "htt");
        assert_eq!(current_token("sum by (inst"), "inst");
        assert_eq!(current_token("{job=\"api\"}"), ""); // ends on punctuation -> no token
        assert_eq!(current_token("http_requests_total"), "http_requests_total");
    }

    #[test]
    fn completion_ranks_metrics_then_functions() {
        let metrics = vec![
            "http_requests_total".to_owned(),
            "http_errors".to_owned(),
            "process_cpu".to_owned(),
        ];
        let candidates = completion_candidates(
            Screen::Metrics,
            &metrics,
            &[],
            &[],
            &HashMap::new(),
            &CompletionContext::Expr,
            "htt",
        );
        let texts = candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        // Both matching metrics, sorted, and no non-matching one.
        assert_eq!(texts, vec!["http_errors", "http_requests_total"]);
        assert!(candidates.iter().all(|c| c.kind == CandidateKind::Metric));

        // Functions are offered when the prefix matches one (`rate`), even with no metric vocabulary.
        let funcs = completion_candidates(
            Screen::Metrics,
            &[],
            &[],
            &[],
            &HashMap::new(),
            &CompletionContext::Expr,
            "rat",
        );
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].text, "rate");
        assert_eq!(funcs[0].kind, CandidateKind::Function);
    }

    #[test]
    fn accepting_a_function_appends_a_paren_and_a_metric_does_not() {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.mode = Mode::Editing;
        app.metric_names = vec!["http_requests_total".to_owned()];

        // Function completion appends `(`.
        *app.active_query_mut() = "rat".to_owned();
        app.refresh_completion();
        app.accept_completion();
        assert_eq!(app.active_query(), "rate(");

        // Metric completion replaces the token verbatim.
        *app.active_query_mut() = "rate(htt".to_owned();
        app.refresh_completion();
        app.accept_completion();
        assert_eq!(app.active_query(), "rate(http_requests_total");
    }

    #[test]
    fn completion_is_suppressed_outside_edit_mode_and_for_exact_matches() {
        let mut app = App::new();
        app.route = Route::Metrics;
        // Not editing -> never suggest.
        *app.active_query_mut() = "rat".to_owned();
        app.refresh_completion();
        assert!(app.completion.is_none());

        // Editing, but the token already equals the only candidate -> nothing more to offer.
        app.mode = Mode::Editing;
        *app.active_query_mut() = "rate".to_owned();
        app.refresh_completion();
        assert!(app.completion.is_none());
    }

    #[test]
    fn completion_context_classifies_the_caret_position() {
        // Expression position.
        assert_eq!(completion_context("rate(htt").0, CompletionContext::Expr);
        // Inside a matcher block, writing a label name after the metric.
        assert_eq!(
            completion_context("http_requests_total{ser").0,
            CompletionContext::LabelName {
                metric: Some("http_requests_total".to_owned())
            }
        );
        // A bare selector has no metric.
        assert_eq!(
            completion_context("{ser").0,
            CompletionContext::LabelName { metric: None }
        );
        // After a comma, still a label name.
        assert_eq!(
            completion_context("m{a=\"1\",ho").0,
            CompletionContext::LabelName {
                metric: Some("m".to_owned())
            }
        );
        // Inside a quoted value, the label is captured.
        assert_eq!(
            completion_context("http_requests_total{service=\"ca").0,
            CompletionContext::LabelValue {
                metric: Some("http_requests_total".to_owned()),
                label: "service".to_owned()
            }
        );
        // Regex-match operator too.
        assert_eq!(
            completion_context("m{service=~\"ca").0,
            CompletionContext::LabelValue {
                metric: Some("m".to_owned()),
                label: "service".to_owned()
            }
        );
        // An unquoted value position (after `=`) offers nothing.
        assert_eq!(
            completion_context("m{service=").0,
            CompletionContext::Suppressed
        );
        // The returned token is the partial being written.
        assert_eq!(completion_context("m{service=\"ca").1, "ca");
        assert_eq!(completion_context("m{ser").1, "ser");
    }

    fn app_with_discovered_dims() -> App {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.mode = Mode::Editing;
        app.metric_names = vec!["http_requests_total".to_owned()];
        app.metric_tree = vec![MetricNode {
            name: "http_requests_total".to_owned(),
            kind: "sum".to_owned(),
            unit: String::new(),
            temporality: String::new(),
            expanded: false,
            whole_selected: false,
            dims: Some(vec![
                DimNode {
                    label: "service".to_owned(),
                    values: vec!["cart".to_owned(), "checkout".to_owned()],
                    expanded: false,
                    selected: None,
                },
                DimNode {
                    label: "host".to_owned(),
                    values: vec!["node-a".to_owned()],
                    expanded: false,
                    selected: None,
                },
            ]),
            loading: false,
        }];
        app
    }

    #[test]
    fn completion_offers_label_names_inside_a_matcher() {
        let mut app = app_with_discovered_dims();
        *app.active_query_mut() = "http_requests_total{s".to_owned();
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("label-name candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["service"]); // only the label starting with `s`
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::Label)
        );
    }

    #[test]
    fn completion_offers_label_values_inside_a_quoted_matcher() {
        let mut app = app_with_discovered_dims();
        *app.active_query_mut() = "http_requests_total{service=\"c".to_owned();
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("label-value candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        // Both values start with `c`, sorted, tagged as label values.
        assert_eq!(texts, vec!["cart", "checkout"]);
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::LabelValue)
        );

        // Accepting replaces just the partial value.
        app.accept_completion();
        assert_eq!(app.active_query(), "http_requests_total{service=\"cart");
    }

    #[test]
    fn completion_requests_dims_for_an_undiscovered_metric_once() {
        let mut app = app_with_discovered_dims();
        // Reset the metric to "not discovered yet".
        app.metric_tree[0].dims = None;
        *app.active_query_mut() = "http_requests_total{s".to_owned();
        // No label vocabulary yet -> no popup, but a discovery request is emitted exactly once.
        app.refresh_completion();
        assert!(app.completion.is_none());
        assert_eq!(
            app.completion_dim_request(),
            Some(("http_requests_total".to_owned(), "sum".to_owned()))
        );
        // Marked loading now, so it does not fire again.
        assert_eq!(app.completion_dim_request(), None);
    }

    /// A Logs app in edit mode with a discovered label vocabulary (label names + one label's values),
    /// mirroring `app_with_discovered_dims` but for the Logs screen's cross-signal attribute source.
    fn logs_app_with_labels() -> App {
        let mut app = App::new();
        app.route = Route::Logs;
        app.mode = Mode::Editing;
        app.log_labels = Some(vec![
            "service.name".to_owned(),
            "http.method".to_owned(),
            "host".to_owned(),
        ]);
        app.log_label_values.insert(
            "service.name".to_owned(),
            vec!["cart".to_owned(), "checkout".to_owned()],
        );
        app
    }

    #[test]
    fn logs_expression_position_offers_operator_hints_not_promql_functions() {
        // Empty token in expression position on the Logs screen: the LogQL line-filter operator hints
        // (and pipeline keywords), never the PromQL/range function list.
        let candidates = completion_candidates(
            Screen::Logs,
            &[],
            &[],
            &[],
            &HashMap::new(),
            &CompletionContext::Expr,
            "",
        );
        let ops = candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Operator)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ops, vec!["!=", "!?", "!~", "|=", "|?", "|~"]);
        // No PromQL/range function candidates on the Logs box (e.g. `rate`, `count_over_time`).
        assert!(
            candidates.iter().all(|c| c.kind != CandidateKind::Function),
            "Logs expression position must not offer PromQL functions"
        );
        assert!(
            !candidates.iter().any(|c| c.text == "rate"),
            "`rate` must not be offered on the Logs box"
        );
    }

    #[test]
    fn logs_expression_popup_opens_on_an_empty_token() {
        // Unlike Metrics/Traces (whose Expr vocabulary is large and waits for input), the Logs box's
        // short operator-hint list pops immediately after a selector.
        let mut app = logs_app_with_labels();
        *app.active_query_mut() = "{}".to_owned();
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("operator-hint popup");
        assert!(
            completion
                .candidates
                .iter()
                .any(|c| c.kind == CandidateKind::Operator && c.text == "|?")
        );
    }

    #[test]
    fn completion_offers_log_label_names_inside_a_matcher() {
        let mut app = logs_app_with_labels();
        *app.active_query_mut() = "{h".to_owned();
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("log label-name candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        // Only the labels starting with `h`, sorted, tagged as label names.
        assert_eq!(texts, vec!["host", "http.method"]);
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::Label)
        );
    }

    #[test]
    fn completion_offers_log_label_values_inside_a_quoted_matcher() {
        let mut app = logs_app_with_labels();
        *app.active_query_mut() = "{service.name=\"c".to_owned();
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("log label-value candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["cart", "checkout"]);
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::LabelValue)
        );
        // Accepting replaces just the partial value.
        app.accept_completion();
        assert_eq!(app.active_query(), "{service.name=\"cart");
    }

    #[test]
    fn completion_requests_log_labels_once_when_undiscovered() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.mode = Mode::Editing;
        // No label vocabulary discovered yet.
        *app.active_query_mut() = "{s".to_owned();
        app.refresh_completion();
        assert!(app.completion.is_none(), "no vocabulary -> no popup yet");
        assert_eq!(
            app.completion_log_request(),
            Some(LogCompletionRequest::Labels)
        );
        // Marked loading now, so it does not fire again.
        assert_eq!(app.completion_log_request(), None);

        // Once the names arrive, the same caret position fills the popup in.
        app.log_labels = Some(vec!["service.name".to_owned()]);
        app.log_labels_loading = false;
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("labels now available");
        assert_eq!(completion.candidates[0].text, "service.name");
    }

    #[test]
    fn completion_requests_log_label_values_once_per_key() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.mode = Mode::Editing;
        app.log_labels = Some(vec!["service.name".to_owned()]);
        // In a quoted value for an as-yet-undiscovered key.
        *app.active_query_mut() = "{service.name=\"c".to_owned();
        app.refresh_completion();
        assert!(app.completion.is_none(), "no values yet -> no popup");
        assert_eq!(
            app.completion_log_request(),
            Some(LogCompletionRequest::Values("service.name".to_owned()))
        );
        // Marked loading, so the same key does not fire again.
        assert_eq!(app.completion_log_request(), None);
    }

    #[test]
    fn pan_window_shifts_the_span_and_freezes_it_absolute() {
        let mut app = App::new();
        // Anchor an absolute window well in the past so the pan-later clamp to `now` never bites.
        let span = 100_000_000_000i64; // 100s
        app.abs_window = Some((0, span));
        // Pan earlier by half the span.
        assert!(app.pan_window(-0.5));
        assert_eq!(app.abs_window, Some((-span / 2, span / 2)));
        // Pan later by half the span (still far in the past → no clamp).
        assert!(app.pan_window(0.5));
        assert_eq!(app.abs_window, Some((0, span)));
    }

    #[test]
    fn pan_later_into_now_resumes_the_live_rolling_window() {
        let mut app = App::new();
        let span = 100_000_000_000i64; // 100s
        let now = Timestamp::now().0;
        // An absolute window whose leading edge is within half a span of `now`, so a `]` (0.5) reaches
        // it: end = now - span/4 (room = span/4 < delta = span/2).
        app.abs_window = Some((now - span - span / 4, now - span / 4));
        assert!(app.pan_window(0.5), "pan reaches now");
        assert_eq!(
            app.abs_window, None,
            "catching up to now returns to the live rolling window, not a frozen absolute one"
        );
        // Already live: another `]` is a no-op (can't pan past now).
        assert!(!app.pan_window(0.5));
        assert_eq!(app.abs_window, None);
    }

    #[test]
    fn zoom_window_scales_about_the_center_within_bounds() {
        let mut app = App::new();
        let span = 100_000_000_000i64; // 100s, center at 50s
        app.abs_window = Some((0, span));
        // Zoom out 2x: span doubles (200s) about the center (50s ± 100s) → (-50s, 150s).
        assert!(app.zoom_window(2.0));
        assert_eq!(app.abs_window, Some((-span / 2, span + span / 2)));
        // Zoom in past the floor clamps to MIN_WINDOW_NS (1s) about the current center.
        let (start, end) = app.effective_window();
        let center = start + (end - start) / 2;
        assert!(app.zoom_window(1e-9));
        let (zs, ze) = app.abs_window.unwrap();
        assert_eq!(ze - zs, MIN_WINDOW_NS);
        assert_eq!(zs + (ze - zs) / 2, center);
    }

    #[test]
    fn log_paging_guards_reject_when_not_applicable() {
        let mut app = App::new();
        app.route = Route::Logs;
        // Page 0 with no next cursor: older is a no-op, newer is a no-op.
        assert!(!app.logs_page_older(), "no next cursor -> no older page");
        assert!(!app.logs_page_newer(), "page 0 -> no newer page");
        // Off the Logs list entirely.
        app.route = Route::Traces;
        assert!(!app.logs_page_older());
        assert!(!app.logs_page_newer());
    }

    #[test]
    fn metric_ident_from_promql_finds_the_selector() {
        // The exact PromQL shapes `build_metric_query` emits from the catalog.
        assert_eq!(
            metric_ident_from_promql("up{a=\"1\"}").as_deref(),
            Some("up")
        );
        assert_eq!(
            metric_ident_from_promql("rate(http_requests_total{a=\"1\"}[5m])").as_deref(),
            Some("http_requests_total")
        );
        assert_eq!(
            metric_ident_from_promql("sum by (svc) (rate(errors[1m]))").as_deref(),
            Some("errors")
        );
        assert_eq!(
            metric_ident_from_promql("avg by (host) (cpu_usage)").as_deref(),
            Some("cpu_usage")
        );
        assert_eq!(
            metric_ident_from_promql(
                "histogram_quantile(0.95, sum by (le) (rate(latency_bucket[5m])))"
            )
            .as_deref(),
            Some("latency_bucket")
        );
        assert_eq!(metric_ident_from_promql("(((").as_deref(), None);
    }

    #[test]
    fn metric_name_prefers_the_name_label_then_the_query() {
        let with_label = MetricDetail {
            labels: "__name__=req_total,service=api".to_owned(),
            query: "rate(other[5m])".to_owned(),
            points: Vec::new(),
        };
        assert_eq!(
            metric_name_from_detail(&with_label).as_deref(),
            Some("req_total")
        );
        let without_label = MetricDetail {
            labels: "service=api".to_owned(),
            query: "rate(bar[5m])".to_owned(),
            points: Vec::new(),
        };
        assert_eq!(
            metric_name_from_detail(&without_label).as_deref(),
            Some("bar")
        );
    }

    #[test]
    fn metric_detail_chart_follows_a_refresh_so_pan_zoom_redraws() {
        let mut app = App::new();
        app.generation = 3;
        // A metric detail opened over an old window (two points).
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "__name__=m,svc=api".to_owned(),
                query: "m".to_owned(),
                points: vec![(0, 1.0), (10, 2.0)],
            },
        };
        app.metric_cursor = 1;
        // A refresh (as a pan/zoom would trigger) lands a Metrics result whose matching series carries
        // the new window's points.
        let mut snapshot = Snapshot::message("PromQL", "");
        snapshot.series = vec![
            SeriesData {
                labels: "__name__=m,svc=api".to_owned(),
                points: vec![(100, 5.0), (110, 6.0), (120, 7.0)],
            },
            SeriesData {
                labels: "__name__=other".to_owned(),
                points: vec![(100, 9.0)],
            },
        ];
        app.apply(QueryResult {
            generation: 3,
            screen: Screen::Metrics,
            result: Ok(snapshot),
        });
        let detail = app.route_metric_detail().expect("still a metric detail");
        assert_eq!(
            detail.points,
            vec![(100, 5.0), (110, 6.0), (120, 7.0)],
            "the detail chart adopts the matching series' new-window points"
        );
        // The cursor stays in range of the (now longer) series.
        assert!(app.metric_cursor < detail.points.len());

        // A refresh whose window no longer contains the series clears the plot (honest empty).
        let mut empty = Snapshot::message("PromQL", "");
        empty.series = vec![SeriesData {
            labels: "__name__=unrelated".to_owned(),
            points: vec![(0, 1.0)],
        }];
        app.apply(QueryResult {
            generation: 3,
            screen: Screen::Metrics,
            result: Ok(empty),
        });
        assert!(app.route_metric_detail().unwrap().points.is_empty());
    }

    #[test]
    fn metric_detail_with_no_points_renders_without_panicking() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // Panning/zooming to an empty window can leave the detail with zero points; drawing it must not
        // index the empty series (regression: `detail.points[cursor]` panicked and killed the program).
        let mut app = App::new();
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "__name__=m".to_owned(),
                query: "m".to_owned(),
                points: Vec::new(),
            },
        };
        app.metric_cursor = 5; // stale cursor from a previously non-empty window
        let options = Options::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, &options))
            .expect("draw must not panic on an empty metric detail");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("no samples in this window"), "{text:?}");
    }

    #[test]
    fn nearest_exemplar_trace_tracks_the_chart_cursor() {
        let mut app = App::new();
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "__name__=m".to_owned(),
                query: "m".to_owned(),
                points: vec![(0, 1.0), (10, 2.0), (20, 3.0)],
            },
        };
        app.metric_exemplars = vec![
            ExemplarMarker {
                time_ns: 1,
                trace_id: "aaaa".to_owned(),
            },
            ExemplarMarker {
                time_ns: 19,
                trace_id: "bbbb".to_owned(),
            },
        ];
        app.metric_cursor = 0; // t=0 -> nearest is the t=1 exemplar
        assert_eq!(app.nearest_exemplar_trace().as_deref(), Some("aaaa"));
        app.metric_cursor = 2; // t=20 -> nearest is the t=19 exemplar
        assert_eq!(app.nearest_exemplar_trace().as_deref(), Some("bbbb"));
        // No markers -> nothing to jump to.
        app.metric_exemplars.clear();
        assert_eq!(app.nearest_exemplar_trace(), None);
    }

    #[test]
    fn history_restores_a_trace_to_log_correlation() {
        let mut app = App::new();
        app.route = Route::Traces;
        // Drill into a trace's logs: capture Traces, then set the correlation on the Logs view.
        app.push_history();
        app.route = Route::Logs;
        app.log_correlation = Some(LogCorrelation {
            trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
            span_id: None,
        });
        // Back to Traces clears it; Forward to the correlated Logs restores it.
        assert!(app.go_back());
        assert_eq!(app.log_correlation, None);
        assert!(app.go_forward());
        assert_eq!(
            app.log_correlation.as_ref().map(|c| c.trace_id.as_str()),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn tiny_terminal_shows_a_resize_prompt_and_a_full_one_does_not() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let app = App::new();
        let options = Options::default();
        let text_of = |w: u16, h: u16| {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|frame| draw(frame, &app, &options)).unwrap();
            let buffer = terminal.backend().buffer();
            let mut out = String::new();
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    out.push_str(buffer[(x, y)].symbol());
                }
            }
            out
        };
        // Below the minimum: the resize prompt, and none of the normal chrome.
        let tiny = text_of(20, 6);
        assert!(tiny.contains("too small"), "tiny render: {tiny:?}");
        assert!(tiny.contains("40x10"));
        // A comfortable terminal renders the real UI, never the prompt.
        let full = text_of(80, 24);
        assert!(!full.contains("too small"), "full render leaked the prompt");
    }
}
