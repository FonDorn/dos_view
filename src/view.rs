//! Display modes and layout arithmetic.
//!
//! All three modes are the same bytes formatted differently. The only thing
//! that changes is how many cells one byte takes, and from that follows how
//! many bytes fit on a line.

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Binary: every byte as eight bits.
    Binary,
    /// Classic hex dump with a text column on the right.
    Hex,
    /// Text only, decoded with the current encoding.
    Text,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Binary => "BIN",
            Mode::Hex => "HEX",
            Mode::Text => "TEXT",
        }
    }

    /// Whether there is a text column on the right.
    pub fn has_text_column(self) -> bool {
        matches!(self, Mode::Binary | Mode::Hex)
    }
}

/// Layout computed for the current terminal width.
pub struct Layout {
    /// Bytes per line.
    pub bpl: usize,
    /// How many hex digits the offset column holds.
    pub offset_digits: usize,
    /// The widest line this terminal can hold in this mode — the ceiling the
    /// line-width keys stop at.
    pub fit: usize,
}

/// Smallest and largest line a mode allows, before the terminal width has its
/// say. Four bytes is the narrowest hex dump worth reading; past 64 the eye
/// cannot follow a row across anyway.
pub fn limits(mode: Mode) -> (usize, usize) {
    match mode {
        Mode::Hex => (4, 64),
        Mode::Binary => (1, 16),
        Mode::Text => (1, 512),
    }
}

/// How much one press of the line-width keys moves it. Hex steps in fours so
/// the dump keeps the four-byte grouping that makes it readable.
pub fn step(mode: Mode) -> usize {
    match mode {
        Mode::Hex => 4,
        Mode::Binary => 1,
        Mode::Text => 8,
    }
}

/// Fit a layout to the terminal width.
///
/// `fixed` is a line width the user asked for; without one the line is as wide
/// as the terminal can hold. Either way the mode's own limits and the terminal
/// width get the last word.
///
/// The offset column grows to 12 digits on files larger than 4 GiB — eight
/// digits stop being enough there and offsets start looking truncated.
pub fn layout(mode: Mode, width: u16, file_len: u64, fixed: Option<usize>) -> Layout {
    let offset_digits = if file_len > u32::MAX as u64 { 12 } else { 8 };
    let avail = (width as usize).saturating_sub(offset_digits + 2);
    let (min, max) = limits(mode);

    let fit = match mode {
        // "XX " per byte + separator + one text cell
        Mode::Hex => {
            let raw = avail.saturating_sub(1) / 4;
            // round down to a multiple of 4 — that is what makes a dump
            // readable by eye
            ((raw / 4) * 4).clamp(min, max)
        }
        // "bbbbbbbb " per byte + separator + one text cell
        Mode::Binary => (avail.saturating_sub(1) / 10).clamp(min, max),
        // one cell per byte
        Mode::Text => avail.clamp(min, max),
    };

    Layout {
        bpl: fixed.map_or(fit, |n| n.clamp(min, fit)),
        offset_digits,
        fit,
    }
}

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// Build the offset column, e.g. "0000A840: ".
pub fn format_offset(out: &mut String, offset: u64, digits: usize) {
    out.clear();
    for i in (0..digits).rev() {
        let nibble = (offset >> (i * 4)) & 0xF;
        out.push(HEX_DIGITS[nibble as usize] as char);
    }
    out.push(':');
    out.push(' ');
}

/// Append one byte's representation to the main column.
///
/// `glyph` is the byte's decoded text cell, already produced by the encoding
/// layer; only the text mode uses it.
///
/// Text mode draws a character once and skips its continuation bytes, so
/// Cyrillic in UTF-8 reads as "привет" and not as "п·р·и·в·е·т·". It can do
/// that because there is no byte column beside it to line up with — the text
/// column of the byte modes keeps the continuation marks, where dropping them
/// would put the two columns out of step. The line ends up shorter than its
/// byte count, which is the honest picture: those bytes are one letter.
///
/// Formatting happens byte by byte rather than a whole line at a time because
/// highlighting colours individual bytes, so a line is emitted in runs of
/// different colours.
pub fn push_byte(out: &mut String, mode: Mode, b: u8, glyph: char) {
    match mode {
        Mode::Hex => {
            out.push(HEX_DIGITS[(b >> 4) as usize] as char);
            out.push(HEX_DIGITS[(b & 0x0F) as usize] as char);
            out.push(' ');
        }
        Mode::Binary => {
            for bit in (0..8).rev() {
                out.push(if (b >> bit) & 1 == 1 { '1' } else { '0' });
            }
            out.push(' ');
        }
        Mode::Text => {
            if glyph != crate::encoding::CONTINUATION {
                out.push(glyph);
            }
        }
    }
}

/// Append blank space in place of a byte: the last line of a file is shorter
/// than the rest, but the text column must not shift.
pub fn push_gap(out: &mut String, mode: Mode) {
    for _ in 0..byte_width(mode) {
        out.push(' ');
    }
}

/// How many cells one byte takes in this mode.
pub fn byte_width(mode: Mode) -> usize {
    match mode {
        Mode::Hex => 3,
        Mode::Binary => 9,
        Mode::Text => 1,
    }
}

/// Parse an offset typed by the user.
///
/// Hexadecimal by default: every offset this viewer shows is hex, so `2A0` is
/// the one thing "go to 2A0" can reasonably mean. `0x2A0`, `$2A0` and `2A0h`
/// say the same thing for anyone whose fingers insist. A leading `d` — `d672` —
/// is the way out to decimal.
pub fn parse_offset(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(dec) = s.strip_prefix(|c| c == 'd' || c == 'D') {
        return dec.trim().parse::<u64>().ok();
    }
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .or_else(|| s.strip_prefix('$'))
        .unwrap_or(s);
    let hex = hex.strip_suffix(|c| c == 'h' || c == 'H').unwrap_or(hex);
    u64::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{self, Encoding};

    #[test]
    fn hex_layout_fits_the_width() {
        for w in [40u16, 80, 100, 120, 200] {
            let l = layout(Mode::Hex, w, 1024, None);
            let used = l.offset_digits + 2 + l.bpl * 3 + 1 + l.bpl;
            assert!(used <= w as usize, "w={w} bpl={} used={used}", l.bpl);
        }
    }

    #[test]
    fn binary_layout_fits_the_width() {
        for w in [40u16, 80, 120, 200] {
            let l = layout(Mode::Binary, w, 1024, None);
            let used = l.offset_digits + 2 + l.bpl * 9 + 1 + l.bpl;
            assert!(used <= w as usize, "w={w} bpl={} used={used}", l.bpl);
        }
    }

    #[test]
    fn huge_files_get_wider_offset_column() {
        assert_eq!(layout(Mode::Hex, 120, 1024, None).offset_digits, 8);
        assert_eq!(layout(Mode::Hex, 120, 8 * 1024 * 1024 * 1024, None).offset_digits, 12);
    }

    #[test]
    fn a_chosen_line_width_is_honoured_within_the_limits() {
        let wide = layout(Mode::Hex, 200, 1024, None);
        assert_eq!(wide.bpl, wide.fit);

        // Narrower than the terminal: that is the whole point of the keys.
        assert_eq!(layout(Mode::Hex, 200, 1024, Some(8)).bpl, 8);
        // Wider than the terminal can hold, or narrower than the mode allows:
        // clamped, never left to spill or collapse.
        assert_eq!(layout(Mode::Hex, 200, 1024, Some(999)).bpl, wide.fit);
        assert_eq!(layout(Mode::Hex, 200, 1024, Some(1)).bpl, 4);
        assert_eq!(layout(Mode::Binary, 200, 1024, Some(999)).bpl, layout(Mode::Binary, 200, 1024, None).fit);
    }

    #[test]
    fn a_chosen_width_still_fits_the_window() {
        for w in [40u16, 80, 120, 200] {
            for want in [1usize, 4, 16, 64, 999] {
                let l = layout(Mode::Hex, w, 1024, Some(want));
                let used = l.offset_digits + 2 + l.bpl * 3 + 1 + l.bpl;
                assert!(used <= w as usize, "w={w} want={want} bpl={}", l.bpl);
            }
        }
    }

    /// Build a whole line the same way drawing does.
    fn line(mode: Mode, enc: Encoding, bytes: &[u8], bpl: usize) -> String {
        let mut glyphs = Vec::new();
        encoding::decode(enc, 0, bytes, &mut glyphs);
        let mut s = String::new();
        for i in 0..bpl {
            match bytes.get(i) {
                Some(&b) => push_byte(&mut s, mode, b, glyphs.get(i).copied().unwrap_or(' ')),
                None => push_gap(&mut s, mode),
            }
        }
        s
    }

    #[test]
    fn short_tail_is_padded_so_text_column_stays_aligned() {
        let m = line(Mode::Hex, Encoding::Ascii, &[0xDE, 0xAD], 16);
        assert_eq!(m.chars().count(), 48);
        assert!(m.starts_with("DE AD "));
        assert!(m.ends_with("   "));
    }

    #[test]
    fn binary_mode_spells_out_bits() {
        assert_eq!(line(Mode::Binary, Encoding::Ascii, &[0b1010_0001], 1), "10100001 ");
    }

    #[test]
    fn text_mode_follows_the_selected_encoding() {
        let cyrillic = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(line(Mode::Text, Encoding::Cp1251, &cyrillic, 6), "Привет");
        assert_eq!(line(Mode::Text, Encoding::Ascii, &cyrillic, 6), "......");
        assert_eq!(line(Mode::Text, Encoding::Cp437, &[0x01, 0x03, 0xB2, b'A'], 4), "☺♥▓A");
    }

    #[test]
    fn text_mode_reads_as_text_not_as_letters_with_dots_between() {
        // The complaint this fixes: UTF-8 Cyrillic used to come out as
        // "П·р·и·в·е·т·" because every continuation byte claimed a cell.
        let utf8 = "Привет".as_bytes();
        assert_eq!(line(Mode::Text, Encoding::Utf8, utf8, utf8.len()), "Привет");
        let utf16: Vec<u8> = "Ok".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(line(Mode::Text, Encoding::Utf16Le, &utf16, utf16.len()), "Ok");
    }

    #[test]
    fn the_text_column_of_a_byte_mode_still_marks_continuations() {
        // There it has to: drop a cell and the text stops lining up with the
        // bytes it belongs to. That column is drawn from the glyphs directly.
        let mut glyphs = Vec::new();
        encoding::decode(Encoding::Utf8, 0, "Привет".as_bytes(), &mut glyphs);
        let column: String = glyphs.iter().collect();
        assert_eq!(column, "П·р·и·в·е·т·");
        assert_eq!(column.chars().count(), "Привет".len());
    }

    #[test]
    fn gap_width_matches_byte_width_in_every_mode() {
        for mode in [Mode::Hex, Mode::Binary, Mode::Text] {
            let mut a = String::new();
            push_byte(&mut a, mode, 0x41, 'A');
            let mut b = String::new();
            push_gap(&mut b, mode);
            assert_eq!(a.chars().count(), b.chars().count());
        }
    }

    #[test]
    fn offsets_are_zero_padded() {
        let mut s = String::new();
        format_offset(&mut s, 0xA840, 8);
        assert_eq!(s, "0000A840: ");
        format_offset(&mut s, 0x1_0000_0000, 12);
        assert_eq!(s, "000100000000: ");
    }

    #[test]
    fn a_bare_offset_is_hex_because_every_offset_on_screen_is() {
        assert_eq!(parse_offset("2A0"), Some(0x2A0));
        assert_eq!(parse_offset("2a0"), Some(0x2A0));
        assert_eq!(parse_offset("0x4D5A"), Some(0x4D5A));
        assert_eq!(parse_offset("$FF"), Some(0xFF));
        assert_eq!(parse_offset("ffh"), Some(0xFF));
        assert_eq!(parse_offset(" 1000 "), Some(0x1000));
    }

    #[test]
    fn decimal_needs_saying_so() {
        assert_eq!(parse_offset("d672"), Some(672));
        assert_eq!(parse_offset("D672"), Some(672));
        // 672 on its own is hex, and means something else entirely.
        assert_eq!(parse_offset("672"), Some(0x672));
    }

    #[test]
    fn nonsense_is_rejected() {
        assert_eq!(parse_offset("nonsense"), None);
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_offset("d"), None);
        assert_eq!(parse_offset("0x"), None);
        assert_eq!(parse_offset("2A0zz"), None);
    }
}
