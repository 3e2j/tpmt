//! Sidecars hold what a decoded format can't represent as editable files, so
//! the build can restore it without needing the original disc.
//!
//! One module and one `.tpmt-<format>.toml` file per format, dotted so it
//! stays out of the way of what's meant to be edited.

pub(crate) mod arc;
