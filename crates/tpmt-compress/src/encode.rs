//! The write path: turns raw bytes into Yaz0 data.

use crate::token::backref::{Backreference, MAX_DISTANCE, MAX_LENGTH, MIN_LENGTH};
use crate::token::{Flags, GROUP_SIZE, TOP_FLAG_BIT, Token};
use crate::{Error, Result, header};

/// After finding a valid backref match, we look to see if a match beside it is
/// better, a "lazy match". These are the knobs that shape detection.
///
/// Each lazy match is a mathematical gamble. By taking the closest match (not
/// deferring), we may miss a better match beside it. But deferring has a cost:
/// it moves the cursor forward +1 and leaves behind a literal byte. So a bare
/// +1 improvement is a tie (no net gain), and is a small loss if it also trips
/// the backreference into its extended-length byte. These tokens are a sunk cost
/// so ideally we want the best chance at a good match that outweighs the loss/tie.
struct LazyMatch {
    /// How many further positions it's willing to chase for a better backreference
    depth: usize,
    /// Extra length demanded on top of the bare +1 improvement required to
    /// defer at all before it's worth spending a literal on.
    slack: usize,
}

impl LazyMatch {
    /// Nintendo's original encoder.
    ///
    /// Only checks for a lazy match once, reducing build time. With only one shot
    /// at the gamble, the floor must be raised to zero so a net-loss cannot occur.
    /// At worst a gamble loses -2 bytes (literal + extended-byte), so we need a
    /// guaranteed +2 improvement (slack of 1).
    const PARITY: Self = Self {
        depth: 1,
        slack: 1, // Improvement of +2
    };

    /// Gets a smaller file.
    ///
    /// Chases a better match over many tries to recover from a bad gamble.
    ///
    /// `depth` of `MAX_LENGTH` is a safe cap: each step must beat the last
    /// by `slack`, and matches can't grow past `MAX_LENGTH` either, so the
    /// chase most likely runs out of room long before using it up.
    // Still a greedy heuristic, not the smallest possible output: a
    // locally-good match can rule out a shorter one that would have opened
    // onto a longer one right after. Optimal parsing (shortest path over
    // every candidate length at every position, not just the longest)
    // would close that gap. Not implemented here without a reason to need it.
    //
    // TODO: Make the cost of specifically hitting the boundary length be slack = 1
    // so we never get a loss, just a more extensive search.
    const EXTENSIVE: Self = Self {
        depth: MAX_LENGTH as usize,
        slack: 0,
    };

    const fn pick(extensive: bool) -> Self {
        if extensive {
            Self::EXTENSIVE
        } else {
            Self::PARITY
        }
    }
}

/// Compresses a buffer into Yaz0 data. See the module docs for the token format.
///
/// `extensive` picks the [`LazyMatch::EXTENSIVE`] strategy to chase a longer
/// backreference over [`LazyMatch::PARITY`] at the cost of speed and byte-accuracy.
///
/// # Errors
///
/// Returns [`Error::TooLarge`] if `input` is too long to fit in a Yaz0 header's
/// 32-bit size field.
pub fn yaz0_encode(input: &[u8], extensive: bool) -> Result<Vec<u8>> {
    encode_with(input, &LazyMatch::pick(extensive))
}

/// Runs the encoder against an explicit strategy.
fn encode_with(input: &[u8], strategy: &LazyMatch) -> Result<Vec<u8>> {
    let decompressed_size =
        u32::try_from(input.len()).map_err(|_| Error::TooLarge { len: input.len() })?;

    let mut out = Vec::with_capacity(header::LEN + input.len());
    out.extend_from_slice(header::MAGIC);
    out.extend_from_slice(&decompressed_size.to_be_bytes());
    out.extend_from_slice(&[0u8; 8]);

    let mut chains = Chains::new(input.len());
    let mut lookahead = Lookahead::default();
    let mut pos = 0;
    let mut group_was_full = false;

    while pos < input.len() {
        let mut flags: Flags = 0;
        let mut body = Vec::new();
        let mut items = 0;

        for flag_bit in 0..GROUP_SIZE {
            if pos >= input.len() {
                break;
            }
            items += 1;

            let matched = match next_token(&mut chains, &mut lookahead, input, pos, strategy) {
                Token::Literal(byte) => {
                    flags |= TOP_FLAG_BIT >> flag_bit;
                    body.push(byte);
                    pos += 1;
                    continue;
                }
                Token::BackReference(matched) => matched,
            };

            matched.write(&mut body);
            pos += matched.length as usize;
        }

        out.push(flags);
        out.append(&mut body);
        group_was_full = items == GROUP_SIZE;
    }

    // Nintendo parity: when the last group comes out full, their encoder still
    // writes the code byte for the next group, empty as it is.
    if group_was_full {
        out.push(0);
    }

    Ok(out)
}

/// A backreference the lazy match has committed to using, plus how many more
/// literal bytes have to come out of [`next_token`] before it's due.
#[derive(Default)]
struct Lookahead {
    pending: Option<Backreference>,
    literals_before: usize,
}

/// Drains any pending lookahead state, else gets the longest match at `pos`
/// and hands it to [`chase_lazy_match`] to see if deferring finds something
/// better.
fn next_token(
    chains: &mut Chains,
    lookahead: &mut Lookahead,
    data: &[u8],
    pos: usize,
    strategy: &LazyMatch,
) -> Token {
    if lookahead.literals_before > 0 {
        lookahead.literals_before -= 1;
        return Token::Literal(data[pos]);
    }
    if let Some(matched) = lookahead.pending.take() {
        return Token::BackReference(matched);
    }

    let Some(best) = chains.longest_match(data, pos) else {
        return Token::Literal(data[pos]);
    };

    let (best, steps) = chase_lazy_match(chains, data, pos, strategy, best);

    if steps == 0 {
        return Token::BackReference(best);
    }
    lookahead.pending = Some(best);
    lookahead.literals_before = steps - 1;
    Token::Literal(data[pos])
}

/// Chases a longer match than `best` past `pos`, per `strategy`. Returns the
/// best match found and how many positions it deferred to find it; `steps
/// == 0` means `best` came back unchanged.
fn chase_lazy_match(
    chains: &mut Chains,
    data: &[u8],
    pos: usize,
    strategy: &LazyMatch,
    mut best: Backreference,
) -> (Backreference, usize) {
    let mut steps = 0;
    while steps < strategy.depth && pos + steps + 1 < data.len() {
        let Some(next) = chains.longest_match(data, pos + steps + 1) else {
            break;
        };
        if next.length as usize <= best.length as usize + strategy.slack {
            break;
        }
        best = next;
        steps += 1;
    }
    (best, steps)
}

/// Bits in a [`HASH_SIZE`] bucket index. Chosen wide enough to keep
/// [`MAX_DISTANCE`]-sized windows of three-byte prefixes from colliding too
/// often, no formula behind the exact value.
const HASH_BITS: u32 = 13;
/// Buckets of three-byte prefixes: one chain per hash of the [`MAX_DISTANCE`]
/// window's worth of positions.
const HASH_SIZE: usize = 1 << HASH_BITS;
/// Position zero is a real one, so absence needs its own value.
const NO_POSITION: u32 = u32::MAX;

/// Where a three-byte prefix turned up earlier: one chain per hash bucket,
/// threaded backward through the input by `prev`.
///
/// Brute force is O(n × [`MAX_DISTANCE`]): compare every position against
/// its whole window. Filing positions by their 3-byte prefix limits each
/// comparison to positions sharing that prefix, cheap when prefixes are
/// varied. But nothing caps chain length: if a whole window shares one
/// prefix (a long run, a short repeat), the chain holds all of it, and the
/// walk is back to O([`MAX_DISTANCE`]) per position.
///
/// Flat `Vec`s, not `HashMap`: `head` is the bucket lookup, a collision
/// just costs one wasted byte comparison in `longest_match`, never a wrong
/// match, and `prev` threads every position sharing a bucket, not just the
/// newest. Both are sized once up front, no per-position allocation.
struct Chains {
    /// Newest position filed under each prefix.
    head: Vec<u32>,
    /// Previous position sharing a position's prefix.
    prev: Vec<u32>,
    /// First position not yet filed. Positions go in in order and once each.
    unfiled: usize,
    /// The chain within the window, reused so the search does not allocate
    /// per byte.
    in_window: Vec<u32>,
}

impl Chains {
    fn new(len: usize) -> Self {
        Self {
            head: vec![NO_POSITION; HASH_SIZE],
            prev: vec![NO_POSITION; len],
            unfiled: 0,
            in_window: Vec::with_capacity(MAX_DISTANCE as usize),
        }
    }

    /// Knuth's multiplicative hash over the three bytes at `pos`, which the
    /// caller has checked are there.
    fn hash(data: &[u8], pos: usize) -> usize {
        let key =
            u32::from(data[pos]) << 16 | u32::from(data[pos + 1]) << 8 | u32::from(data[pos + 2]);
        (key.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
    }

    /// Files every position below `end`. The last `MIN_MATCH - 1` bytes have
    /// no full-length prefix and are left out.
    fn fill(&mut self, data: &[u8], end: usize) {
        let end = end.min(data.len().saturating_sub(MIN_LENGTH as usize - 1));
        while self.unfiled < end {
            let at = self.unfiled;
            let bucket = Self::hash(data, at);
            self.prev[at] = self.head[bucket];
            // `at < data.len()`, and `encode_with` already rejected input longer
            // than `u32::MAX` before `Chains` was built.
            self.head[bucket] = u32::try_from(at).expect("input length fits u32");
            self.unfiled += 1;
        }
    }

    /// The match to take at `pos`, if any.
    ///
    /// Positions that a match skipped over get filed on the next call, so
    /// the caller only ever walks forward.
    fn longest_match(&mut self, data: &[u8], pos: usize) -> Option<Backreference> {
        if pos + MIN_LENGTH as usize > data.len() {
            return None;
        }
        self.fill(data, pos + 1);

        let window = pos.saturating_sub(MAX_DISTANCE as usize);
        let ceiling = (data.len() - pos).min(MAX_LENGTH as usize);

        self.in_window.clear();
        let mut candidate = self.prev[pos];
        while candidate != NO_POSITION && candidate as usize >= window {
            self.in_window.push(candidate);
            candidate = self.prev[candidate as usize];
        }

        let mut best: Option<Backreference> = None;
        // Nintendo parity: the window is scanned front to back and a candidate
        // only replaces a strictly shorter one, so ties go to the earliest.
        // This tie-break only changes which equal-length match is picked, not
        // the final filesize. The chain runs newest first, hence the reverse.
        for &candidate in self.in_window.iter().rev() {
            let start = candidate as usize;
            let best_len = best.map_or(0, |matched: Backreference| matched.length as usize);

            // The byte right after `best_len` must also match, or the candidate
            // can't beat it. Checked early before doing the full compare below.
            if best_len > 0 && data[start + best_len] != data[pos + best_len] {
                continue;
            }

            let mut length = 0;
            while length < ceiling && data[start + length] == data[pos + length] {
                length += 1;
            }
            if length >= MIN_LENGTH as usize && length > best_len {
                best = Some(Backreference {
                    distance: u16::try_from(pos - start).expect("within MAX_DISTANCE"),
                    length: u16::try_from(length).expect("within MAX_LENGTH"),
                });
                // Nothing scanned after this can beat it, so the first candidate
                // to reach the cap wins outright and the walk can stop here.
                if length == ceiling {
                    break;
                }
            }
        }

        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{is_yaz0, yaz0_decode};

    /// Deterministic noise, so a failure repeats.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (state >> 16) as u8
            })
            .collect()
    }

    fn round_trip(input: &[u8]) {
        for extensive in [false, true] {
            let encoded = yaz0_encode(input, extensive).unwrap();
            assert!(
                is_yaz0(&encoded),
                "encoder wrote something else entirely, extensive={extensive}"
            );
            assert_eq!(
                yaz0_decode(&encoded).unwrap(),
                input,
                "on {} bytes, extensive={extensive}",
                input.len()
            );
        }
    }

    /// Literals, back-references, and a run overlapping its own output, which
    /// is why the decoder copies a byte at a time.
    #[test]
    fn round_trips_literals_and_runs() {
        let mut input = b"the quick brown fox jumps over the quick brown dog".to_vec();
        input.extend(std::iter::repeat_n(b'!', 300));
        round_trip(&input);
    }

    /// Both length encodings and the boundary where the nibble runs out.
    #[test]
    fn round_trips_every_match_length() {
        for length in MIN_LENGTH as usize..=MAX_LENGTH as usize + 8 {
            let mut input = noise(length);
            input.extend_from_within(..);
            round_trip(&input);
        }
    }

    /// Lengths either side of a full group of eight, empty included.
    #[test]
    fn round_trips_short_buffers() {
        for length in 0..40 {
            round_trip(&b"abcabcabcabcabcabcabcabcabcabcabcabcabca"[..length]);
        }
    }
}
