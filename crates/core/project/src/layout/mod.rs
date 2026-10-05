//! One module per directory a project holds, each owning its name and the
//! files tpmt writes there.

pub mod modding;
pub mod store;
pub mod vanilla;

/// Every top-level name this crate writes. A directory holding nothing but
/// these and their [`Staging`](crate::io::Staging) copies is ours, however
/// far an unpack got before it failed.
pub const OWNED: [&str; 4] = [vanilla::DIR, modding::DIR, crate::build::DIR, store::DIR];
