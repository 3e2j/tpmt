//! Bounds-checked big-endian byte cursors.
//!
//! Everything on the disc is big-endian, and every format crate parses the same
//! shape: read a header, follow an offset into a table, read records at
//! computed positions. A reader does that over a borrowed buffer; a writer
//! builds one up and backpatches the offsets that were not knowable at the
//! point they were reserved.
//!
//! No format knowledge lives here. This is a crate rather than a module in one
//! of the others only because all of them need it.
