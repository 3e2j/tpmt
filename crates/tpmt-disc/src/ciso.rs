//! CISO containers: a disc image with the blocks that held nothing left out.
//!
//! A header of magic, block size and a map, then the blocks the map says are
//! there, back to back in map order. Whatever the map leaves out was zeros.
//!
//! The `NKIT  v2` footer some of these carry is ignored. It is undocumented and
//! its source is not public, but all it records is how to put the mastering fill
//! back, which we drop anyway.

use tpmt_bytes::Reader;

use crate::{Error, Result};

pub(crate) const MAGIC: &[u8; 4] = b"CISO";
pub(crate) const HEADER_LEN: u64 = 0x8000;
pub(crate) const BLOCK_SIZE_FIELD: usize = 0x04;
pub(crate) const MAP_OFFSET: usize = 0x08;
pub(crate) const MAP_LEN: usize = HEADER_LEN as usize - MAP_OFFSET;
pub(crate) const MIN_BLOCK_SIZE: u32 = 0x8000;
pub(crate) const UNUSED: u8 = 0;
pub(crate) const USED: u8 = 1;

/// Which blocks of the image the container kept, and where each one landed.
pub(crate) struct Map {
    block_size: u64,
    /// For each block of the image, its position in the file counted in blocks
    /// after the header, or `None` if it was not stored.
    blocks: Vec<Option<u64>>,
}

impl Map {
    /// Reads the map out of a container's header. `file_len` is the length of
    /// the container itself, which is what says whether the blocks the map
    /// promises are actually there.
    pub(crate) fn read(header: &[u8], file_len: u64) -> Result<Self> {
        let reader = Reader::new(header);

        // The one little endian field anywhere near a GameCube disc. It is the
        // container's own, not the game's.
        let field = reader.slice_at(BLOCK_SIZE_FIELD, 4)?;
        let block_size = u32::from_le_bytes([field[0], field[1], field[2], field[3]]);
        if block_size < MIN_BLOCK_SIZE || block_size % MIN_BLOCK_SIZE != 0 {
            return Err(Error::CorruptHeader("the block size is not a block size"));
        }
        let block_size = u64::from(block_size);

        // One byte per block, saying only whether it is there.
        let mut blocks = Vec::new();
        let mut stored = 0;
        for &used in reader.slice_at(MAP_OFFSET, MAP_LEN)? {
            match used {
                UNUSED => blocks.push(None),
                USED => {
                    blocks.push(Some(stored));
                    stored += 1;
                }
                _ => return Err(Error::CorruptHeader("the block map is not a block map")),
            }
        }

        // The map is a fixed size whatever the image is, so the image ends after
        // the last block anybody stored.
        while blocks.last() == Some(&None) {
            blocks.pop();
        }

        if HEADER_LEN + stored * block_size > file_len {
            return Err(Error::CorruptHeader("the container is missing blocks"));
        }

        Ok(Self { block_size, blocks })
    }

    /// The length of the image inside, holes included.
    pub(crate) const fn image_len(&self) -> u64 {
        self.blocks.len() as u64 * self.block_size
    }

    /// Where an image offset sits in the container, and how far the block it
    /// landed in runs on for. `None` is a block nobody stored.
    pub(crate) fn locate(&self, offset: u64) -> (Option<u64>, u64) {
        let within = offset % self.block_size;
        let index = (offset / self.block_size) as usize;
        let stored = self.blocks.get(index).copied().flatten();
        (
            stored.map(|block| HEADER_LEN + block * self.block_size + within),
            self.block_size - within,
        )
    }
}
