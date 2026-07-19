//! The opt-in **async ingest** queue (ARCHITECTURE.md §5/§10.5). When a `Db` is opened with
//! [`imbh_core::Ingest::Async`], the protobuf decode still runs on the caller (so the `accepted`
//! count and a malformed-body error stay synchronous), but the WAL append + Arrow encode + buffer
//! push are handed to one background worker task through this bounded queue.
//!
//! The queue is a plain `Mutex<VecDeque<IngestJob>>` guarded by two [`tokio::sync::Notify`]s — one to
//! wake the single consumer (`item_ready`), one to wake [`Overflow::Block`] producers parked waiting
//! for a slot (`space_ready`). The `std` mutex is only ever held for the O(1) push/pop; it is never
//! held across an `.await`. Overflow behavior is chosen at open time by [`Overflow`].
//!
//! The worker loop itself lives in the facade (`run_ingest_worker` in `lib.rs`) because it needs the
//! private `Db::storage`; this module owns only the self-contained channel + job types.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Notify;

use imbh_core::{
    Error, ExpHistogramRow, HistogramRow, LogRow, Overflow, Result, ScalarMetricRow, SpanRow,
    SummaryRow,
};

/// One decoded OTLP request awaiting the WAL + buffer write. Carries the owned raw bytes (the WAL
/// frame needs them, and the caller's borrow cannot outlive async processing) plus the normalized
/// rows the decode produced. One variant per signal; metrics carry all four row kinds from a single
/// request (one WAL frame / LSN covers them, matching `Storage::ingest_metrics`).
pub(crate) enum IngestJob {
    Logs {
        raw: Vec<u8>,
        rows: Vec<LogRow>,
    },
    Traces {
        raw: Vec<u8>,
        rows: Vec<SpanRow>,
    },
    Metrics {
        raw: Vec<u8>,
        rows: Vec<ScalarMetricRow>,
        histograms: Vec<HistogramRow>,
        exp_histograms: Vec<ExpHistogramRow>,
        summaries: Vec<SummaryRow>,
    },
}

/// A bounded, single-consumer async ingest queue with a configurable overflow policy.
pub(crate) struct IngestChannel {
    q: Mutex<VecDeque<IngestJob>>,
    capacity: usize,
    overflow: Overflow,
    /// Wakes the worker when a job is enqueued (or the channel is closed).
    item_ready: Notify,
    /// Wakes [`Overflow::Block`] producers parked waiting for a slot to free.
    space_ready: Notify,
    /// Set by [`IngestChannel::close`]; tells the worker to drain and exit.
    closed: AtomicBool,
    /// Count of jobs evicted by [`Overflow::DropOldest`] (surfaced via `stats().ingest_dropped`).
    dropped: AtomicU64,
    /// Count of worker-side ingest failures (surfaced via `stats().ingest_errors`).
    errors: AtomicU64,
}

/// The result of a single enqueue attempt. `Full` hands the job back so a blocking producer can retry
/// it; `Closed` drops it (the caller gets a closed error — the channel will never process it).
enum OfferOutcome {
    /// The job was pushed to the queue and the worker was notified.
    Enqueued,
    /// The queue is at capacity; the job is returned for a blocking retry.
    Full(IngestJob),
    /// The channel is closed; the job was not enqueued and never will be.
    Closed,
}

impl OfferOutcome {
    /// Collapse a non-blocking outcome into the caller-facing `Result`: full → backpressure error,
    /// closed → closed error, enqueued → `Ok`.
    fn into_send_result(self, channel: &IngestChannel) -> Result<()> {
        match self {
            OfferOutcome::Enqueued => Ok(()),
            OfferOutcome::Full(_) => Err(channel.full_error()),
            OfferOutcome::Closed => Err(channel.closed_error()),
        }
    }
}

impl IngestChannel {
    /// Build a channel with `capacity` in-flight jobs (clamped to at least 1) and the given policy.
    pub(crate) fn new(capacity: usize, overflow: Overflow) -> Self {
        IngestChannel {
            q: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
            overflow,
            item_ready: Notify::new(),
            space_ready: Notify::new(),
            closed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    /// Try to enqueue `job`. The `closed` flag is read **under the queue lock** (and set the same way
    /// by [`Self::close`]), so once `close()` has committed no enqueue can slip past the worker's
    /// drain: an accepted job was necessarily enqueued strictly before `close()` and is therefore in
    /// the queue when the worker performs its post-`closed` final drain (see `run_ingest_worker`).
    fn offer(&self, job: IngestJob) -> OfferOutcome {
        let mut q = self.q.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            OfferOutcome::Closed
        } else if q.len() < self.capacity {
            q.push_back(job);
            drop(q);
            self.item_ready.notify_one();
            OfferOutcome::Enqueued
        } else {
            OfferOutcome::Full(job)
        }
    }

    /// Evict the oldest un-processed job (if at capacity), then enqueue `job`. Succeeds unless the
    /// channel is already closed (checked under the queue lock, same as [`Self::offer`]).
    fn push_dropping_oldest(&self, job: IngestJob) -> OfferOutcome {
        let mut q = self.q.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return OfferOutcome::Closed;
        }
        if q.len() >= self.capacity && q.pop_front().is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(job);
        drop(q);
        self.item_ready.notify_one();
        OfferOutcome::Enqueued
    }

    /// Enqueue from the **awaiting** path (`ingest_otlp_*`). Honors the full overflow policy:
    /// `Block` awaits a free slot, `Fail` returns a backpressure error, `DropOldest` evicts + pushes.
    /// Any policy returns [`Error::ingest_msg`] once the channel is closed rather than a false `Ok`
    /// for a job that would never be processed.
    pub(crate) async fn send(&self, job: IngestJob) -> Result<()> {
        match self.overflow {
            Overflow::DropOldest => self.push_dropping_oldest(job).into_send_result(self),
            Overflow::Fail => self.offer(job).into_send_result(self),
            Overflow::Block => {
                let mut job = job;
                loop {
                    // Arm the wakeup *before* offering, so a slot freed by the worker (or a `close()`
                    // waking parked producers) between our failed offer and our await is never lost —
                    // the permit is stored on the future.
                    let notified = self.space_ready.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    match self.offer(job) {
                        OfferOutcome::Enqueued => return Ok(()),
                        OfferOutcome::Closed => return Err(self.closed_error()),
                        OfferOutcome::Full(returned) => job = returned,
                    }
                    notified.await;
                }
            }
        }
    }

    /// Enqueue from the **non-blocking** path (`try_ingest_otlp_*`). Never awaits: `DropOldest` evicts
    /// then pushes, while `Block` and `Fail` both fail fast with a backpressure error when the queue is
    /// full (the non-awaiting path cannot honor `Block`, so it degrades to fail-fast). Returns a closed
    /// error once the channel is closed.
    pub(crate) fn try_send(&self, job: IngestJob) -> Result<()> {
        match self.overflow {
            Overflow::DropOldest => self.push_dropping_oldest(job).into_send_result(self),
            Overflow::Block | Overflow::Fail => self.offer(job).into_send_result(self),
        }
    }

    /// Pop the next job for the worker (FIFO), waking one parked [`Overflow::Block`] producer since a
    /// slot just freed. `None` when the queue is empty.
    pub(crate) fn pop(&self) -> Option<IngestJob> {
        let job = self.q.lock().unwrap().pop_front();
        if job.is_some() {
            self.space_ready.notify_one();
        }
        job
    }

    /// Park the worker until a job arrives or the channel is closed, without missing a wakeup that
    /// races the check: arm first, then re-test the queue/closed state before actually awaiting.
    pub(crate) async fn wait_for_item(&self) {
        let notified = self.item_ready.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_empty() || self.is_closed() {
            return;
        }
        notified.await;
    }

    fn is_empty(&self) -> bool {
        self.q.lock().unwrap().is_empty()
    }

    /// Current queue depth (in-flight jobs not yet processed).
    pub(crate) fn depth(&self) -> usize {
        self.q.lock().unwrap().len()
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// Record a worker-side ingest failure (the worker has no caller to return the error to).
    pub(crate) fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Signal the worker to drain the remaining jobs and exit. Idempotent. The `closed` store is done
    /// **under the queue lock** so it is totally ordered with respect to [`Self::offer`]'s under-lock
    /// `closed` check — an enqueue that observed `closed == false` was serialized before this store and
    /// its job is already in the queue for the worker's final drain. Both `Notify`s are woken: the
    /// worker (to drain and exit) and every parked [`Overflow::Block`] producer (so it re-offers, sees
    /// the closed flag, and returns a closed error instead of hanging — `close()` never frees a slot).
    pub(crate) fn close(&self) {
        {
            let _guard = self.q.lock().unwrap();
            self.closed.store(true, Ordering::Release);
        }
        self.item_ready.notify_waiters();
        self.space_ready.notify_waiters();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn full_error(&self) -> Error {
        Error::queue_full(self.capacity, self.capacity)
    }

    fn closed_error(&self) -> Error {
        Error::ingest_msg("async ingest queue is closed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logs_job(n: usize) -> IngestJob {
        IngestJob::Logs {
            raw: vec![n as u8],
            rows: Vec::new(),
        }
    }

    /// The tag byte we stashed in `raw`, so tests can assert FIFO / eviction order.
    fn tag(job: &IngestJob) -> u8 {
        match job {
            IngestJob::Logs { raw, .. } => raw[0],
            _ => unreachable!(),
        }
    }

    #[test]
    fn fail_policy_rejects_when_full() {
        let ch = IngestChannel::new(2, Overflow::Fail);
        assert!(ch.try_send(logs_job(1)).is_ok());
        assert!(ch.try_send(logs_job(2)).is_ok());
        let err = ch.try_send(logs_job(3)).unwrap_err();
        assert!(
            err.is_backpressure(),
            "full Fail queue → backpressure error"
        );
        assert_eq!(ch.depth(), 2);
    }

    #[test]
    fn block_policy_degrades_to_fail_fast_on_try_send() {
        // The non-awaiting path cannot block, so under Block it fails fast when full.
        let ch = IngestChannel::new(1, Overflow::Block);
        assert!(ch.try_send(logs_job(1)).is_ok());
        assert!(ch.try_send(logs_job(2)).unwrap_err().is_backpressure());
    }

    #[test]
    fn drop_oldest_evicts_and_counts() {
        let ch = IngestChannel::new(2, Overflow::DropOldest);
        ch.try_send(logs_job(1)).unwrap();
        ch.try_send(logs_job(2)).unwrap();
        ch.try_send(logs_job(3)).unwrap(); // evicts #1
        assert_eq!(ch.depth(), 2);
        assert_eq!(ch.dropped(), 1);
        assert_eq!(tag(&ch.pop().unwrap()), 2, "oldest surviving is #2");
        assert_eq!(tag(&ch.pop().unwrap()), 3);
        assert!(ch.pop().is_none());
    }

    #[test]
    fn pop_is_fifo() {
        let ch = IngestChannel::new(4, Overflow::Fail);
        for n in 1..=3 {
            ch.try_send(logs_job(n)).unwrap();
        }
        assert_eq!(tag(&ch.pop().unwrap()), 1);
        assert_eq!(tag(&ch.pop().unwrap()), 2);
        assert_eq!(tag(&ch.pop().unwrap()), 3);
    }

    #[test]
    fn try_send_after_close_is_rejected_not_silently_dropped() {
        for overflow in [Overflow::Block, Overflow::Fail, Overflow::DropOldest] {
            let ch = IngestChannel::new(2, overflow);
            ch.close();
            let err = ch
                .try_send(logs_job(1))
                .expect_err("a closed channel must reject, not accept-and-drop");
            assert!(
                !err.is_backpressure(),
                "closed is a distinct error, not backpressure"
            );
            assert_eq!(ch.depth(), 0, "the rejected job is never enqueued");
        }
    }

    #[tokio::test]
    async fn send_after_close_is_rejected_for_every_policy() {
        for overflow in [Overflow::Block, Overflow::Fail, Overflow::DropOldest] {
            let ch = IngestChannel::new(1, overflow);
            ch.close();
            // Block must not hang here: close() woke parked producers and the closed flag short-circuits
            // the retry loop instead of awaiting a slot that will never free.
            assert!(
                ch.send(logs_job(1)).await.is_err(),
                "closed send must return an error"
            );
            assert_eq!(ch.depth(), 0);
        }
    }

    #[test]
    fn jobs_enqueued_before_close_survive_for_the_final_drain() {
        // close() must not discard already-queued jobs; the worker drains them post-close.
        let ch = IngestChannel::new(4, Overflow::Block);
        ch.try_send(logs_job(1)).unwrap();
        ch.try_send(logs_job(2)).unwrap();
        ch.close();
        assert_eq!(tag(&ch.pop().unwrap()), 1, "queued jobs remain drainable");
        assert_eq!(tag(&ch.pop().unwrap()), 2);
        assert!(ch.pop().is_none());
    }

    #[tokio::test]
    async fn block_send_completes_after_a_slot_frees() {
        use std::sync::Arc;
        let ch = Arc::new(IngestChannel::new(1, Overflow::Block));
        ch.send(logs_job(1)).await.unwrap(); // fills the single slot

        // This send must park until a slot frees.
        let producer = {
            let ch = Arc::clone(&ch);
            tokio::spawn(async move { ch.send(logs_job(2)).await })
        };
        // Give the producer a moment to park, then free a slot by popping.
        tokio::task::yield_now().await;
        assert_eq!(tag(&ch.pop().unwrap()), 1);
        producer.await.unwrap().unwrap();
        assert_eq!(ch.depth(), 1, "the parked job landed once space freed");
        assert_eq!(tag(&ch.pop().unwrap()), 2);
    }
}
