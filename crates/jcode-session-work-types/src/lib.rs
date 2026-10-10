//! Public session-work types: item aliases, the workflow model, session
//! activation records and the contract versions clients negotiate.
//!
//! The authority for every fact lives in `jcode-base::session_work`. These
//! types describe that state to the runtime, its clients and later adapters;
//! holding one never confers authority.
use serde::{Deserialize, Serialize};

mod activation;
mod correlation;
mod items;
mod workflow;

pub use activation::*;
pub use correlation::*;
pub use items::*;
pub use workflow::*;

/// The `session_work_v1` contract version this crate describes.
pub const SESSION_WORK_VERSION: u32 = 1;
