//! The Traces list's waterfall preview: which trace it is showing, when that trace's fetch fires, and
//! the small cache that serves a revisit without one.
//!
//! A cursor move only *schedules* the fetch, [`WATERFALL_DEBOUNCE`] out, and every further move
//! pushes the deadline back; the event loop fires it once the cursor has been still that long
//! ([`App::take_due_waterfall`]). Holding ↓ through a list therefore issues one `traces().get()` for
//! the row it stops on rather than one per row passed. Each fetch carries a *ticket*, and only the
//! ticket in flight for the current selection is put on screen — anything older is a trace the cursor
//! has already left. It still goes into the [`WaterfallCache`], so walking back to it is free.
//!
//! The pane says when it is waiting ([`App::waterfall_wait`] drives its spinner), but nothing here
//! touches [`App::loading`]: one detail pane catching up is not a reason to hold the keyboard.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::App;
use crate::model::{DetailPane, DetailStyle, Route, SPINNER_FRAME, Screen};
use crate::waterfall::TraceDetail;

/// How long the Traces cursor must rest on a row before that row's waterfall is fetched.
///
/// Short enough that a deliberate stop reads as immediate, long enough that the gap between two
/// auto-repeated key presses (typically 30–50 ms) never lets a fetch slip out mid-scroll.
pub(crate) const WATERFALL_DEBOUNCE: Duration = Duration::from_millis(120);

/// How many fetched waterfalls the preview keeps for revisits. A trace detail holds every span, so
/// this is kept small; it only has to cover the handful of rows a user flicks between.
pub(crate) const WATERFALL_CACHE_CAP: usize = 16;

/// When a selection change should fire its fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaterfallFetch {
    /// After the cursor has rested for [`WATERFALL_DEBOUNCE`]: a cursor move, which is usually one of
    /// many in a row.
    Debounced,
    /// On the next turn of the event loop: a freshly landed result, whose selected row is where the
    /// user is about to look.
    Immediate,
}

/// A fetch the event loop should issue now: the trace, and the ticket its arrival must present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaterfallRequest {
    pub(crate) trace_id: String,
    pub(crate) ticket: u64,
}

/// A least-recently-used cache of fetched waterfalls, keyed by trace id.
///
/// A `VecDeque` searched linearly rather than a map plus a recency list: at
/// [`WATERFALL_CACHE_CAP`] entries a scan is cheaper than the bookkeeping, and it needs no dependency.
/// The front is the most recently used entry; the back is the next to go.
#[derive(Debug, Clone)]
pub(crate) struct WaterfallCache {
    entries: VecDeque<(String, DetailPane, TraceDetail)>,
    capacity: usize,
}

impl WaterfallCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// The cached waterfall for `trace_id`, marking it most recently used.
    pub(crate) fn get(&mut self, trace_id: &str) -> Option<(DetailPane, TraceDetail)> {
        let index = self.entries.iter().position(|(id, ..)| id == trace_id)?;
        let entry = self.entries.remove(index)?;
        let found = (entry.1.clone(), entry.2.clone());
        self.entries.push_front(entry);
        Some(found)
    }

    /// Remember a fetched waterfall, replacing any older copy and evicting the least recently used
    /// entry when full.
    pub(crate) fn insert(&mut self, trace_id: String, pane: DetailPane, trace: TraceDetail) {
        if self.capacity == 0 {
            return;
        }
        self.entries.retain(|(id, ..)| *id != trace_id);
        self.entries.push_front((trace_id, pane, trace));
        self.entries.truncate(self.capacity);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, trace_id: &str) -> bool {
        self.entries.iter().any(|(id, ..)| id == trace_id)
    }
}

/// The preview pane's fetch scheduling. `detail_trace_id` / `trace_detail` stay on [`App`] itself,
/// since the detail views read them too; this is only the part that decides when to ask.
#[derive(Debug, Clone)]
pub(crate) struct WaterfallPreview {
    /// When the scheduled fetch for `detail_trace_id` is due. `None` when nothing is scheduled.
    pub(crate) due: Option<Instant>,
    /// The ticket of the fetch in flight for `detail_trace_id`, if one is.
    pub(crate) in_flight: Option<u64>,
    /// The last ticket handed out.
    pub(crate) last_ticket: u64,
    /// Tickets below this were issued against a result set that has since been replaced, so their
    /// arrivals are not cached (the cache was cleared with that result set).
    pub(crate) cache_floor: u64,
    /// When the pane started waiting — the first move of a scroll, not the last — so the spinner
    /// turns steadily instead of restarting on every key press.
    pub(crate) waiting_since: Option<Instant>,
    pub(crate) cache: WaterfallCache,
}

impl WaterfallPreview {
    pub(crate) fn new() -> Self {
        Self {
            due: None,
            in_flight: None,
            last_ticket: 0,
            cache_floor: 0,
            waiting_since: None,
            cache: WaterfallCache::new(WATERFALL_CACHE_CAP),
        }
    }

    /// Stop waiting: nothing scheduled, nothing in flight that may land.
    fn idle(&mut self) {
        self.due = None;
        self.in_flight = None;
        self.waiting_since = None;
    }
}

/// The placeholder the pane shows while it waits for `trace_id`. The spinner is drawn over its text
/// from [`App::waterfall_wait`]; the text is what a frame drawn without one (a test, a snapshot)
/// still says.
fn waiting_pane(trace_id: &str) -> DetailPane {
    DetailPane {
        title: format!("Waterfall: {trace_id}"),
        lines: vec!["Loading waterfall...".to_owned()],
        waterfall: None,
        table: None,
        style: DetailStyle::Preview,
    }
}

impl App {
    /// The trace the preview pane should be showing: a pending log→trace focus, else the row cursor.
    fn preview_target(&self) -> Option<String> {
        self.focus_trace_id
            .clone()
            .or_else(|| self.selected_trace_id())
    }

    /// Follow the selection with the preview pane. A no-op when the target is already shown, being
    /// waited on, or there is none.
    ///
    /// A new target is served from the cache when it can be, which settles it on the spot. Otherwise
    /// the pane switches to its waiting placeholder — the previous trace's bars under a different
    /// selected row would be a wrong answer, not a stale one — and the fetch is scheduled per `when`.
    pub(crate) fn select_waterfall(&mut self, now: Instant, when: WaterfallFetch) {
        // A screen switch's placeholder has no list yet; a log→trace focus waits for the result, which
        // re-selects at once on landing. Scheduling now would only spend a fetch on the way there.
        if self.snapshot.is_loading_placeholder() {
            return;
        }
        let Some(trace_id) = self.preview_target() else {
            return;
        };
        if self.detail_trace_id.as_deref() == Some(trace_id.as_str()) {
            return;
        }
        self.detail_trace_id = Some(trace_id.clone());
        // The retained trace belongs to the *previous* selection; drop it so Enter cannot open a
        // detail for a trace the cursor has already left. This is also where a pending Enter intent is
        // abandoned (it was meant for the trace just left).
        self.trace_detail = None;
        self.pending_trace_open = false;
        // Whatever was in flight was for another trace: it may still be cached on arrival, but it is
        // no longer what this pane is waiting for.
        self.preview.in_flight = None;
        if let Some((pane, trace)) = self.preview.cache.get(&trace_id) {
            self.preview.idle();
            self.snapshot.detail = Some(pane);
            self.trace_detail = Some(trace);
            return;
        }
        self.preview.due = Some(match when {
            WaterfallFetch::Debounced => now + WATERFALL_DEBOUNCE,
            WaterfallFetch::Immediate => now,
        });
        self.preview.waiting_since.get_or_insert(now);
        self.snapshot.detail = Some(waiting_pane(&trace_id));
    }

    /// Bring a scheduled fetch forward to now. An explicit Enter on the list wants the trace it is
    /// pointing at, and should not sit out the rest of a debounce meant for a scroll.
    pub(crate) fn expedite_waterfall(&mut self, now: Instant) {
        if let Some(due) = self.preview.due.as_mut() {
            *due = (*due).min(now);
        }
    }

    /// The fetch to issue now, if its debounce has run out. Hands out a fresh ticket and records it
    /// as the one in flight. A deadline that comes due off the Traces screen is dropped instead: the
    /// pane it was for is no longer on screen, and returning re-selects anyway.
    pub(crate) fn take_due_waterfall(&mut self, now: Instant) -> Option<WaterfallRequest> {
        if self.preview.due? > now {
            return None;
        }
        self.preview.due = None;
        let trace_id = match self.detail_trace_id.clone() {
            Some(trace_id) if self.screen() == Screen::Traces => trace_id,
            _ => {
                self.preview.idle();
                return None;
            }
        };
        self.preview.last_ticket = self.preview.last_ticket.wrapping_add(1);
        let ticket = self.preview.last_ticket;
        self.preview.in_flight = Some(ticket);
        Some(WaterfallRequest { trace_id, ticket })
    }

    /// Land a fetched waterfall. Cached whenever it is a real trace from the current result set; shown
    /// only when it is the fetch the pane is waiting for. Returns whether it was shown.
    ///
    /// Showing it may complete an Enter pressed while it was in flight — the Traces-list Enter
    /// (`pending_trace_open`) or a log/exemplar→trace jump — so this also opens that detail.
    pub(crate) fn land_waterfall(
        &mut self,
        ticket: u64,
        trace_id: String,
        detail: DetailPane,
        trace: Option<TraceDetail>,
    ) -> bool {
        // Only a materialized trace is cached: a miss or an error may well be different next time.
        if ticket >= self.preview.cache_floor
            && let Some(trace) = &trace
        {
            self.preview
                .cache
                .insert(trace_id.clone(), detail.clone(), trace.clone());
        }
        if self.preview.in_flight != Some(ticket)
            || self.detail_trace_id.as_deref() != Some(trace_id.as_str())
            || self.screen() != Screen::Traces
        {
            return false;
        }
        self.preview.idle();
        self.snapshot.detail = Some(detail);
        self.trace_detail = trace;
        if self.pending_trace_open {
            self.pending_trace_open = false;
            self.open_trace_detail();
        } else {
            self.open_focused_trace_detail();
        }
        true
    }

    /// Forget which trace the pane shows and stop waiting for any, keeping the cache. For a change
    /// that replaces the snapshot the pane belongs to (a screen switch); the next selection on a
    /// Traces list starts afresh.
    pub(crate) fn drop_waterfall_fetch(&mut self) {
        self.detail_trace_id = None;
        self.preview.idle();
    }

    /// A fresh result set has landed: what the pane showed, what it was waiting for and what it had
    /// cached all describe the previous one. The cache goes too — a trace still receiving spans would
    /// otherwise be served as it looked before the refresh the user just asked for.
    pub(crate) fn reset_waterfalls(&mut self) {
        self.drop_waterfall_fetch();
        self.preview.cache.clear();
        self.preview.cache_floor = self.preview.last_ticket.wrapping_add(1);
    }

    /// How long the Traces preview pane has been waiting for its waterfall — through the debounce and
    /// the fetch — or `None` when it is not waiting (or is not on screen). Drives the pane's spinner.
    pub(crate) fn waterfall_wait(&self) -> Option<Duration> {
        if !matches!(self.route, Route::Traces) {
            return None;
        }
        if self.preview.due.is_none() && self.preview.in_flight.is_none() {
            return None;
        }
        self.preview.waiting_since.map(|since| since.elapsed())
    }

    /// The longest the event loop may sleep and still serve the preview: until a scheduled fetch is
    /// due, and no longer than a spinner frame while the pane is waiting. `None` when it has no say.
    pub(crate) fn waterfall_wake(&self, now: Instant) -> Option<Duration> {
        let due = self
            .preview
            .due
            .map(|due| due.saturating_duration_since(now));
        let spin = self.waterfall_wait().map(|_| SPINNER_FRAME);
        match (due, spin) {
            (Some(due), Some(spin)) => Some(due.min(spin)),
            (due, spin) => due.or(spin),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Snapshot;
    use crate::testutil::sample_trace;
    use crate::waterfall::build_trace_detail;

    /// A Traces list of `ids`, cursor on the first row, nothing fetched yet.
    fn traces_list(ids: &[&str]) -> App {
        let mut app = App::new();
        app.route = Route::Traces;
        let mut lines = vec![format!("{} matching traces", ids.len())];
        lines.extend(ids.iter().map(|id| format!("{id}  ts")));
        app.snapshot = Snapshot {
            lines,
            trace_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            list_from: Some(1),
            ..Default::default()
        };
        app.selected = 1;
        app
    }

    fn fetched(trace_id: &str) -> (DetailPane, Option<TraceDetail>) {
        let mut trace = build_trace_detail(&sample_trace(), true);
        trace.trace_id = trace_id.to_owned();
        let pane = DetailPane {
            title: format!("Waterfall: {trace_id}  (enter: detail)"),
            lines: Vec::new(),
            waterfall: Some(trace.waterfall.clone()),
            table: None,
            style: DetailStyle::Preview,
        };
        (pane, Some(trace))
    }

    #[test]
    fn a_scroll_collapses_into_one_fetch_for_the_row_it_stops_on() {
        let mut app = traces_list(&["a", "b", "c", "d", "e"]);
        let start = Instant::now();
        // Four auto-repeated presses 30 ms apart: each re-schedules, none fires.
        for step in 0..4u32 {
            let now = start + Duration::from_millis(30) * step;
            app.select_waterfall(now, WaterfallFetch::Debounced);
            assert_eq!(app.take_due_waterfall(now), None, "step {step}: mid-scroll");
            app.selected += 1;
        }
        let last_move = start + Duration::from_millis(120);
        app.select_waterfall(last_move, WaterfallFetch::Debounced);
        assert_eq!(app.detail_trace_id.as_deref(), Some("e"));
        // Still inside the debounce after the last move: nothing yet.
        assert_eq!(
            app.take_due_waterfall(last_move + WATERFALL_DEBOUNCE - Duration::from_millis(1)),
            None
        );
        // Once the cursor has rested, exactly one fetch — for the row it stopped on.
        let request = app
            .take_due_waterfall(last_move + WATERFALL_DEBOUNCE)
            .expect("the rested cursor's fetch");
        assert_eq!(request.trace_id, "e");
        assert_eq!(
            app.take_due_waterfall(last_move + WATERFALL_DEBOUNCE * 10),
            None,
            "and only one"
        );
        assert_eq!(app.preview.in_flight, Some(request.ticket));
    }

    #[test]
    fn a_fresh_result_fetches_its_selected_row_without_waiting() {
        let mut app = traces_list(&["a", "b"]);
        let now = Instant::now();
        app.select_waterfall(now, WaterfallFetch::Immediate);
        assert_eq!(
            app.take_due_waterfall(now).map(|request| request.trace_id),
            Some("a".to_owned())
        );
    }

    #[test]
    fn an_enter_mid_debounce_fires_the_fetch_at_once() {
        let mut app = traces_list(&["a"]);
        let now = Instant::now();
        app.select_waterfall(now, WaterfallFetch::Debounced);
        assert_eq!(app.take_due_waterfall(now), None);
        app.expedite_waterfall(now);
        assert!(app.take_due_waterfall(now).is_some());
    }

    #[test]
    fn a_revisited_trace_is_served_from_the_cache_without_a_fetch() {
        let mut app = traces_list(&["a", "b"]);
        let now = Instant::now();
        app.select_waterfall(now, WaterfallFetch::Immediate);
        let request = app.take_due_waterfall(now).expect("first visit fetches");
        let (pane, trace) = fetched("a");
        assert!(app.land_waterfall(request.ticket, "a".to_owned(), pane, trace));
        assert!(app.preview.cache.contains("a"));

        // Away to `b` and back to `a`.
        app.selected = 2;
        app.select_waterfall(now, WaterfallFetch::Debounced);
        assert_eq!(app.detail_trace_id.as_deref(), Some("b"));
        app.selected = 1;
        app.select_waterfall(now, WaterfallFetch::Debounced);
        // `a` is on screen at once, retained for Enter, and nothing is scheduled.
        assert_eq!(app.preview.due, None);
        assert_eq!(
            app.take_due_waterfall(now + WATERFALL_DEBOUNCE * 10),
            None,
            "a cache hit issues no fetch"
        );
        assert_eq!(
            app.trace_detail
                .as_ref()
                .map(|trace| trace.trace_id.as_str()),
            Some("a")
        );
        assert!(app.snapshot.detail.as_ref().unwrap().waterfall.is_some());
        assert_eq!(app.waterfall_wait(), None, "not waiting on anything");
    }

    #[test]
    fn an_arrival_for_a_trace_the_cursor_has_left_is_cached_but_not_shown() {
        let mut app = traces_list(&["a", "b"]);
        let now = Instant::now();
        app.select_waterfall(now, WaterfallFetch::Immediate);
        let stale = app.take_due_waterfall(now).expect("a's fetch");
        // The cursor moves on to `b` before `a` lands.
        app.selected = 2;
        app.select_waterfall(now, WaterfallFetch::Debounced);
        let (pane, trace) = fetched("a");
        assert!(!app.land_waterfall(stale.ticket, "a".to_owned(), pane, trace));
        assert!(
            app.trace_detail.is_none(),
            "b is not open-able with a's data"
        );
        assert_eq!(
            app.snapshot.detail.as_ref().map(|pane| pane.title.as_str()),
            Some("Waterfall: b"),
            "the pane still waits for b"
        );
        assert!(
            app.preview.cache.contains("a"),
            "but a revisit of a is free"
        );
    }

    #[test]
    fn a_superseded_fetch_for_the_same_trace_is_dropped_by_its_ticket() {
        // A refresh lands while `a`'s fetch is in flight and re-selects `a`: the older fetch describes
        // the previous result set, so only the new ticket may land — and the old one is not cached.
        let mut app = traces_list(&["a"]);
        let now = Instant::now();
        app.select_waterfall(now, WaterfallFetch::Immediate);
        let old = app.take_due_waterfall(now).expect("first fetch");
        app.reset_waterfalls();
        app.select_waterfall(now, WaterfallFetch::Immediate);
        let new = app.take_due_waterfall(now).expect("re-fetch");
        assert_ne!(old.ticket, new.ticket);
        let (pane, trace) = fetched("a");
        assert!(!app.land_waterfall(old.ticket, "a".to_owned(), pane.clone(), trace.clone()));
        assert!(
            !app.preview.cache.contains("a"),
            "pre-refresh data is not cached"
        );
        assert!(app.land_waterfall(new.ticket, "a".to_owned(), pane, trace));
    }

    #[test]
    fn the_pane_reports_its_wait_through_the_debounce_and_the_fetch() {
        let mut app = traces_list(&["a"]);
        let now = Instant::now();
        assert_eq!(app.waterfall_wait(), None);
        app.select_waterfall(now, WaterfallFetch::Debounced);
        assert!(app.waterfall_wait().is_some(), "waiting out the debounce");
        assert_eq!(
            app.waterfall_wake(now),
            Some(SPINNER_FRAME.min(WATERFALL_DEBOUNCE)),
            "the loop wakes for the deadline and the spinner"
        );
        let request = app
            .take_due_waterfall(now + WATERFALL_DEBOUNCE)
            .expect("due");
        assert!(app.waterfall_wait().is_some(), "waiting on the fetch");
        assert_eq!(app.waterfall_wake(now), Some(SPINNER_FRAME));
        let (pane, trace) = fetched("a");
        app.land_waterfall(request.ticket, "a".to_owned(), pane, trace);
        assert_eq!(app.waterfall_wait(), None);
        assert_eq!(app.waterfall_wake(now), None, "no reason to wake");
        // Nothing about it ever touched the query lock.
        assert!(!app.loading && !app.input_locked);
    }

    #[test]
    fn a_focused_jump_waits_for_its_result_rather_than_fetching_on_the_way() {
        // A log→trace jump focuses the trace and switches to Traces, whose snapshot is the loading
        // placeholder until the query lands. Nothing is scheduled against the placeholder.
        let mut app = App::new();
        app.route = Route::Traces;
        app.snapshot = Snapshot::loading("Traces");
        app.focus_trace_id = Some("a".to_owned());
        app.select_waterfall(Instant::now(), WaterfallFetch::Debounced);
        assert_eq!(app.detail_trace_id, None);
        assert_eq!(app.preview.due, None);
        assert!(app.snapshot.detail.is_none());
    }

    #[test]
    fn a_deadline_that_comes_due_off_the_traces_screen_is_dropped() {
        let mut app = traces_list(&["a"]);
        let now = Instant::now();
        app.select_waterfall(now, WaterfallFetch::Debounced);
        app.route = Route::Overview;
        assert_eq!(app.take_due_waterfall(now + WATERFALL_DEBOUNCE), None);
        assert_eq!(app.preview.due, None);
    }

    #[test]
    fn the_cache_evicts_the_least_recently_used_entry() {
        let mut cache = WaterfallCache::new(2);
        let (pane, trace) = fetched("a");
        let trace = trace.unwrap();
        cache.insert("a".to_owned(), pane.clone(), trace.clone());
        cache.insert("b".to_owned(), pane.clone(), trace.clone());
        // Touch `a`, so `b` is now the oldest.
        assert!(cache.get("a").is_some());
        cache.insert("c".to_owned(), pane.clone(), trace.clone());
        assert_eq!(cache.len(), 2);
        assert!(cache.contains("a") && cache.contains("c"));
        assert!(!cache.contains("b"));
        // Re-inserting an id replaces it rather than holding two copies.
        cache.insert("a".to_owned(), pane, trace);
        assert_eq!(cache.len(), 2);
    }
}
