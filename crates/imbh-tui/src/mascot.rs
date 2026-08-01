//! The animated mascot ("Atta") and its motion foundation.
//!
//! The mascot is event-driven and its motions are pluggable. The run loop turns state changes into
//! [`MascotEvent`]s (navigation, idle/active, data refresh) and feeds them, plus a per-frame tick, to
//! a [`Mascot`] controller. Two behaviours ride on this foundation today:
//!   * [`IdleWander`] — the default: intermittent left/right strolling, but ONLY while the user is
//!     idle.
//!   * [`ChartRide`] — an easter egg: on a random, temporary basis, when a metric chart shows a large
//!     drop, the mascot jumps onto the high point and slides down the datapoints, then hands back.
//!
//! A new motion plugs in by implementing [`MascotMotion`] (+ a [`MascotIgniter`] that decides when to
//! start it) and pushing the igniter into [`Mascot::new`]; nothing else changes. The art itself is
//! blitted by [`crate::ui::draw_mascot`].

use std::time::{Duration, Instant};

use imbh::Timestamp;
use rand::rngs::SmallRng;
use rand::{Rng as _, SeedableRng};
use ratatui::layout::Rect;

use crate::chart::ChartGeometry;

/// The animated mascot, "Atta": block-glyph art (a little bobbing creature), transcribed verbatim from
/// `mascot.txt`. The four frames are **two facings, two waddle phases each**: indices `0..2` face and
/// step **right** (`mascot.txt` frames `1-a`/`1-b`), indices `2..4` face and step **left** (`2-a`/`2-b`).
/// Within a facing the two phases alternate to waddle; the head/feet whitespace differs so it bobs.
/// [`mascot_art`] selects the pair for a facing, and [`draw_mascot`](crate::ui::draw_mascot) blits
/// the chosen frame at the [`Mascot`]'s current position.
pub(crate) const MASCOT_FRAMES: [[&str; 3]; 4] = [
    ["▄████▄", "███▄█▄██", "▐▛▛▛▛▛▛▌"],
    ["▄████▄", "███▄█▄██", "▐▜▜▜▜▜▜▎"],
    ["  ▄████▄", "██▄█▄███", "▐▜▜▜▜▜▜▛"],
    ["  ▄████▄", "██▄█▄███", " ▛▛▛▛▛▛▌"],
];

/// Mascot waddle cadence: advance one waddle phase every this many nanoseconds. Paired with the faster
/// `Normal`-mode wake tick in [`run`](crate::runtime::run) so the animation is redrawn smoothly.
pub(crate) const MASCOT_FRAME_NS: i64 = 1_000_000_000;

/// Rows of art per frame (all frames are 3 rows tall).
pub(crate) const MASCOT_ART_HEIGHT: u16 = 3;

/// Nominal art width (the wider frames are 8 cells); used for horizontal centring/bounds so motions do
/// not have to know the exact per-frame width. [`draw_mascot`](crate::ui::draw_mascot) uses the
/// real width when it blits.
pub(crate) const MASCOT_ART_WIDTH: u16 = 8;

/// Rows kept clear at the bottom for the status/hint bar (the idle mascot rests just above it).
pub(crate) const MASCOT_BOTTOM_MARGIN: u16 = 2;

/// How long without a keystroke before the user is considered idle (the wander then starts).
pub(crate) const MASCOT_IDLE_AFTER: Duration = Duration::from_secs(3);

/// The art frame for a facing (`+1` = right, `-1`/`0` = left) and a waddle `phase` (`0`/`1`). Right uses
/// `MASCOT_FRAMES[0..2]`, left uses `[2..4]` — see the [`MASCOT_FRAMES`] doc.
pub(crate) fn mascot_art(facing: i8, phase: usize) -> &'static [&'static str; 3] {
    let base = if facing < 0 { 2 } else { 0 };
    &MASCOT_FRAMES[base + (phase & 1)]
}

/// The waddle phase (`0`/`1`) for an accumulated `phase_ns`, flipping every [`MASCOT_FRAME_NS`].
/// `rem_euclid` keeps it in range for negative accumulators.
pub(crate) fn mascot_phase(phase_ns: i64) -> usize {
    (phase_ns.rem_euclid(MASCOT_FRAME_NS * 2) / MASCOT_FRAME_NS) as usize
}

/// Discrete signals the mascot's motions react to. Emitted by [`run`](crate::runtime::run) from
/// frame-to-frame state deltas.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MascotEvent {
    /// The current [`Route`](crate::model::Route) changed; `on_metric_chart` is whether it is now
    /// the metric diagram.
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
pub(crate) enum Activity {
    Active,
    Idle,
}

/// The mascot's position (screen-cell space, floats for smooth sub-cell motion), the direction it faces
/// (which art pair to show), and an accumulator that drives the waddle phase.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MascotBody {
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// `+1` faces/steps right, `-1` left.
    pub(crate) facing: i8,
    pub(crate) phase_ns: i64,
}

/// Read-only context handed to motions/igniters each tick: the area to stay within and the metric
/// chart's geometry when one is on screen.
pub(crate) struct MascotCtx {
    pub(crate) area: Rect,
    pub(crate) chart: Option<ChartGeometry>,
}

/// A transient, pluggable mascot motion. [`tick`](MascotMotion::tick) advances the body by `dt_ns`;
/// returning `false` means the motion is finished and control returns to the idle behaviour.
pub(crate) trait MascotMotion {
    fn tick(&mut self, dt_ns: i64, body: &mut MascotBody, ctx: &MascotCtx) -> bool;
}

/// Decides when to start a motion — the pluggable ignition seam. [`arm`](MascotIgniter::arm) records
/// interest from an event; [`poll`](MascotIgniter::poll) runs every tick and may fire (returning the
/// motion to run). Splitting the two lets a motion that depends on render geometry (like the chart
/// ride) arm on an event yet wait until the geometry it needs actually appears.
pub(crate) trait MascotIgniter {
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
pub(crate) struct IdleWander {
    /// Where it is currently heading (`None` between strolls, while dwelling).
    pub(crate) target_x: Option<f32>,
    /// Remaining dwell before the next stroll.
    pub(crate) dwell_ns: i64,
}

impl IdleWander {
    /// Cells per second while strolling.
    pub(crate) const SPEED: f32 = 12.0;

    pub(crate) fn new() -> Self {
        Self {
            target_x: None,
            dwell_ns: 0,
        }
    }

    /// Advance the stroll. Called by the controller only while [`Activity::Idle`].
    pub(crate) fn tick(
        &mut self,
        dt_ns: i64,
        body: &mut MascotBody,
        ctx: &MascotCtx,
        rng: &mut SmallRng,
    ) {
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
pub(crate) fn densify(cells: &[(u16, u16)]) -> Vec<(u16, u16)> {
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
pub(crate) struct ChartRide {
    /// Dense body-space positions from the peak down to the trough, one cell apart, so the slide is a
    /// smooth, slow, clearly-visible glide.
    pub(crate) steps: Vec<(f32, f32)>,
    /// Set on the first tick, capturing where the hop launches from.
    pub(crate) launch: Option<(f32, f32)>,
    /// Elapsed time in the hop phase.
    pub(crate) hop_t_ns: i64,
    /// Whether the hop is done and the slide is underway.
    pub(crate) sliding: bool,
    /// Fractional index along `steps` while sliding.
    pub(crate) pos: f32,
    /// Current slide speed in cells/sec, integrated from the local gradient (gravity + friction).
    pub(crate) speed: f32,
    /// The overall horizontal direction of the descent (`+1` right, `-1` left); the mascot holds this
    /// facing for the whole slide so it stays oriented downhill instead of flickering on vertical steps.
    pub(crate) slide_facing: i8,
}

impl ChartRide {
    /// Hop duration onto the peak (kept short so the downhill slide is the star of the show).
    pub(crate) const HOP_NS: i64 = 320_000_000;
    /// Peak height of the hop arc, in rows.
    pub(crate) const ARC_ROWS: f32 = 2.0;
    /// Along-slope gravity (cells/sec²): steeper pitches accelerate the slide harder.
    pub(crate) const SLIDE_GRAVITY: f32 = 54.0;
    /// Drag (1/sec): with gravity this gives a terminal speed of `GRAVITY·sin(θ)/FRICTION`, so the
    /// steady speed tracks the local steepness — the whole point of being faithful to the hill.
    pub(crate) const SLIDE_FRICTION: f32 = 3.0;
    /// Speed bounds (cells/sec): never fully stall, never blur past the datapoints.
    pub(crate) const SLIDE_MIN_SPEED: f32 = 2.0;
    pub(crate) const SLIDE_MAX_SPEED: f32 = 22.0;
    /// A gentle push-off so the glide starts moving rather than creeping from rest.
    pub(crate) const SLIDE_START_SPEED: f32 = 5.0;
    /// Half-window (in dense cells) for the smoothed local gradient, so speed follows the terrain's
    /// disposition rather than the ±1-cell quantisation of the densified path.
    pub(crate) const SLOPE_WINDOW: usize = 3;

    pub(crate) fn new(path: Vec<(u16, u16)>) -> Self {
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
    pub(crate) fn local_steepness(&self, i: usize) -> f32 {
        let lo = i.saturating_sub(Self::SLOPE_WINDOW);
        let hi = (i + Self::SLOPE_WINDOW).min(self.steps.len() - 1);
        let (a, b) = (self.steps[lo], self.steps[hi]);
        let dx = (b.0 - a.0).abs();
        let dy = (b.1 - a.1).max(0.0); // downhill only (rows increase downward)
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        (dy / len).clamp(0.0, 1.0)
    }

    /// Body position that places the mascot's base on datapoint cell `c` (art centred over the column).
    pub(crate) fn body_at(c: (u16, u16)) -> (f32, f32) {
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
pub(crate) fn great_displacement_path(chart: &ChartGeometry) -> Option<Vec<(u16, u16)>> {
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
pub(crate) struct ChartRideIgniter {
    pub(crate) armed_ns: i64,
    /// Whether the single dice roll for the current arming has been spent.
    pub(crate) rolled: bool,
    /// Probability the current arming actually fires (set by the arming event).
    pub(crate) chance: f64,
}

impl ChartRideIgniter {
    /// How long an arming stays live waiting for the chart geometry to appear.
    pub(crate) const ARMED_NS: i64 = 2_000_000_000;
    /// Arriving at a chart usually rides — occasionally not, so it stays a wink rather than a metronome.
    pub(crate) const NAV_CHANCE: f64 = 0.8;
    /// A background auto-refresh only occasionally re-rides the same view.
    pub(crate) const REFRESH_CHANCE: f64 = 0.25;

    pub(crate) fn new() -> Self {
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
pub(crate) struct Mascot {
    pub(crate) body: MascotBody,
    pub(crate) idle: IdleWander,
    pub(crate) active: Option<Box<dyn MascotMotion>>,
    pub(crate) igniters: Vec<Box<dyn MascotIgniter>>,
    pub(crate) activity: Activity,
    pub(crate) rng: SmallRng,
    pub(crate) last_tick: Instant,
    /// Whether the body has been positioned against a real area yet (deferred to the first update, when
    /// the terminal size is known).
    pub(crate) placed: bool,
}

impl Mascot {
    /// Clamp a per-frame delta so a long pause (mascot hidden, a modal open) never teleports it.
    pub(crate) const MAX_DT_NS: i64 = 500_000_000;

    pub(crate) fn new() -> Self {
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
    pub(crate) fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    /// Advance the mascot: apply `events`, then move by the elapsed time within `ctx`.
    pub(crate) fn update(&mut self, events: &[MascotEvent], ctx: &MascotCtx) {
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
    pub(crate) fn place(&mut self, area: Rect) {
        self.body.x = area.right().saturating_sub(MASCOT_ART_WIDTH + 1) as f32;
        self.body.y = area
            .bottom()
            .saturating_sub(MASCOT_ART_HEIGHT + MASCOT_BOTTOM_MARGIN) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
