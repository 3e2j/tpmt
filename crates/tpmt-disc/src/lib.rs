//! Reading and writing GameCube disc images.
//!
//! Raw disc dumps, distributed as `.iso`, `.gcm`, or other variants.
//!
//! A disc is a fixed-layout preamble (boot header, disc metadata, apploader,
//! executable) followed by a file string table describing everything else as a
//! directory tree.
//!
//! Hands out byte ranges. Decoding the file contents is the job of the format
//! crates.
