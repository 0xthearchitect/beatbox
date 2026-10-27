//! Where events go, so a sandbox never has to know who is listening.
//!
//! Delivery is synchronous and in subscription order. A handler that panics
//! is logged and skipped: the publisher already did what it is reporting.
//! A handler that publishes is delivered depth-first, with a later sequence.
//!
//! Lanes publish from several threads, but subscribers must see events in
//! `sequence` order, so stamping and delivering happen inside one turn of a
//! reentrant lock. That lets a handler publish inside its parent's turn
//! while another thread waits. The subscriber list has its own short lock,
//! so handlers can subscribe and unsubscribe too.
//!
//! **A handler must never wait for a sandbox's lane**: it runs inside a turn
//! another lane may be waiting for. Read `Sandbox::state`, which never waits.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

use crate::clock::{Clock, system_clock};
use crate::events::event::{Data, Event};
use crate::faults::ContractError;
use crate::names::{validate, validate_pattern};

/// Something that wants to hear about events.
pub type Handler = Arc<dyn Fn(&Event) + Send + Sync>;

/// A handle, because the same function can be subscribed twice to the same
/// pattern and unsubscribing has to remove just one of them.
#[derive(Clone)]
pub struct Subscription {
    id: u64,
    pattern: String,
    handler: Handler,
}

impl Subscription {
    /// The pattern it listens on.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Subscription")
            .field("id", &self.id)
            .field("pattern", &self.pattern)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Listing {
    subscriptions: Vec<Subscription>,
    next_id: u64,
    sequence: u64,
}

/// A lock the thread holding it can take again. Ours, because std's
/// `ReentrantLock` isn't stable yet.
#[derive(Default)]
struct Turn {
    holder: Mutex<(Option<ThreadId>, usize)>,
    free: Condvar,
}

struct TurnGuard<'a>(&'a Turn);

impl Turn {
    fn take(&self) -> TurnGuard<'_> {
        let me = thread::current().id();
        let mut held = lock(&self.holder);
        loop {
            match held.0 {
                None => {
                    *held = (Some(me), 1);
                    break;
                }
                Some(owner) if owner == me => {
                    held.1 += 1;
                    break;
                }
                Some(_) => held = self.free.wait(held).unwrap_or_else(PoisonError::into_inner),
            }
        }
        TurnGuard(self)
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        let mut held = lock(&self.0.holder);
        held.1 -= 1;
        if held.1 == 0 {
            held.0 = None;
            self.0.free.notify_one();
        }
    }
}

/// A lock whose holder panicking does not make it unusable: every lock in
/// this engine guards book-keeping that is valid between any two statements.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Publish, subscribe, and one counter that decides what came first.
pub struct EventBus {
    clock: Clock,
    turn: Turn,
    listing: Mutex<Listing>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(system_clock())
    }
}

impl EventBus {
    /// A bus whose events are stamped by `clock`.
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            turn: Turn::default(),
            listing: Mutex::new(Listing {
                next_id: 1,
                ..Listing::default()
            }),
        }
    }

    /// Stamp an event and hand it to everyone who asked for it.
    ///
    /// Returns the event, so a caller can keep what it said without
    /// listening to itself.
    pub fn publish(
        &self,
        name: &str,
        sandbox_id: &str,
        data: Data,
    ) -> Result<Event, ContractError> {
        validate(name, "an event name")?;
        let _turn = self.turn.take();
        let (event, subscriptions) = {
            let mut listing = lock(&self.listing);
            listing.sequence += 1;
            let event = Event {
                name: name.to_owned(),
                sandbox_id: sandbox_id.to_owned(),
                at: (self.clock)(),
                sequence: listing.sequence,
                data,
            };
            (event, listing.subscriptions.clone())
        };
        for subscription in subscriptions {
            if !event.matches(&subscription.pattern) {
                continue;
            }
            let handler = &subscription.handler;
            if catch_unwind(AssertUnwindSafe(|| handler(&event))).is_err() {
                log::error!("BeatBox event subscriber failed on {}", event.name);
            }
        }
        Ok(event)
    }

    /// `"*"`, `"process.*"` or an exact name. See [`crate::names`].
    pub fn subscribe(
        &self,
        pattern: &str,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> Result<Subscription, ContractError> {
        validate_pattern(pattern, "a subscription pattern")?;
        let mut listing = lock(&self.listing);
        let subscription = Subscription {
            id: listing.next_id,
            pattern: pattern.to_owned(),
            handler: Arc::new(handler),
        };
        listing.next_id += 1;
        listing.subscriptions.push(subscription.clone());
        Ok(subscription)
    }

    /// `true` if it was there. Unsubscribing twice is not an error.
    pub fn unsubscribe(&self, subscription: &Subscription) -> bool {
        let mut listing = lock(&self.listing);
        let before = listing.subscriptions.len();
        listing
            .subscriptions
            .retain(|kept| kept.id != subscription.id);
        listing.subscriptions.len() != before
    }

    /// Every subscription, in the order they were made.
    pub fn subscriptions(&self) -> Vec<Subscription> {
        lock(&self.listing).subscriptions.clone()
    }

    /// How many events this bus has stamped. The sequence of the last one.
    pub fn published(&self) -> u64 {
        lock(&self.listing).sequence
    }
}

/// Everything that was said, kept for whoever asks afterwards. Install it
/// with `bus.subscribe("*", recorder.sink())`; tests assert on it.
///
/// Bounded, oldest out first, since nobody prunes it. Clones share one record.
#[derive(Clone)]
pub struct Recorder {
    limit: usize,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl Recorder {
    /// A recorder that keeps the newest `limit` events.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            events: Arc::default(),
        }
    }

    /// What to subscribe.
    pub fn sink(&self) -> impl Fn(&Event) + Send + Sync + 'static {
        let recorder = self.clone();
        move |event| recorder.record(event)
    }

    /// Keep one event.
    pub fn record(&self, event: &Event) {
        let mut events = lock(&self.events);
        events.push(event.clone());
        if events.len() > self.limit {
            let excess = events.len() - self.limit;
            events.drain(..excess);
        }
    }

    /// How many are kept.
    pub fn len(&self) -> usize {
        lock(&self.events).len()
    }

    /// Whether nothing has been said.
    pub fn is_empty(&self) -> bool {
        lock(&self.events).is_empty()
    }

    /// Everything kept, oldest first.
    pub fn events(&self) -> Vec<Event> {
        lock(&self.events).clone()
    }

    /// The story, in order, as a list of names -- what a test asserts on.
    pub fn names(&self) -> Vec<String> {
        lock(&self.events).iter().map(|e| e.name.clone()).collect()
    }

    /// The events `pattern` covers, oldest first.
    pub fn matching(&self, pattern: &str) -> Vec<Event> {
        lock(&self.events)
            .iter()
            .filter(|event| event.matches(pattern))
            .cloned()
            .collect()
    }

    /// Forget everything.
    pub fn clear(&self) {
        lock(&self.events).clear();
    }
}
