//! The MIDI 2.0 Universal MIDI Packet.

/// UMP message type values, the top four bits of the first word.
pub mod message_type {
    /// Utility messages (NOOP, jitter reduction clock). One word.
    pub const UTILITY: u8 = 0x0;
    /// System Real Time and System Common. One word.
    pub const SYSTEM: u8 = 0x1;
    /// MIDI 1.0 Channel Voice, packed into a single word. One word.
    pub const MIDI1_CHANNEL_VOICE: u8 = 0x2;
    /// 64-bit data messages: System Exclusive 7. Two words.
    pub const DATA_64: u8 = 0x3;
    /// MIDI 2.0 Channel Voice. Two words.
    pub const MIDI2_CHANNEL_VOICE: u8 = 0x4;
    /// 128-bit data messages: System Exclusive 8 and Mixed Data Set. Four words.
    pub const DATA_128: u8 = 0x5;
    /// Flex Data messages. Four words.
    pub const FLEX_DATA: u8 = 0xD;
    /// UMP Stream messages (endpoint discovery, function blocks). Four words.
    pub const UMP_STREAM: u8 = 0xF;
}

/// A MIDI 2.0 Universal MIDI Packet: one to four 32-bit words.
///
/// The packet is always four words wide in memory; `len` says how many of them belong to the
/// message. Words beyond `len` are ignored and should be zero. This mirrors
/// `DauxEventMidi2V1` in `docs/specifications/abi-v1.md` §9 so an adapter can copy the words
/// straight across.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ump {
    /// The packet words, most significant word first. Only `words[..len]` is meaningful.
    pub words: [u32; 4],
    /// Number of meaningful words, `1..=4`.
    pub len: u8,
}

impl Ump {
    /// [any-thread] A one-word packet.
    pub const fn from_word(w0: u32) -> Self {
        Self {
            words: [w0, 0, 0, 0],
            len: 1,
        }
    }

    /// [any-thread] A two-word packet.
    pub const fn from_words2(w0: u32, w1: u32) -> Self {
        Self {
            words: [w0, w1, 0, 0],
            len: 2,
        }
    }

    /// [any-thread] A four-word packet.
    pub const fn from_words4(w0: u32, w1: u32, w2: u32, w3: u32) -> Self {
        Self {
            words: [w0, w1, w2, w3],
            len: 4,
        }
    }

    /// [any-thread] Builds a packet from raw words, rejecting a `len` outside `1..=4`.
    pub const fn try_new(words: [u32; 4], len: u8) -> Option<Self> {
        if len == 0 || len > 4 {
            return None;
        }
        Some(Self { words, len })
    }

    /// [any-thread] The message type, bits 31..28 of the first word.
    pub const fn message_type(&self) -> u8 {
        (self.words[0] >> 28) as u8
    }

    /// [any-thread] The UMP group `0..=15`, bits 27..24 of the first word.
    pub const fn group(&self) -> u8 {
        ((self.words[0] >> 24) & 0x0F) as u8
    }

    /// [any-thread] Replaces the group nibble in place.
    pub const fn set_group(&mut self, group: u8) {
        self.words[0] = (self.words[0] & !(0x0F << 24)) | (((group as u32) & 0x0F) << 24);
    }

    /// [any-thread] The status nibble, bits 23..20 of the first word.
    ///
    /// Meaningful for channel voice and data message types; for
    /// [`message_type::SYSTEM`] the status is a whole byte, see [`Ump::status_byte`].
    pub const fn status(&self) -> u8 {
        ((self.words[0] >> 20) & 0x0F) as u8
    }

    /// [any-thread] Bits 23..16 of the first word as a whole byte. This is the MIDI 1.0
    /// status byte for [`message_type::SYSTEM`] and [`message_type::MIDI1_CHANNEL_VOICE`].
    pub const fn status_byte(&self) -> u8 {
        ((self.words[0] >> 16) & 0xFF) as u8
    }

    /// [any-thread] The channel nibble, bits 19..16 of the first word.
    ///
    /// Meaningful only for channel voice message types.
    pub const fn channel(&self) -> u8 {
        ((self.words[0] >> 16) & 0x0F) as u8
    }

    /// [any-thread] Number of meaningful words, clamped to `0..=4`.
    pub const fn word_count(&self) -> usize {
        if self.len > 4 { 4 } else { self.len as usize }
    }

    /// [any-thread] The meaningful words only.
    pub fn as_words(&self) -> &[u32] {
        &self.words[..self.word_count()]
    }

    /// [any-thread] The number of words a packet of message type `mt` must occupy.
    ///
    /// Follows the UMP specification's size table, including the reserved message types so
    /// that a stream of unknown packets can still be walked:
    ///
    /// | message type      | words |
    /// | ----------------- | ----- |
    /// | `0x0`, `0x1`, `0x2`, `0x6`, `0x7` | 1 |
    /// | `0x3`, `0x4`, `0x8`, `0x9`, `0xA` | 2 |
    /// | `0xB`, `0xC`                      | 3 |
    /// | `0x5`, `0xD`, `0xE`, `0xF`        | 4 |
    pub const fn words_for_message_type(mt: u8) -> u8 {
        match mt & 0x0F {
            0x0 | 0x1 | 0x2 | 0x6 | 0x7 => 1,
            0x3 | 0x4 | 0x8 | 0x9 | 0xA => 2,
            0xB | 0xC => 3,
            _ => 4,
        }
    }

    /// [any-thread] `true` when `len` is in range and matches the packet's message type.
    pub const fn is_well_formed(&self) -> bool {
        self.len != 0
            && self.len <= 4
            && self.len == Self::words_for_message_type(self.message_type())
    }

    /// [any-thread] Rebuilds `len` from the message type, discarding any words the type
    /// does not use.
    pub const fn normalized(mut self) -> Self {
        let want = Self::words_for_message_type(self.message_type());
        let mut i = want as usize;
        while i < 4 {
            self.words[i] = 0;
            i += 1;
        }
        self.len = want;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_type_and_group_decode() {
        let u = Ump::from_words2(0x4B3F_0000, 0xDEAD_BEEF);
        assert_eq!(u.message_type(), message_type::MIDI2_CHANNEL_VOICE);
        assert_eq!(u.group(), 0xB);
        assert_eq!(u.status(), 0x3);
        assert_eq!(u.channel(), 0xF);
        assert_eq!(u.status_byte(), 0x3F);
        assert_eq!(u.word_count(), 2);
        assert_eq!(u.as_words(), &[0x4B3F_0000, 0xDEAD_BEEF]);
    }

    #[test]
    fn word_counts_follow_the_ump_size_table() {
        let expected = [1u8, 1, 1, 2, 2, 4, 1, 1, 2, 2, 2, 3, 3, 4, 4, 4];
        for (mt, want) in expected.into_iter().enumerate() {
            assert_eq!(
                Ump::words_for_message_type(mt as u8),
                want,
                "message type 0x{mt:X}"
            );
        }
    }

    #[test]
    fn the_four_message_types_we_model_have_the_documented_sizes() {
        assert_eq!(Ump::words_for_message_type(message_type::UTILITY), 1);
        assert_eq!(Ump::words_for_message_type(message_type::SYSTEM), 1);
        assert_eq!(
            Ump::words_for_message_type(message_type::MIDI1_CHANNEL_VOICE),
            1
        );
        assert_eq!(Ump::words_for_message_type(message_type::DATA_64), 2);
        assert_eq!(
            Ump::words_for_message_type(message_type::MIDI2_CHANNEL_VOICE),
            2
        );
        assert_eq!(Ump::words_for_message_type(message_type::DATA_128), 4);
        assert_eq!(Ump::words_for_message_type(message_type::FLEX_DATA), 4);
        assert_eq!(Ump::words_for_message_type(message_type::UMP_STREAM), 4);
    }

    #[test]
    fn high_nibble_of_message_type_argument_is_ignored() {
        assert_eq!(
            Ump::words_for_message_type(0xF4),
            Ump::words_for_message_type(0x04)
        );
    }

    #[test]
    fn try_new_rejects_out_of_range_lengths() {
        assert!(Ump::try_new([0; 4], 0).is_none());
        assert!(Ump::try_new([0; 4], 5).is_none());
        assert!(Ump::try_new([0; 4], 255).is_none());
        assert_eq!(Ump::try_new([1, 2, 3, 4], 3).map(|u| u.len), Some(3));
    }

    #[test]
    fn well_formedness_checks_len_against_message_type() {
        assert!(Ump::from_word(0x2000_0000).is_well_formed());
        assert!(!Ump::from_words2(0x2000_0000, 0).is_well_formed());
        assert!(Ump::from_words2(0x4000_0000, 0).is_well_formed());
        assert!(!Ump::from_word(0x4000_0000).is_well_formed());
        assert!(Ump::from_words4(0x5000_0000, 0, 0, 0).is_well_formed());
        assert!(
            !Ump {
                words: [0x4000_0000, 0, 0, 0],
                len: 0
            }
            .is_well_formed()
        );
        assert!(
            !Ump {
                words: [0x5000_0000, 0, 0, 0],
                len: 9
            }
            .is_well_formed()
        );
    }

    #[test]
    fn normalized_fixes_len_and_clears_unused_words() {
        let u = Ump {
            words: [0x2000_0000, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF],
            len: 4,
        };
        let n = u.normalized();
        assert_eq!(n.len, 1);
        assert_eq!(n.words, [0x2000_0000, 0, 0, 0]);
        assert!(n.is_well_formed());

        let u = Ump {
            words: [0x4000_0000, 7, 0xFF, 0xFF],
            len: 1,
        };
        let n = u.normalized();
        assert_eq!(n.len, 2);
        assert_eq!(n.words, [0x4000_0000, 7, 0, 0]);
    }

    #[test]
    fn word_count_clamps_a_corrupt_len() {
        let u = Ump {
            words: [1, 2, 3, 4],
            len: 200,
        };
        assert_eq!(u.word_count(), 4);
        assert_eq!(u.as_words().len(), 4);
    }

    #[test]
    fn set_group_only_touches_the_group_nibble() {
        let mut u = Ump::from_words2(0x4F9F_3C00, 0xFFFF_0000);
        u.set_group(0x2);
        assert_eq!(u.words[0], 0x429F_3C00);
        assert_eq!(u.group(), 2);
        u.set_group(0xFF);
        assert_eq!(u.group(), 0xF);
        assert_eq!(u.words[1], 0xFFFF_0000);
    }
}
