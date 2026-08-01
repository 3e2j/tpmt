//! Reading and writing GameCube disc images.
//!
//! Raw disc dumps, distributed as `.iso`, `.gcm`, or other variants.
//!
//! A disc is a fixed-layout preamble (boot header, disc metadata, apploader,
//! executable) followed by a file string table describing everything else as a
//! directory tree.
//!
//! Also identifies a disc. The game id and revision come out of the boot
//! header, and a SHA-1 over the whole image says whether this is the same dump
//! a project was unpacked from.
//!
//! Hands out byte ranges. Decoding the file contents is the job of the format
//! crates.
