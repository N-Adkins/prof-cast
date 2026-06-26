//! Core data types and shared APIs for profcast

pub mod error;
pub mod format;
pub mod model;

pub use error::{ProfcastError, Result};

/// The package name for this crate
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// The package version for this crate
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A user-facing profcast version string
pub const VERSION_STRING: &str = concat!("profcast ", env!("CARGO_PKG_VERSION"));
