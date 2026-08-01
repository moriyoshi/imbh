//! [`run`] — the event loop: draw, wait for input/updates/ticks, apply, repeat.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use imbh::Db;
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use crate::app::App;
use crate::keys::{Control, handle_key};
use crate::mascot::{MASCOT_IDLE_AFTER, MascotCtx, MascotEvent};
use crate::model::{Mode, Options, Route, Update};
use crate::tasks::{request_refresh, request_waterfall};
use crate::terminal::{TerminalGuard, input_reader, install_panic_hook};
use crate::ui::draw;

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
                        Update::Waterfall { generation, trace_id, detail, trace } => {
                            // Apply only if still current: same query generation and the selection has
                            // not moved to a different trace since the fetch was issued.
                            if generation == app.generation
                                && app.detail_trace_id.as_deref() == Some(trace_id.as_str())
                            {
                                app.snapshot.detail = Some(detail);
                                app.trace_detail = trace;
                                // An Enter pressed while this fetch was in flight opens the full trace
                                // detail now that the data is here.
                                if app.pending_trace_open {
                                    app.pending_trace_open = false;
                                    app.open_trace_detail();
                                }
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
