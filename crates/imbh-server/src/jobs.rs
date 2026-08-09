//! Queued housekeeping, and the record a client polls for it.
//!
//! Housekeeping — seal, commit pending rewrites, apply retention, and optionally compact — is the one
//! `/admin` action whose duration follows the *corpus* rather than the request. On a database with a
//! long retention window a compaction pass can run for minutes, which is longer than a proxy will
//! hold a connection open and far longer than a caller should have to wait to learn its request was
//! accepted. So it is submitted rather than performed: `POST` answers `202` with a job id, and the
//! client asks about that id afterwards.
//!
//! **One at a time.** Every job takes the same permit, so a second submission waits for the first
//! rather than running beside it. Two concurrent passes over one database would contend for the same
//! disk and, worse, could each seal and compact segments the other had just planned around — the
//! serialization is what makes "housekeeping is running" a state the database can be in, rather than
//! a race to describe.
//!
//! **Duplicate submissions coalesce.** A submission that matches a job still *queued* answers with
//! that job's id instead of adding a second one. A caller on a timer, or two operators reaching for
//! the same button, would otherwise queue passes that each do nothing the one before them did not —
//! the passes are serialized, so the pile-up is pure wait. Coalescing stops at the queue: a *running*
//! job is not a match, because it snapshotted the database before this submission arrived and may
//! already be past the segments the caller wants covered. A queued one has not looked yet, so it will
//! see everything the new request wants.
//!
//! **Bounded history.** Finished records are kept so a client that polls late still gets an answer,
//! but only [`HISTORY`] of them: a daemon that runs housekeeping on a timer for a year must not
//! accumulate a year of records. Eviction is oldest-finished-first, so the ones a client is most
//! likely still asking about survive longest.
//!
//! **Ids do not outlive the process.** A job id carries the registry's creation time, so an id issued
//! by a previous `imbhd` is *not found* rather than colliding with a fresh counter and describing
//! somebody else's job. The queue is in memory by design: an interrupted pass has no partial state to
//! resume — seal, commit and retention are each individually crash-safe (ARCHITECTURE.md §7), so the
//! honest answer after a restart is "submit it again", not a resurrected record.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use imbh::{Db, Timestamp};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::offload;

/// Finished job records kept for polling. Sized for "a client that stepped away", not for an audit
/// log — a deployment that wants history should read the daemon's own telemetry.
const HISTORY: usize = 32;

/// Where a job is in its life. A client polls until it is one of the two terminal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Submitted, waiting for the permit an earlier job holds.
    Queued,
    /// Holding the permit and running.
    Running,
    Succeeded,
    Failed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
        }
    }

    /// Whether the job will not change again — what a polling client waits for.
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Succeeded | JobState::Failed)
    }
}

/// One submitted housekeeping pass.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub state: JobState,
    /// Whether this pass compacts as well as maintaining — the expensive half, and the reason the
    /// endpoint is asynchronous at all.
    pub compact: bool,
    /// Partitions the compaction half may rewrite, or `None` for all of them. The analogue of
    /// `imbh-housekeeper --max-jobs`: a bound on the *work* of one pass, so a caller can make
    /// incremental progress on a corpus too large to compact in one go.
    pub max_jobs: Option<usize>,
    pub submitted_unix_nano: i64,
    pub started_unix_nano: Option<i64>,
    pub finished_unix_nano: Option<i64>,
    /// What the pass did, on success. Both halves are reported, so a caller can tell a pass that
    /// found nothing to do from one that never ran.
    pub report: Option<Value>,
    pub error: Option<String>,
}

impl Job {
    /// The record as a client reads it. Timestamps are epoch nanoseconds, like every other timestamp
    /// this server emits.
    pub fn to_json(&self) -> Value {
        json!({
            "job_id": self.id,
            "state": self.state.as_str(),
            "compact": self.compact,
            "max_jobs": self.max_jobs,
            "submitted_unix_nano": self.submitted_unix_nano,
            "started_unix_nano": self.started_unix_nano,
            "finished_unix_nano": self.finished_unix_nano,
            "report": self.report,
            "error": self.error,
        })
    }
}

/// The submitted jobs of one running server.
#[derive(Debug)]
pub struct Jobs {
    entries: Mutex<HashMap<String, Job>>,
    /// Submission order, so eviction can drop the oldest finished record rather than an arbitrary one.
    order: Mutex<Vec<String>>,
    /// One permit: housekeeping passes run one at a time (see the module docs).
    gate: Arc<Semaphore>,
    next: AtomicU64,
    /// This registry's creation time, mixed into every id so ids from a previous process do not look
    /// like ids from this one.
    nonce: i64,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            entries: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            gate: Arc::new(Semaphore::new(1)),
            next: AtomicU64::new(1),
            nonce: Timestamp::now().0,
        }
    }
}

/// What a submission did: queued a new pass, or found the identical one already waiting.
///
/// Two outcomes rather than one `Job`, because the caller answers differently — a created job is a
/// `202` and a coalesced one a `200` carrying the id that will do the work.
#[derive(Debug, Clone)]
pub enum Submission {
    Created(Job),
    Coalesced(Job),
}

impl Submission {
    pub fn job(&self) -> &Job {
        match self {
            Submission::Created(job) | Submission::Coalesced(job) => job,
        }
    }

    pub fn is_coalesced(&self) -> bool {
        matches!(self, Submission::Coalesced(_))
    }
}

impl Jobs {
    /// Queue a housekeeping pass, or hand back the identical one already queued.
    ///
    /// Returns immediately either way: a created pass runs on a spawned task that first waits for the
    /// permit. The caller therefore learns its request was *accepted*, which is the only thing it can
    /// learn quickly and the only thing it needs before polling.
    ///
    /// "Identical" is an exact parameter match, not a subsumption test. A queued `compact: true` pass
    /// would in fact cover a new `compact: false` request, but the rule that says so has to be
    /// explained every time someone reads a job id they did not expect; an exact match is the one a
    /// caller can predict, and it covers the case that motivates coalescing at all — the same request,
    /// repeatedly, from a timer.
    pub fn submit(
        self: &Arc<Self>,
        db: &Arc<Db>,
        compact: bool,
        max_jobs: Option<usize>,
    ) -> Submission {
        let job = Job {
            // Placeholder: `insert_unless_queued` assigns the id, so the counter is not spent on a
            // submission that turns out to coalesce.
            id: String::new(),
            state: JobState::Queued,
            compact,
            max_jobs,
            submitted_unix_nano: Timestamp::now().0,
            started_unix_nano: None,
            finished_unix_nano: None,
            report: None,
            error: None,
        };
        let job = match self.insert_unless_queued(job) {
            Submission::Coalesced(existing) => return Submission::Coalesced(existing),
            Submission::Created(job) => job,
        };
        let id = job.id.clone();

        let (jobs, db, gate) = (Arc::clone(self), Arc::clone(db), Arc::clone(&self.gate));
        tokio::spawn(async move {
            // The permit is what serializes passes. `acquire_owned` on a closed semaphore cannot
            // happen — nothing closes it — but a failure here must still leave the record terminal
            // rather than stuck in `queued` forever.
            let permit = gate.acquire_owned().await;
            if permit.is_err() {
                jobs.finish(&id, Err("the housekeeping queue is closed".to_owned()));
                return;
            }
            jobs.update(&id, |job| {
                job.state = JobState::Running;
                job.started_unix_nano = Some(Timestamp::now().0);
            });
            jobs.finish(&id, run(&db, compact, max_jobs).await);
        });
        Submission::Created(job)
    }

    /// One job by id, or `None` for an id this process never issued.
    pub fn get(&self, id: &str) -> Option<Job> {
        self.lock_entries().get(id).cloned()
    }

    /// Every retained job, newest submission first — what a client lists when it has lost an id.
    pub fn recent(&self) -> Vec<Job> {
        let entries = self.lock_entries();
        let order = self.order.lock().unwrap_or_else(PoisonError::into_inner);
        order
            .iter()
            .rev()
            .filter_map(|id| entries.get(id).cloned())
            .collect()
    }

    /// Assign an id and record `job`, unless a **queued** job already asks for the same work.
    ///
    /// The search and the insert happen under one lock acquisition: two submissions arriving together
    /// must not both find nothing and both queue a pass, which is exactly the pile-up this prevents.
    fn insert_unless_queued(&self, mut job: Job) -> Submission {
        let mut entries = self.lock_entries();
        let mut order = self.order.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = order.iter().rev().find_map(|id| {
            entries.get(id).filter(|candidate| {
                candidate.state == JobState::Queued
                    && candidate.compact == job.compact
                    && candidate.max_jobs == job.max_jobs
            })
        }) {
            return Submission::Coalesced(existing.clone());
        }
        job.id = format!(
            "{:x}-{}",
            self.nonce,
            self.next.fetch_add(1, Ordering::Relaxed)
        );
        let recorded = job.clone();
        order.push(job.id.clone());
        entries.insert(job.id.clone(), job);
        // Evict the oldest *finished* records only: a queued or running job is still going to be
        // asked about, and dropping it would leave a client polling an id that answers 404 while the
        // work it names is still happening.
        while entries.len() > HISTORY {
            let Some(position) = order
                .iter()
                .position(|id| entries.get(id).is_some_and(|job| job.state.is_terminal()))
            else {
                break;
            };
            let id = order.remove(position);
            entries.remove(&id);
        }
        Submission::Created(recorded)
    }

    fn update(&self, id: &str, edit: impl FnOnce(&mut Job)) {
        if let Some(job) = self.lock_entries().get_mut(id) {
            edit(job);
        }
    }

    fn finish(&self, id: &str, outcome: Result<Value, String>) {
        self.update(id, |job| {
            job.finished_unix_nano = Some(Timestamp::now().0);
            match outcome {
                Ok(report) => {
                    job.state = JobState::Succeeded;
                    job.report = Some(report);
                }
                Err(error) => {
                    job.state = JobState::Failed;
                    job.error = Some(error);
                }
            }
        });
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One housekeeping pass: maintain, then compact if asked.
///
/// Maintenance first, and not merely by convention: it commits pending rewrites and applies retention,
/// so compacting afterwards works on the segment set that survives rather than merging segments
/// retention is about to drop.
async fn run(db: &Arc<Db>, compact: bool, max_jobs: Option<usize>) -> Result<Value, String> {
    let maintenance = offload(db.maintain()).await.map_err(|e| e.to_string())?;
    let mut report = json!({
        "sealed": maintenance.sealed,
        "segments_dropped": maintenance.segments_dropped,
        "bytes_freed": maintenance.bytes_freed,
        "pending_applied": maintenance.pending_applied,
        "pending_discarded": maintenance.pending_discarded,
        "pending_segments_replaced": maintenance.pending_segments_replaced,
    });
    if compact {
        // `max_jobs` bounds the *partitions rewritten*, not the segments: a partition is the unit
        // compaction works in, and it is what a caller can meaningfully ask for less of.
        let compaction = offload(db.compact_bounded(max_jobs.unwrap_or(usize::MAX)))
            .await
            .map_err(|e| e.to_string())?;
        report["segments_merged"] = json!(compaction.segments_merged);
        report["segments_converged"] = json!(compaction.segments_converged);
        report["segments_created"] = json!(compaction.segments_created);
        // `segments_created` is the partitions this pass rewrote, so a caller draining a large corpus
        // stops when it reaches zero and knows to call again while it has not.
        report["compaction_complete"] = json!(match max_jobs {
            Some(cap) => (compaction.segments_created as usize) < cap,
            None => true,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record as `insert_unless_queued` takes one: it assigns the id, so `id` here is a placeholder
    /// except where a test needs to name the record afterwards.
    fn job(id: &str, state: JobState) -> Job {
        Job {
            id: id.to_owned(),
            state,
            compact: false,
            max_jobs: None,
            submitted_unix_nano: 0,
            started_unix_nano: None,
            finished_unix_nano: None,
            report: None,
            error: None,
        }
    }

    /// History is bounded, and eviction never takes a job that is still going to be asked about.
    #[test]
    fn eviction_drops_finished_records_and_keeps_live_ones() {
        let jobs = Jobs::default();
        // One job that never finishes, then enough finished ones to overflow the history twice over.
        // The states differ, so nothing here coalesces onto anything else.
        jobs.insert_unless_queued(job("live", JobState::Running));
        for _ in 0..HISTORY * 2 {
            jobs.insert_unless_queued(job("", JobState::Succeeded));
        }

        let recent = jobs.recent();
        assert!(
            recent.iter().any(|job| job.state == JobState::Running),
            "a running job is still going to be polled, so it outlives the finished ones"
        );
        assert!(
            recent.len() <= HISTORY + 1,
            "history is bounded: {} records",
            recent.len()
        );
        // Newest first, so the survivors are the most recently finished ones.
        assert_eq!(recent[0].state, JobState::Succeeded);
    }

    /// Ids are unique within a process and carry its nonce, so an id from a previous `imbhd` is not
    /// found rather than colliding with a fresh counter and describing somebody else's job.
    #[test]
    fn ids_are_unique_and_scoped_to_this_process() {
        let jobs = Jobs::default();
        let mine: Vec<String> = (0..8)
            .map(|_| {
                format!(
                    "{:x}-{}",
                    jobs.nonce,
                    jobs.next.fetch_add(1, Ordering::Relaxed)
                )
            })
            .collect();
        let mut sorted = mine.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), mine.len(), "unique: {mine:?}");

        let other = Jobs::default();
        assert!(
            mine.iter().all(|id| other.get(id).is_none()),
            "another registry never issued these"
        );
    }

    /// A submission matching a **queued** job hands back that job; anything else queues its own.
    ///
    /// Driven through `insert_unless_queued` rather than `submit` so the states under test are the
    /// ones the test puts there — `submit` would race its own spawned task into `Running`.
    #[test]
    fn a_duplicate_submission_joins_the_queued_job() {
        let jobs = Jobs::default();
        let queued = jobs.insert_unless_queued(job("", JobState::Queued));
        assert!(!queued.is_coalesced());
        let first = queued.job().id.clone();

        // The identical request finds it and does not add a second pass.
        let again = jobs.insert_unless_queued(job("", JobState::Queued));
        assert!(again.is_coalesced(), "the same work is already waiting");
        assert_eq!(again.job().id, first);
        assert_eq!(jobs.recent().len(), 1, "one pass, not two");

        // Different parameters are different work, so they queue separately.
        let mut other = job("", JobState::Queued);
        other.compact = true;
        let other = jobs.insert_unless_queued(other);
        assert!(!other.is_coalesced());
        assert_ne!(other.job().id, first);
        let mut bounded = job("", JobState::Queued);
        bounded.compact = true;
        bounded.max_jobs = Some(4);
        assert!(!jobs.insert_unless_queued(bounded).is_coalesced());
        assert_eq!(jobs.recent().len(), 3);
    }

    /// Coalescing stops at the queue. A *running* pass snapshotted the database before this
    /// submission arrived and may already be past what the caller wants covered; a queued one has not
    /// looked yet. Same for a finished pass, which is over.
    #[test]
    fn a_running_or_finished_job_is_not_joined() {
        for state in [JobState::Running, JobState::Succeeded, JobState::Failed] {
            let jobs = Jobs::default();
            let existing = jobs.insert_unless_queued(job("", state));
            let submitted = jobs.insert_unless_queued(job("", JobState::Queued));
            assert!(
                !submitted.is_coalesced(),
                "a {} pass covers no request made after it",
                state.as_str()
            );
            assert_ne!(submitted.job().id, existing.job().id);
        }
    }

    /// The two terminal states are the ones a poll stops on, and nothing else is.
    #[test]
    fn only_finished_states_are_terminal() {
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running.is_terminal());
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Failed.is_terminal());
        assert_eq!(JobState::Queued.as_str(), "queued");
    }
}
