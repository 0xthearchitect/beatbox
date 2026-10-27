//! The providers the engine itself carries: one, a test double.
//!
//! The host sandbox lives in `beatbox-host`, because the engine may not see
//! QEMU at all.

mod mock;

pub use mock::{CAPABILITIES as MOCK_CAPABILITIES, KILLED, MockSandbox};
