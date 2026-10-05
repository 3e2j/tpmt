/// One step of the output: a literal byte, or a back-reference.
pub enum Token {
    Literal(u8),
    BackReference(backref::Backreference),
}

pub mod backref {
    /// Size of the `u16` pair alone: a 4-bit `length` nibble and a 12-bit
    /// `distance`. An optional extra byte can follow to account for long
    /// lengths that cannot fit in the nibble alone; that byte is not
    /// counted here.
    const PAIR_SIZE: u16 = 2;

    /// To warrant a backref, we need at least one more than the min size it takes
    /// to hold a backref, otherwise we could have just stored the literal cheaply.
    pub const MIN_LENGTH: u16 = PAIR_SIZE + 1;

    /// Length too big for to hold in the nibble, so an extra byte follows.
    /// Extended byte stores how far past this point the length reaches, not the length
    /// itself.
    pub const MIN_EXTENDED_LENGTH: u16 = MIN_LENGTH + 0xF;

    /// Max amount that can be represented.
    /// A full extended byte on top of `MIN_EXTENDED_LENGTH`, which already
    /// bakes in the full nibble.
    pub const MAX_LENGTH: u16 = 0xFF + MIN_EXTENDED_LENGTH;

    /// Twelve-bit field: the raw stored value's bitmask.
    pub const DISTANCE_MASK: u16 = 0xFFF;
    /// A distance of 0 will still jump back one, so the real distance is the
    /// stored value plus one.
    pub const MAX_DISTANCE: u16 = DISTANCE_MASK + 1;

    /// A back-reference's distance and length, decoupled from the u16-pair
    /// plus optional extension byte it's packed into on the wire.
    ///
    /// The single home for that packing, read in [`Backreference::from_pair`]
    /// and written in [`Backreference::pair`] and [`Backreference::extended`],
    /// so the two directions can't drift apart. Where the pair and its
    /// extended byte go is up to each format.
    #[derive(Clone, Copy)]
    pub struct Backreference {
        distance: u16,
        length: u16,
    }

    impl Backreference {
        /// A match of `length` bytes found `distance` bytes back, or `None`
        /// if the wire format has no room for it.
        pub fn new(distance: usize, length: usize) -> Option<Self> {
            let distance = u16::try_from(distance)
                .ok()
                .filter(|distance| (1..=MAX_DISTANCE).contains(distance))?;
            let length = u16::try_from(length)
                .ok()
                .filter(|length| (MIN_LENGTH..=MAX_LENGTH).contains(length))?;
            Some(Self { distance, length })
        }

        pub const fn length(self) -> u16 {
            self.length
        }

        /// Unpacks a pair, calling `extended` for the byte that follows it
        /// when the nibble is zero.
        pub fn from_pair(
            pair: u16,
            extended: impl FnOnce() -> crate::Result<u8>,
        ) -> crate::Result<Self> {
            // The stored distance is one short of the real one, so a distance
            // field of zero still means "the byte before this one".
            let distance = (pair & DISTANCE_MASK) + 1;
            let length = match pair >> 12 {
                0 => u16::from(extended()?) + MIN_EXTENDED_LENGTH,
                nibble => nibble - 1 + MIN_LENGTH,
            };
            Ok(Self { distance, length })
        }

        /// The pair this packs into, the inverse of [`from_pair`](Self::from_pair).
        pub const fn pair(self) -> u16 {
            // Stored one short, matching the plus one `from_pair` puts back.
            let distance = self.distance - 1;
            if self.length < MIN_EXTENDED_LENGTH {
                // -1 here to make sure it's never represented as 0 (used for extended byte)
                let nibble = self.length - (MIN_LENGTH - 1);
                nibble << 12 | distance
            } else {
                distance // Empty nibble + distance
            }
        }

        /// The byte that follows the pair, for a length the nibble can't hold.
        pub fn extended(self) -> Option<u8> {
            // Only `new` and `from_pair` build one, and both keep `length <=
            // MAX_LENGTH`, which is `0xFF` past `MIN_EXTENDED_LENGTH`.
            #[allow(clippy::expect_used)]
            (self.length >= MIN_EXTENDED_LENGTH).then(|| {
                u8::try_from(self.length - MIN_EXTENDED_LENGTH).expect("within MAX_LENGTH")
            })
        }

        /// Copies this run into `out` at `pos`, from the bytes already written
        /// behind it, and returns where the run ends.
        ///
        /// # Errors
        ///
        /// - [`Error::BackReference`](crate::Error::BackReference) if it
        ///   reaches before the start of `out`
        /// - [`Error::SizeMismatch`](crate::Error::SizeMismatch) if it runs
        ///   past the end of `out`
        pub fn copy(self, out: &mut [u8], pos: usize) -> crate::Result<usize> {
            let (distance, length) = (self.distance as usize, self.length as usize);
            let start = pos
                .checked_sub(distance)
                .ok_or(crate::Error::BackReference { pos, distance })?;
            let end = pos + length;
            if end > out.len() {
                return Err(crate::Error::SizeMismatch {
                    expected: out.len(),
                    actual: end,
                });
            }

            // A run longer than its distance repeats the bytes behind it, so
            // each pass can copy everything written since `start`, doubling
            // the chunk.
            let mut pos = pos;
            while pos < end {
                let chunk = (pos - start).min(end - pos);
                out.copy_within(start..start + chunk, pos);
                pos += chunk;
            }
            Ok(end)
        }
    }
}
