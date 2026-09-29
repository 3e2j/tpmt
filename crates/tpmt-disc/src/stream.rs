//! Reading an image's files once, front to back. See [`Disc::stream`].

use crate::{Disc, Entry, Result, Span};

/// Every file on a disc in offset order. Made by [`Disc::stream`].
pub struct Stream<'a> {
    disc: &'a Disc,
    /// Files still to read, nearest first.
    files: std::vec::IntoIter<(&'a str, Span)>,
}

impl<'a> Stream<'a> {
    pub(crate) fn new(disc: &'a Disc, entries: &'a [Entry]) -> Self {
        let mut files: Vec<_> = entries
            .iter()
            .filter_map(|entry| Some((entry.path(), entry.span()?)))
            .collect();
        files.sort_unstable_by_key(|(_, span)| span.offset);
        Self {
            disc,
            files: files.into_iter(),
        }
    }
}

impl<'a> Iterator for Stream<'a> {
    type Item = Result<(&'a str, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let (path, span) = self.files.next()?;
        Some(self.disc.read(span).map(|bytes| (path, bytes)))
    }
}
