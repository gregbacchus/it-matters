//! The Matter frontend: serves the bridged endpoints the host creates, using rs-matter.

pub mod commissioning;
mod handlers;
mod node;
mod run;
mod shared;
pub mod state;

pub use run::{Settings, run_blocking};
