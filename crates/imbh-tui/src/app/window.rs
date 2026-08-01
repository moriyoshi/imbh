//! The query time window: the relative presets, the absolute-range form, and pan/zoom.

use std::time::Duration;

use imbh::Timestamp;

use crate::app::App;
use crate::model::{MAX_WINDOW_NS, MIN_WINDOW_NS, Mode, TIME_RANGES};
use crate::time::{format_datetime_ns, parse_datetime};
use crate::ui::glyphs::Glyphs;

impl App {
    pub(crate) fn lookback(&self) -> Duration {
        TIME_RANGES[self.range_index].1
    }

    pub(crate) fn step(&self) -> Duration {
        TIME_RANGES[self.range_index].2
    }

    pub(crate) fn range_label(&self) -> &'static str {
        TIME_RANGES[self.range_index].0
    }

    /// The current effective query window `(start_ns, end_ns)`: the committed absolute window if set,
    /// otherwise the rolling `now - lookback .. now` derived from the selected preset.
    pub(crate) fn effective_window(&self) -> (i64, i64) {
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
    pub(crate) fn range_summary(&self, g: &Glyphs) -> String {
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
    pub(crate) fn open_absolute_form(&mut self) {
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
    pub(crate) fn commit_absolute(&mut self) -> bool {
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
    pub(crate) fn pan_window(&mut self, fraction: f64) -> bool {
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
    pub(crate) fn zoom_window(&mut self, factor: f64) -> bool {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_selection_updates_lookback_and_step() {
        let mut app = App::new();
        app.range_index = 0; // 5m
        assert_eq!(app.lookback(), Duration::from_secs(300));
        assert_eq!(app.step(), Duration::from_secs(5));
        assert_eq!(app.range_label(), "5m");
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
}
