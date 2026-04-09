//! Time-unit conversions used throughout the codebase.
//!
//! Centralizes the literal `1_000_000_000`, `1_000_000`, etc. that historically
//! appeared inline at hundreds of call sites and were locally redeclared in
//! several modules with inconsistent types. Use these constants in any new
//! code that does ns/ms/us/s conversion math.

/// Nanoseconds per second, as `u64`. Use for u64-only arithmetic.
pub const NS_PER_SECOND: u64 = 1_000_000_000;

/// Nanoseconds per second, as `f64`. Use for floating-point conversions
/// (`ns as f64 / NS_PER_SECOND_F`).
pub const NS_PER_SECOND_F: f64 = 1_000_000_000.0;
