//! Things the page has to be told about, that it did not ask for.
//!
//! A download at 40%, a guest that shut itself down. Not the engine's bus:
//! this one crosses to the page, and the daemon forwards it to every window.
//!
//! Not a queue. A listener that panics is dropped, not allowed to break the
//! publisher, usually a worker mid-install.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

type Listener = Arc<dyn Fn(&str, &Value) + Send + Sync>;

/// Where the page's events are published.
#[derive(Clone, Default)]
pub struct Hub {
    listeners: Arc<Mutex<Vec<(u64, Listener)>>>,
    next: Arc<Mutex<u64>>,
}

impl Hub {
    /// Hear every event, by name and wire payload. Returns what to hand
    /// [`Hub::unsubscribe`].
    pub fn subscribe(&self, listener: impl Fn(&str, &Value) + Send + Sync + 'static) -> u64 {
        let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        *next += 1;
        let id = *next;
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, Arc::new(listener)));
        id
    }

    /// Stop hearing. Twice is not an error.
    pub fn unsubscribe(&self, id: u64) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(kept, _)| *kept != id);
    }

    /// Tell every listener. Called from whichever thread did the work.
    pub fn publish(&self, name: &str, payload: &Value) {
        let listeners: Vec<(u64, Listener)> = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for (id, listener) in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener(name, payload))).is_err() {
                log::error!("BeatBox event subscriber failed on {name}; dropping it");
                self.unsubscribe(id);
            }
        }
    }
}
