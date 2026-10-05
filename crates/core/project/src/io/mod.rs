//! Disk access shared by the project and the pipeline that fills it:
//! [`fs`] wraps `std::fs` so every error names its path, and [`Staging`]
//! replaces a whole directory or leaves it as it was.

pub mod fs;
mod staging;

pub use staging::{Staging, refuse_unowned};
