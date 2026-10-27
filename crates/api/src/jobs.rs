//! Work that takes longer than a person will wait for a reply.
//!
//! An install, a download: the action hands back a job at once and the work
//! reports itself as `job` events, so the window never freezes.
//!
//! One at a time on purpose: a second is refused, not queued.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use beatbox_contract::Job;
use beatbox_reasons::Reason;

use crate::events::Hub;
use crate::sandbox::reason_view;

/// Still going.
pub const RUNNING: &str = "running";
/// Finished, and it worked.
pub const DONE: &str = "done";
/// Finished, and it did not.
pub const FAILED: &str = "failed";

/// What the page is told, every time a job moves.
pub const EVENT: &str = "job";

/// What a job last said: a sentence, or a reason so a no reads in the
/// reader's language.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    /// Progress, or where the result is.
    Text(String),
    /// Why it failed, named.
    Reason(Reason),
}

impl From<&str> for Said {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for Said {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<Reason> for Said {
    fn from(reason: Reason) -> Self {
        Self::Reason(reason)
    }
}

/// What a job's work is handed, to narrate with.
pub type Say<'a> = &'a dyn Fn(&str);

#[derive(Default)]
struct Table {
    jobs: HashMap<String, Job>,
    active: Option<String>,
}

/// The jobs, and the one that is in flight.
#[derive(Clone)]
pub struct Jobs {
    table: Arc<Mutex<Table>>,
    hub: Hub,
}

/// A job is already running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy(pub String);

fn fresh_id() -> String {
    beatbox_isolation::uuid4()
        .replace('-', "")
        .chars()
        .take(12)
        .collect()
}

impl Jobs {
    /// Jobs that report on `hub`.
    pub fn new(hub: Hub) -> Self {
        Self {
            table: Arc::default(),
            hub,
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The job in flight, or the last one, if there has been one.
    pub fn current(&self) -> Option<Job> {
        let table = self.table();
        table
            .active
            .as_ref()
            .and_then(|id| table.jobs.get(id).cloned())
    }

    /// One job, by id.
    pub fn get(&self, job_id: &str) -> Option<Job> {
        self.table().jobs.get(job_id).cloned()
    }

    /// Start `work` on a thread of its own and report it as a job of `kind`.
    ///
    /// `work` narrates through `say` and returns whether it worked and its
    /// last words. A panic ends the job as failed instead of running forever.
    pub fn run(
        &self,
        kind: &str,
        work: impl FnOnce(Say<'_>) -> (bool, Said) + Send + 'static,
    ) -> Result<Job, Busy> {
        let job = {
            let mut table = self.table();
            if let Some(active) = table.active.as_ref().and_then(|id| table.jobs.get(id))
                && active.state == RUNNING
            {
                return Err(Busy(format!("{} is already running", active.kind)));
            }
            let job = Job {
                id: fresh_id(),
                kind: kind.to_owned(),
                state: RUNNING.to_owned(),
                message: String::new(),
                reason: None,
            };
            table.jobs.insert(job.id.clone(), job.clone());
            table.active = Some(job.id.clone());
            job
        };

        let jobs = self.clone();
        let job_id = job.id.clone();
        let update = move |state: &str, said: Said| {
            let (message, reason) = match said {
                Said::Text(text) => (text, None),
                Said::Reason(why) => (why.to_string(), Some(reason_view(&why))),
            };
            let fresh = {
                let mut table = jobs.table();
                let entry = table.jobs.get_mut(&job_id).expect("a job is never removed");
                entry.state = state.to_owned();
                entry.message = message;
                entry.reason = reason;
                entry.clone()
            };
            jobs.hub.publish(EVENT, &wire(&fresh));
        };

        self.hub.publish(EVENT, &wire(&job));
        let name = format!("beatbox-{kind}");
        let kind = kind.to_owned();
        thread::Builder::new()
            .name(name)
            .spawn(move || {
                let announce = |message: &str| update(RUNNING, Said::Text(message.to_owned()));
                let (ok, said) = match catch_unwind(AssertUnwindSafe(|| work(&announce))) {
                    Ok(result) => result,
                    Err(payload) => {
                        let detail = payload
                            .downcast_ref::<&str>()
                            .map(|text| (*text).to_owned())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "a job panicked".into());
                        log::error!("BeatBox job {kind} crashed: {detail}");
                        (false, Said::Text(format!("panic: {detail}")))
                    }
                };
                update(if ok { DONE } else { FAILED }, said);
            })
            .expect("a thread can be started");
        Ok(job)
    }
}

pub(crate) fn wire(job: &Job) -> serde_json::Value {
    serde_json::to_value(job).expect("a job is JSON")
}
