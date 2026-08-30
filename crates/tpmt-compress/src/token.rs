/// The flag byte itself: one bit per token in its group.
pub type Flags = u8;
/// Number of tokens preceded by one flag byte, each bit marks each following [`Token`] type
// `Flags` is a `u8`, so `BITS` is always 8: this never truncates.
#[allow(clippy::cast_possible_truncation)]
pub const GROUP_SIZE: u8 = Flags::BITS as u8;
/// Whether the token about to be read is a literal or a match (backref).
pub const TOP_FLAG_BIT: Flags = 1 << (Flags::BITS - 1);

/// One entry in a group of eight: a literal byte, or a back-reference.
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
    /// The single home for that packing, read in [`Backreference::read`] and
    /// written in [`Backreference::write`], so the two directions can't drift
    /// apart from each other.
    #[derive(Clone, Copy)]
    pub struct Backreference {
        pub distance: u16,
        pub length: u16,
    }

    impl Backreference {
        pub fn read(reader: &mut tpmt_bytes::Reader) -> crate::Result<Self> {
            let pair = reader.u16()?;
            // The stored distance is one short of the real one, so a distance
            // field of zero still means "the byte before this one".
            let distance = (pair & DISTANCE_MASK) + 1;
            let length = match pair >> 12 {
                0 => u16::from(reader.u8()?) + MIN_EXTENDED_LENGTH,
                nibble => nibble - 1 + MIN_LENGTH,
            };
            Ok(Self { distance, length })
        }

        pub fn write(self, out: &mut Vec<u8>) {
            // Stored one short, matching the plus one `read` puts back.
            let distance = self.distance - 1;
            if self.length < MIN_EXTENDED_LENGTH {
                // -1 here to make sure it's never represented as 0 (used for extended byte)
                let nibble = self.length - (MIN_LENGTH - 1);
                out.extend_from_slice(&(nibble << 12 | distance).to_be_bytes());
            } else {
                out.extend_from_slice(&distance.to_be_bytes()); // Empty nibble + distance
                // `length <= MAX_LENGTH`, which is `0xFF` past `MIN_EXTENDED_LENGTH`.
                out.push(
                    u8::try_from(self.length - MIN_EXTENDED_LENGTH).expect("within MAX_LENGTH"),
                );
            }
        }
    }
}
