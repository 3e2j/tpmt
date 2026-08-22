//! The write path: turns a [`Bmg`] back into bytes.

use crate::{Bmg, Result};

/// Writes a whole message file from what [`unpack`](crate::unpack) took apart.
pub fn pack(_bmg: &Bmg) -> Result<Vec<u8>> {
    todo!("the writer")
}
