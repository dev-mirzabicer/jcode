//! Public session-work types: item aliases, the workflow model, arrivals,
//! session activation records and the contract versions clients negotiate.
//!
//! The authority for every fact lives in `jcode-base::session_work`. These
//! types describe that state to the runtime, its clients and later adapters;
//! holding one never confers authority.
use serde::{Deserialize, Serialize};

mod activation;
mod arrivals;
mod correlation;
mod items;
mod stop;
mod workflow;

pub use activation::*;
pub use arrivals::*;
pub use correlation::*;
pub use items::*;
pub use stop::*;
pub use workflow::*;

/// The `session_work_v1` contract version this crate describes.
pub const SESSION_WORK_VERSION: u32 = 1;
