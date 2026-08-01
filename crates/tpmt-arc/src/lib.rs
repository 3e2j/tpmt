//! RARC archive containers.
//!
//! Distributed as `.arc`, these are containers of game assets: a header, a
//! tree of directory nodes, a flat table of file entries, and a string pool for
//! the names, followed by the file data itself.
//!
//! Nearly every archive on the disc arrives Yaz0-compressed, but that wrapper
//! comes off before anything here sees it. Container only. What an entry holds
//! is somebody else's problem.
