//! Events: a fact, a bus, and a recorder.
//!
//! Apart from `runtime` because nothing here knows what a sandbox is, so a
//! listener never becomes a dependency of the thing that emits.

pub(crate) mod bus;
mod event;

pub use bus::{EventBus, Handler, Recorder, Subscription};
pub use event::{Data, Event};
