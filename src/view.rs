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
}

/// Fit a layout to the terminal width.
///
/// The offset column grows to 12 digits on files larger than 4 GiB — eight
/// digits stop being enough there and offsets start looking truncated.
pub fn layout(mode: Mode, width: u16, file_len: u64) -> Layout {
    let offset_digits = if file_len > u32::MAX as u64 { 12 } else { 8 };
    let ow = offset_digits + 2;
    let avail = (width as usize).saturating_sub(ow);

    let bpl = match mode {
        // "XX " per byte + separator + one text cell
        Mode::Hex => {
            let raw = avail.saturating_sub(1) / 4;
            // round down to a multiple of 4 — that is what makes a dump
            // readable by eye
            let n = (raw / 4) * 4;
            n.clamp(4, 64)
        }
        // "bbbbbbbb " per byte + separator + one text cell
        Mode::Binary => {
            let raw = avail.saturating_sub(1) / 10;
            raw.clamp(1, 16)
        }
        // one cell per byte
        Mode::Text => avail.clamp(1, 512),
    };

    Layout { bpl, offset_digits }
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
        Mode::Text => out.push(glyph),
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

/// Parse an offset typed by the user: `1000` is decimal, `0x1000` and `$1000`
/// are hexadecimal.
pub fn parse_offset(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = s.strip_prefix('$') {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{self, Encoding};

    #[test]
    fn hex_layout_fits_the_width() {
        for w in [40u16, 80, 100, 120, 200] {
            let l = layout(Mode::Hex, w, 1024);
            let used = l.offset_digits + 2 + l.bpl * 3 + 1 + l.bpl;
            assert!(used <= w as usize, "w={w} bpl={} used={used}", l.bpl);
        }
    }

    #[test]
    fn binary_layout_fits_the_width() {
        for w in [40u16, 80, 120, 200] {
            let l = layout(Mode::Binary, w, 1024);
            let used = l.offset_digits + 2 + l.bpl * 9 + 1 + l.bpl;
            assert!(used <= w as usize, "w={w} bpl={} used={used}", l.bpl);
        }
    }

    #[test]
    fn huge_files_get_wider_offset_column() {
        assert_eq!(layout(Mode::Hex, 120, 1024).offset_digits, 8);
        assert_eq!(layout(Mode::Hex, 120, 8 * 1024 * 1024 * 1024).offset_digits, 12);
    }

    /// Build a whole line the same way drawing does.
    fn line(mode: Mode, enc: Encoding, bytes: &[u8], bpl: usize) -> String {
        let mut glyphs = Vec::new();
        encoding::decode(enc, bytes, &mut glyphs);
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
    fn offset_input_accepts_dec_and_hex() {
        assert_eq!(parse_offset("1024"), Some(1024));
        assert_eq!(parse_offset("0x4D5A"), Some(0x4D5A));
        assert_eq!(parse_offset("$FF"), Some(255));
        assert_eq!(parse_offset("nonsense"), None);
    }
}
