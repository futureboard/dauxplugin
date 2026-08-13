//! Borrowed System Exclusive views.

use crate::midi1::status;
use crate::ump::{Ump, message_type};

/// A borrowed 7-bit System Exclusive message.
///
/// This is a view, never an owner: the bytes live in the host's event list or in a
/// preallocated byte arena, and are valid only for as long as `'a`. Nothing in this type
/// allocates, so it is safe to build and read on the audio thread.
///
/// The slice may include the `0xF0` / `0xF7` delimiters or omit them; use
/// [`SysEx7::payload`] to get the bytes between them either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysEx7<'a>(
    /// The borrowed bytes, exactly as they appear on the wire.
    pub &'a [u8],
);

impl<'a> SysEx7<'a> {
    /// [any-thread] Wraps a borrowed byte slice.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// [any-thread] The borrowed bytes.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// [any-thread] Number of borrowed bytes.
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// [any-thread] `true` when nothing is borrowed.
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// [any-thread] `true` when every byte is a legal 7-bit data byte (`<= 0x7F`), ignoring
    /// a leading `0xF0` and a trailing `0xF7`.
    pub fn is_valid(&self) -> bool {
        self.payload().iter().all(|b| *b <= 0x7F)
    }

    /// [any-thread] The bytes between the delimiters: a leading `0xF0` and a trailing `0xF7`
    /// are stripped when present.
    pub fn payload(&self) -> &'a [u8] {
        let bytes: &'a [u8] = self.0;
        let mut start = 0usize;
        let mut end = bytes.len();
        if end > start && bytes[start] == status::SYSEX_START {
            start += 1;
        }
        if end > start && bytes[end - 1] == status::SYSEX_END {
            end -= 1;
        }
        &bytes[start..end]
    }

    /// [main-thread] [audio-thread] Splits the payload into MIDI 2.0 `SysEx7` data packets
    /// (UMP message type `0x3`), six bytes per packet.
    ///
    /// The iterator allocates nothing and yields nothing at all for an empty payload — there
    /// is no data to transmit. Data bytes are masked to seven bits.
    pub fn ump_packets(&self, group: u8) -> SysEx7UmpIter<'a> {
        SysEx7UmpIter {
            bytes: self.payload(),
            group: group & 0x0F,
            pos: 0,
        }
    }
}

/// Iterator over the UMP `SysEx7` data packets of a [`SysEx7`] payload.
///
/// Created by [`SysEx7::ump_packets`]. Every item is a two-word packet whose status nibble is
/// `0` (complete), `1` (start), `2` (continue) or `3` (end).
#[derive(Clone, Debug)]
pub struct SysEx7UmpIter<'a> {
    bytes: &'a [u8],
    group: u8,
    pos: usize,
}

impl Iterator for SysEx7UmpIter<'_> {
    type Item = Ump;

    fn next(&mut self) -> Option<Ump> {
        if self.pos >= self.bytes.len() {
            return None;
        }
        let remaining = self.bytes.len() - self.pos;
        let take = if remaining > 6 { 6 } else { remaining };
        let first = self.pos == 0;
        let last = remaining <= 6;
        let status: u32 = match (first, last) {
            (true, true) => 0,  // complete in one packet
            (true, false) => 1, // start
            (false, true) => 3, // end
            (false, false) => 2,
        };

        let mut data = [0u8; 6];
        for (dst, src) in data.iter_mut().zip(&self.bytes[self.pos..self.pos + take]) {
            *dst = *src & 0x7F;
        }
        self.pos += take;

        let w0 = ((message_type::DATA_64 as u32) << 28)
            | ((self.group as u32) << 24)
            | (status << 20)
            | ((take as u32) << 16)
            | ((data[0] as u32) << 8)
            | (data[1] as u32);
        let w1 = ((data[2] as u32) << 24)
            | ((data[3] as u32) << 16)
            | ((data[4] as u32) << 8)
            | (data[5] as u32);
        Some(Ump::from_words2(w0, w1))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.bytes.len().saturating_sub(self.pos);
        let packets = remaining.div_ceil(6);
        (packets, Some(packets))
    }
}

impl ExactSizeIterator for SysEx7UmpIter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_view_is_empty() {
        let s = SysEx7::new(&[]);
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.is_valid());
        assert_eq!(s.payload(), &[] as &[u8]);
        assert_eq!(s.ump_packets(0).count(), 0);
    }

    #[test]
    fn delimiters_are_stripped_from_the_payload() {
        let framed = [0xF0u8, 0x7E, 0x00, 0x06, 0x01, 0xF7];
        assert_eq!(SysEx7::new(&framed).payload(), &[0x7E, 0x00, 0x06, 0x01]);

        let bare = [0x7Eu8, 0x00, 0x06, 0x01];
        assert_eq!(SysEx7::new(&bare).payload(), &bare);

        // Only the leading delimiter.
        let head = [0xF0u8, 0x41, 0x10];
        assert_eq!(SysEx7::new(&head).payload(), &[0x41, 0x10]);

        // Only the trailing delimiter.
        let tail = [0x41u8, 0x10, 0xF7];
        assert_eq!(SysEx7::new(&tail).payload(), &[0x41, 0x10]);

        // Degenerate: just the delimiters.
        assert_eq!(SysEx7::new(&[0xF0, 0xF7]).payload(), &[] as &[u8]);
        assert_eq!(SysEx7::new(&[0xF0]).payload(), &[] as &[u8]);
        assert_eq!(SysEx7::new(&[0xF7]).payload(), &[] as &[u8]);
    }

    #[test]
    fn validity_checks_the_payload_only() {
        assert!(SysEx7::new(&[0xF0, 0x01, 0x7F, 0xF7]).is_valid());
        assert!(!SysEx7::new(&[0xF0, 0x01, 0x80, 0xF7]).is_valid());
        // A stray 0xF7 in the middle is a data-byte violation.
        assert!(!SysEx7::new(&[0xF0, 0xF7, 0x01, 0xF7]).is_valid());
    }

    #[test]
    fn a_short_payload_becomes_one_complete_packet() {
        let s = SysEx7::new(&[0xF0, 1, 2, 3, 4, 5, 6, 0xF7]);
        let packets: Vec<Ump> = s.ump_packets(5).collect();
        assert_eq!(packets.len(), 1);
        let p = packets[0];
        assert_eq!(p.message_type(), message_type::DATA_64);
        assert_eq!(p.group(), 5);
        assert_eq!(p.status(), 0, "complete-in-one-packet");
        assert_eq!((p.words[0] >> 16) & 0x0F, 6, "byte count");
        assert_eq!(p.words[0] & 0xFFFF, 0x0102);
        assert_eq!(p.words[1], 0x0304_0506);
        assert_eq!(p.len, 2);
        assert!(p.is_well_formed());
    }

    #[test]
    fn a_seven_byte_payload_becomes_start_plus_end() {
        let s = SysEx7::new(&[1, 2, 3, 4, 5, 6, 7]);
        let packets: Vec<Ump> = s.ump_packets(0).collect();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].status(), 1, "start");
        assert_eq!((packets[0].words[0] >> 16) & 0x0F, 6);
        assert_eq!(packets[1].status(), 3, "end");
        assert_eq!((packets[1].words[0] >> 16) & 0x0F, 1);
        assert_eq!(packets[1].words[0] & 0xFFFF, 0x0700);
        assert_eq!(packets[1].words[1], 0);
    }

    #[test]
    fn a_long_payload_becomes_start_continue_end() {
        let bytes: Vec<u8> = (1u8..=13).collect();
        let s = SysEx7::new(&bytes);
        let packets: Vec<Ump> = s.ump_packets(0).collect();
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0].status(), 1, "start");
        assert_eq!(packets[1].status(), 2, "continue");
        assert_eq!(packets[2].status(), 3, "end");
        assert_eq!((packets[2].words[0] >> 16) & 0x0F, 1);

        // Reassembling the payload gives the original bytes back.
        let mut out = Vec::new();
        for p in &packets {
            let n = ((p.words[0] >> 16) & 0x0F) as usize;
            let raw = [
                (p.words[0] >> 8) as u8,
                p.words[0] as u8,
                (p.words[1] >> 24) as u8,
                (p.words[1] >> 16) as u8,
                (p.words[1] >> 8) as u8,
                p.words[1] as u8,
            ];
            out.extend_from_slice(&raw[..n]);
        }
        assert_eq!(out, bytes);
    }

    #[test]
    fn exactly_twelve_bytes_becomes_start_plus_end() {
        let bytes: Vec<u8> = (1u8..=12).collect();
        let packets: Vec<Ump> = SysEx7::new(&bytes).ump_packets(0).collect();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].status(), 1);
        assert_eq!(packets[1].status(), 3);
        assert_eq!((packets[1].words[0] >> 16) & 0x0F, 6);
    }

    #[test]
    fn data_bytes_are_masked_to_seven_bits() {
        let packets: Vec<Ump> = SysEx7::new(&[0xFF, 0x81]).ump_packets(0).collect();
        assert_eq!(packets[0].words[0] & 0xFFFF, 0x7F01);
    }

    #[test]
    fn size_hint_is_exact() {
        for n in 0usize..20 {
            let bytes: Vec<u8> = (0..n).map(|i| (i & 0x7F) as u8).collect();
            let it = SysEx7::new(&bytes).ump_packets(0);
            let expected = n.div_ceil(6);
            assert_eq!(it.size_hint(), (expected, Some(expected)), "n = {n}");
            assert_eq!(it.len(), expected, "n = {n}");
            assert_eq!(it.count(), expected, "n = {n}");
        }
    }

    #[test]
    fn group_is_masked() {
        let packets: Vec<Ump> = SysEx7::new(&[1]).ump_packets(0xFF).collect();
        assert_eq!(packets[0].group(), 0x0F);
    }
}
