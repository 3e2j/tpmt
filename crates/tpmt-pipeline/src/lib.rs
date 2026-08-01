//! Unpacking a disc to a mod folder and building it back.
//!
//! Walks the disc, peels off compression, opens archives, hands decodable files
//! to whichever format crate claims them, and writes the result out as a folder
//! a person can actually edit.
//!
//! Building runs the same route backwards. A clean copy of the original disc
//! stays alongside the mod folder, so working out what changed is a comparison
//! rather than a record: anything matching its counterpart in the clean copy is
//! reused verbatim, and only genuinely edited files are re-encoded. Nothing is
//! re-encoded that nobody touched, which is what keeps the formats we cannot
//! reproduce byte for byte from drifting.
//!
//! The only crate allowed to know how formats stack. That a `.arc` on the disc
//! is usually Yaz0 wrapped around RARC is a fact about this pipeline, not about
//! either format.
