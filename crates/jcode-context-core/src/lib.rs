//! Pure, deterministic provider-context projection and validation.
//!
//! This crate performs no storage, provider, logging, orchestration, or UI work.
//! The authoritative transcript is always supplied by the caller and is never mutated.

pub mod closure;
pub mod economics;
pub mod estimate;
pub mod lifecycle;
pub mod pressure;
pub mod projection;
pub mod target;
pub mod validation;

#[cfg(test)]
mod provider_validation_tests;

pub use closure::*;
pub use economics::*;
pub use estimate::*;
pub use lifecycle::*;
pub use pressure::*;
pub use projection::*;
pub use target::*;
pub use validation::*;

const STABLE_HASH_SEED: u64 = 0xcbf29ce484222325;
const STABLE_HASH_PRIME: u64 = 0x100000001b3;

pub(crate) fn stable_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = STABLE_HASH_SEED;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(STABLE_HASH_PRIME);
    }
    hash
}

pub(crate) fn extend_stable_hash(accumulator: u64, next: u64) -> u64 {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&accumulator.to_le_bytes());
    bytes[8..].copy_from_slice(&next.to_le_bytes());
    stable_hash_bytes(&bytes)
}
