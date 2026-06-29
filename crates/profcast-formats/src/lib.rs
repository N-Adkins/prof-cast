//! Crate to store all of the different format "codecs" - specifies
//! their formats and probing of them, parsing, etc

pub mod folded;
pub mod json;
pub mod pprof;
pub mod registry;
pub mod speedscope;

pub use registry::{Match, Registry};
