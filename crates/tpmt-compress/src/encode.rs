//! The write path: turns raw bytes into a Yaz0 stream.
//!
//! Tokens come in eights behind a flag byte, one bit each: 1 for a literal
//! byte, 0 for a back-reference (a 12-bit distance and a length nibble,
//! each stored with a small offset baked in, detailed at their use). See the
//! crate docs for the shape of the format as a whole.

use crate::{Error, Result, YAZ0_HEADER_LEN, YAZ0_MAGIC};

/// Shortest run the format can store: the nibble holds length minus two, and
/// a zero nibble already means the extended form, so nothing encodes a two.
const MIN_MATCH: usize = 3;
/// Longest run a reference can carry: a zero nibble plus the extra byte.
const MAX_MATCH: usize = 0xFF + 0x12;
/// The distance field is twelve bits, stored one short of the real distance.
const MAX_DISTANCE: usize = 0x1000;
/// First length too big for the nibble, so it spends the extra byte.
const EXTENDED_MATCH: usize = 0x12;

/// Compresses a buffer into a Yaz0 stream that matches Nintendo's encoder
/// byte for byte. See the module docs for the token format.
///
/// Three choices below exist only for that parity, each marked. Drop any one
/// and the stream is still valid and still decodes, so no test in this crate
/// can catch it; only a comparison against the real streams off a disc shows
/// the difference.
pub fn yaz0_encode(input: &[u8]) -> Result<Vec<u8>> {
    let size = u32::try_from(input.len()).map_err(|_| Error::TooLarge { len: input.len() })?;

    let mut out = Vec::with_capacity(YAZ0_HEADER_LEN + input.len());
    out.extend_from_slice(YAZ0_MAGIC);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&[0u8; 8]);

    let mut chains = Chains::new(input.len());
    let mut held = None;
    let mut pos = 0;
    let mut group_was_full = false;

    while pos < input.len() {
        // The flag byte comes first but is not known until all eight tokens
        // are picked, so the group is built up separately.
        let mut code = 0u8;
        let mut body = Vec::new();
        let mut items = 0;

        for bit in 0..8 {
            if pos >= input.len() {
                break;
            }
            items += 1;

            let Some(run) = next_run(&mut chains, &mut held, input, pos) else {
                code |= 0x80 >> bit;
                body.push(input[pos]);
                pos += 1;
                continue;
            };

            // Stored one short, matching the plus one the decoder puts back.
            let distance = (run.distance - 1) as u16;
            if run.length < EXTENDED_MATCH {
                let nibble = (run.length - 2) as u16;
                body.extend_from_slice(&(nibble << 12 | distance).to_be_bytes());
            } else {
                body.extend_from_slice(&distance.to_be_bytes());
                body.push((run.length - EXTENDED_MATCH) as u8);
            }
            pos += run.length;
        }

        out.push(code);
        out.append(&mut body);
        group_was_full = items == 8;
    }

    // Nintendo parity: when the last group comes out full, their encoder still
    // writes the code byte for the next group, empty as it is.
    if group_was_full {
        out.push(0);
    }

    Ok(out)
}

/// A back-reference, in the terms the decoder reads it back in.
#[derive(Clone, Copy)]
struct Run {
    distance: usize,
    length: usize,
}

/// The token for `pos`: a run, or `None` to spend the byte as a literal.
///
/// One byte of lookahead. If the next position starts a run at least two bytes
/// longer, this byte goes out as a literal and that run is held for the next
/// call rather than searched for twice.
fn next_run(chains: &mut Chains, held: &mut Option<Run>, data: &[u8], pos: usize) -> Option<Run> {
    if let Some(run) = held.take() {
        return Some(run);
    }

    let here = chains.longest_match(data, pos)?;
    if pos + 1 < data.len()
        && let Some(ahead) = chains.longest_match(data, pos + 1)
        && ahead.length >= here.length + 2
    {
        *held = Some(ahead);
        return None;
    }
    Some(here)
}

/// Buckets of three-byte prefixes, 8k of them for a 4k window.
const HASH_BITS: u32 = 13;
const HASH_SIZE: usize = 1 << HASH_BITS;
/// Position zero is a real one, so absence needs its own value.
const NO_POSITION: u32 = u32::MAX;

/// Where a three-byte prefix turned up earlier.
///
/// Trying all 4096 window positions per byte is what makes a plain encoder
/// slow, and most of that is wasted on starts that do not match at all. So
/// every position is filed under the three bytes starting there, pointing back
/// at the previous position filed under the same three. A bucket is a chain
/// running backwards through the input.
///
/// Searching only the chain gives the same answer: anything not in it matches
/// two bytes at most, which is a literal either way.
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
        Chains {
            head: vec![NO_POSITION; HASH_SIZE],
            prev: vec![NO_POSITION; len],
            unfiled: 0,
            in_window: Vec::with_capacity(MAX_DISTANCE),
        }
    }

    /// Knuth's multiplicative hash over the three bytes at `pos`, which the
    /// caller has checked are there.
    fn hash(data: &[u8], pos: usize) -> usize {
        let key =
            u32::from(data[pos]) << 16 | u32::from(data[pos + 1]) << 8 | u32::from(data[pos + 2]);
        (key.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
    }

    /// Files every position below `end`. The last two bytes have no three-byte
    /// prefix and are left out.
    fn fill(&mut self, data: &[u8], end: usize) {
        let end = end.min(data.len().saturating_sub(MIN_MATCH - 1));
        while self.unfiled < end {
            let at = self.unfiled;
            let bucket = Self::hash(data, at);
            self.prev[at] = self.head[bucket];
            self.head[bucket] = at as u32;
            self.unfiled += 1;
        }
    }

    /// The run to take at `pos`, if any.
    ///
    /// Positions that a run skipped over get filed on the next call, so the
    /// caller only ever walks forward.
    fn longest_match(&mut self, data: &[u8], pos: usize) -> Option<Run> {
        if pos + MIN_MATCH > data.len() {
            return None;
        }
        self.fill(data, pos + 1);

        let window = pos.saturating_sub(MAX_DISTANCE);
        // Nintendo parity: the search stops at MAX_MATCH, not just the writer.
        // Measuring the true length lets a longer match further on win a tie it
        // should have lost.
        let ceiling = (data.len() - pos).min(MAX_MATCH);

        self.in_window.clear();
        let mut candidate = self.prev[pos];
        while candidate != NO_POSITION && candidate as usize >= window {
            self.in_window.push(candidate);
            candidate = self.prev[candidate as usize];
        }

        let mut best: Option<Run> = None;
        // Nintendo parity: the window is scanned front to back and a candidate
        // only replaces a strictly shorter one, so ties go to the earliest. The
        // chain runs newest first, hence the reverse. It also lets the first
        // candidate to reach the cap win outright, so the walk can stop there.
        for &candidate in self.in_window.iter().rev() {
            let start = candidate as usize;
            let taken = best.map_or(0, |run: Run| run.length);

            // To beat the best so far it has to reach past where that one
            // ends, so the byte sitting there settles it on its own.
            if taken > 0 && data[start + taken] != data[pos + taken] {
                continue;
            }

            let mut length = 0;
            while length < ceiling && data[start + length] == data[pos + length] {
                length += 1;
            }
            if length >= MIN_MATCH && length > taken {
                best = Some(Run {
                    distance: pos - start,
                    length,
                });
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
        let encoded = yaz0_encode(input).unwrap();
        assert!(is_yaz0(&encoded), "encoder wrote something else entirely");
        assert_eq!(
            yaz0_decode(&encoded).unwrap(),
            input,
            "on {} bytes",
            input.len()
        );
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
        for length in MIN_MATCH..=MAX_MATCH + 8 {
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
