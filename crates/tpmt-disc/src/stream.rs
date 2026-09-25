//! Reading an image once, front to back. See [`Disc::stream`].

use sha1::{Digest, Sha1};

use crate::{Disc, Entry, Result, Span};

/// Gaps between files are only hashed, so they're read in pieces this size.
const CHUNK: u64 = 1 << 20;

/// Every file on a disc in offset order, hashing the whole image as it goes.
/// Made by [`Disc::stream`].
pub struct Stream<'a, F> {
    disc: &'a Disc,
    /// Files still to read, nearest first.
    files: std::vec::IntoIter<(&'a str, Span)>,
    hash: Sha1,
    /// Everything in the image before this has gone into `hash`.
    hashed: u64,
    /// Called with the number of bytes each read added to `hash`.
    progress: F,
}

impl<'a, F: FnMut(u64)> Stream<'a, F> {
    pub(crate) fn new(disc: &'a Disc, entries: &'a [Entry], progress: F) -> Self {
        let mut files: Vec<_> = entries
            .iter()
            .filter_map(|entry| Some((entry.path(), entry.span()?)))
            .collect();
        files.sort_unstable_by_key(|(_, span)| span.offset);
        Self {
            disc,
            files: files.into_iter(),
            hash: Sha1::new(),
            hashed: 0,
            progress,
        }
    }

    /// Hashes the rest of the image, files not yet handed out included, and
    /// returns what [`Disc::sha1`] would.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Read`](crate::Error::Read).
    pub fn finish(mut self) -> Result<String> {
        self.hash_to(self.disc.len())?;
        Ok(format!("{:x}", self.hash.finalize()))
    }

    fn hash_to(&mut self, end: u64) -> Result<()> {
        while self.hashed < end {
            let span = Span {
                offset: self.hashed,
                size: CHUNK.min(end - self.hashed),
            };
            let bytes = self.disc.read(span)?;
            self.absorb(span, &bytes);
        }
        Ok(())
    }

    fn read(&mut self, span: Span) -> Result<Vec<u8>> {
        self.hash_to(span.offset)?;
        let bytes = self.disc.read(span)?;
        self.absorb(span, &bytes);
        Ok(bytes)
    }

    /// Hashes the part of `bytes`, read from `span`, that lies past `hashed`.
    /// A file table can point two files at the same bytes, so `span` may start
    /// or end inside the hashed part, but never starts past it.
    fn absorb(&mut self, span: Span, bytes: &[u8]) {
        let end = span.offset + span.size;
        let seen = self.hashed - span.offset;
        if let Some(fresh) = usize::try_from(seen)
            .ok()
            .and_then(|seen| bytes.get(seen..))
        {
            self.hash.update(fresh);
            (self.progress)(end.saturating_sub(self.hashed));
        }
        self.hashed = self.hashed.max(end);
    }
}

impl<'a, F: FnMut(u64)> Iterator for Stream<'a, F> {
    type Item = Result<(&'a str, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let (path, span) = self.files.next()?;
        Some(self.read(span).map(|bytes| (path, bytes)))
    }
}
