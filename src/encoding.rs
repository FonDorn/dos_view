//! Byte-to-glyph decoding: N bytes always become exactly N screen cells.
//!
//! One byte = one cell is the invariant the whole layout rests on. The text
//! column has to line up with the byte column, and every cell has to map back
//! to a file offset. Single-byte code pages satisfy that for free; UTF-8 does
//! not, so a multi-byte character is drawn on the cell of its leading byte and
//! its continuation bytes get `·`. The text stays readable and the columns
//! stay aligned.
//!
//! The tables are hardcoded instead of pulled from a crate: it is 128 chars
//! per code page, and in exchange there is full control over what shows up in
//! place of NUL, NBSP and undefined bytes.

/// Which code page the text cells are decoded with.
///
/// Applies both to the text column of the byte modes and to the full-screen
/// text mode, so a hex dump of a Windows-1251 file reads as Cyrillic instead
/// of dots.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Encoding {
    /// Strict ASCII: printable as-is, everything else a dot.
    Ascii,
    /// Code page 437, the original IBM PC palette: bytes 0x01..0x1F show up as
    /// ☺☻♥♦♣♠ and the upper half as box drawing.
    Cp437,
    /// Code page 866 — DOS Cyrillic, what Far shows by default on Russian text.
    Cp866,
    /// Windows-1251 — Cyrillic on Windows.
    Cp1251,
    /// KOI8-R — Cyrillic in older Unix files and mail.
    Koi8r,
    /// UTF-8, decoded per character; continuation bytes get `·`.
    Utf8,
}

impl Encoding {
    /// Cycling order for the encoding key.
    pub const ALL: [Encoding; 6] = [
        Encoding::Ascii,
        Encoding::Cp437,
        Encoding::Cp866,
        Encoding::Cp1251,
        Encoding::Koi8r,
        Encoding::Utf8,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Encoding::Ascii => "ASCII",
            Encoding::Cp437 => "CP437",
            Encoding::Cp866 => "CP866",
            Encoding::Cp1251 => "CP1251",
            Encoding::Koi8r => "KOI8-R",
            Encoding::Utf8 => "UTF-8",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&e| e == self).unwrap_or(0)
    }

    pub fn next(self) -> Encoding {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Encoding {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Glyph drawn on the cells of a UTF-8 continuation byte.
pub const CONTINUATION: char = '·';

/// Decode `bytes` into `out`, one glyph per byte.
///
/// `out` is cleared first and always ends up the same length as `bytes`, which
/// is what lets the caller index cells by byte offset.
pub fn decode(enc: Encoding, bytes: &[u8], out: &mut Vec<char>) {
    out.clear();
    out.reserve(bytes.len());
    match enc {
        // CP437 is the one table that covers all 256 bytes on purpose: showing
        // a glyph for every control byte is the whole point of the DOS view.
        Encoding::Cp437 => out.extend(bytes.iter().map(|&b| CP437[b as usize])),
        Encoding::Ascii => out.extend(bytes.iter().map(|&b| ascii_cell(b))),
        Encoding::Utf8 => decode_utf8(bytes, out),
        _ => {
            let high = high_table(enc).expect("single-byte encoding has a table");
            out.extend(bytes.iter().map(|&b| match b {
                0x00..=0x7F => ascii_cell(b),
                _ => one_cell_or_dot(high[b as usize - 0x80]),
            }));
        }
    }
}

/// How many extra bytes past the end of a line are worth reading so that a
/// character straddling the line break can still be decoded.
pub const LOOKAHEAD: usize = 3;

fn decode_utf8(bytes: &[u8], out: &mut Vec<char>) {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(ascii_cell(b));
            i += 1;
            continue;
        }
        let width = utf8_width(b);
        // An invalid lead byte, a stray continuation byte, or a sequence cut
        // off by the end of the slice: show the byte as a dot and resync on
        // the next one rather than guessing.
        if width == 0 || i + width > bytes.len() {
            out.push('.');
            i += 1;
            continue;
        }
        match std::str::from_utf8(&bytes[i..i + width]) {
            Ok(s) => {
                let c = s.chars().next().unwrap_or('.');
                out.push(one_cell_or_dot(c));
                for _ in 1..width {
                    out.push(CONTINUATION);
                }
                i += width;
            }
            Err(_) => {
                out.push('.');
                i += 1;
            }
        }
    }
}

/// Length of the UTF-8 sequence this byte starts, or 0 if it starts none.
fn utf8_width(lead: u8) -> usize {
    match lead {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        // 0xC0/0xC1 only ever appear in overlong forms, 0x80..0xBF are
        // continuation bytes, 0xF5.. is past the last code point.
        _ => 0,
    }
}

#[inline]
fn ascii_cell(b: u8) -> char {
    if (0x20..0x7F).contains(&b) {
        b as char
    } else {
        '.'
    }
}

#[inline]
fn one_cell_or_dot(c: char) -> char {
    if one_cell(c) {
        c
    } else {
        '.'
    }
}

/// Can this character be drawn in exactly one terminal cell?
///
/// No `unicode-width` dependency: the viewer only needs to protect the column
/// grid, and the ranges below are what actually breaks it — controls,
/// combining marks, zero-width characters and the East Asian double-width
/// blocks. Whatever is rejected falls back to a dot, which is honest: the byte
/// is there, we just do not pretend to draw it.
fn one_cell(c: char) -> bool {
    if c.is_control() || c == '\0' {
        return false;
    }
    let u = c as u32;
    if u == 0x00AD || u == 0x200B || u == 0xFEFF {
        return false; // soft hyphen, zero-width space, BOM
    }
    !matches!(u,
        0x0300..=0x036F        // combining marks
        | 0x1100..=0x115F      // Hangul Jamo
        | 0x2E80..=0x303E      // CJK radicals, Kangxi, CJK punctuation
        | 0x3041..=0x33FF      // kana, CJK compatibility
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF      // CJK unified ideographs
        | 0xA000..=0xA4CF      // Yi
        | 0xAC00..=0xD7A3      // Hangul syllables
        | 0xF900..=0xFAFF      // CJK compatibility ideographs
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60      // fullwidth forms
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF    // emoji
        | 0x20000..=0x3FFFD    // CJK extensions
    )
}

/// The 0x80..0xFF half of a single-byte code page, or `None` for the
/// encodings that do not have one.
fn high_table(enc: Encoding) -> Option<&'static [char]> {
    match enc {
        Encoding::Cp437 => Some(&CP437[0x80..]),
        Encoding::Cp866 => Some(&CP866_HIGH),
        Encoding::Cp1251 => Some(&CP1251_HIGH),
        Encoding::Koi8r => Some(&KOI8R_HIGH),
        Encoding::Ascii | Encoding::Utf8 => None,
    }
}

/// Encode one character the way the file would hold it, or `None` if this
/// encoding cannot represent it.
pub fn encode_char(enc: Encoding, c: char) -> Option<u8> {
    // Every encoding here agrees with ASCII below 0x80, so the low half needs
    // no table lookup — and looking it up would be wrong for CP437, whose
    // table maps control bytes to glyphs.
    if (c as u32) < 0x80 {
        return Some(c as u8);
    }
    let high = high_table(enc)?;
    high.iter().position(|&t| t == c).map(|i| (i + 0x80) as u8)
}

/// Encode a search string into file bytes. On failure returns the character
/// that does not fit the current encoding, so the caller can name it.
pub fn encode_text(enc: Encoding, s: &str) -> Result<Vec<u8>, char> {
    // UTF-8 is also the fallback for the ASCII view: ASCII cannot express
    // anything above 0x7F, and text typed while looking at an ASCII dump is
    // almost always UTF-8 in the file.
    if matches!(enc, Encoding::Ascii | Encoding::Utf8) {
        return Ok(s.as_bytes().to_vec());
    }
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        out.push(encode_char(enc, c).ok_or(c)?);
    }
    Ok(out)
}

/// Code page 437, all 256 bytes. NUL and NBSP show as a space, like in Far.
pub const CP437: [char; 256] = [
    // 0x00..0x0F
    ' ', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼',
    // 0x10..0x1F
    '►', '◄', '↕', '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
    // 0x20..0x2F
    ' ', '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    // 0x30..0x3F
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    // 0x40..0x4F
    '@', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    // 0x50..0x5F
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '[', '\\', ']', '^', '_',
    // 0x60..0x6F
    '`', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    // 0x70..0x7F
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '{', '|', '}', '~', '⌂',
    // 0x80..0x8F
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    // 0x90..0x9F
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    // 0xA0..0xAF
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    // 0xB0..0xBF
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    // 0xC0..0xCF
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    // 0xD0..0xDF
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    // 0xE0..0xEF
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    // 0xF0..0xFF
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', ' ',
];

/// Code page 866, upper half. DOS Cyrillic: the alphabet sits in two runs with
/// the CP437 box drawing left untouched in between, which is why DOS programs
/// kept their frames when they switched to Russian.
const CP866_HIGH: [char; 128] = [
    // 0x80..0x8F
    'А', 'Б', 'В', 'Г', 'Д', 'Е', 'Ж', 'З', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О', 'П',
    // 0x90..0x9F
    'Р', 'С', 'Т', 'У', 'Ф', 'Х', 'Ц', 'Ч', 'Ш', 'Щ', 'Ъ', 'Ы', 'Ь', 'Э', 'Ю', 'Я',
    // 0xA0..0xAF
    'а', 'б', 'в', 'г', 'д', 'е', 'ж', 'з', 'и', 'й', 'к', 'л', 'м', 'н', 'о', 'п',
    // 0xB0..0xBF
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    // 0xC0..0xCF
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    // 0xD0..0xDF
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    // 0xE0..0xEF
    'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ъ', 'ы', 'ь', 'э', 'ю', 'я',
    // 0xF0..0xFF
    'Ё', 'ё', 'Є', 'є', 'Ї', 'ї', 'Ў', 'ў', '°', '∙', '·', '√', '№', '¤', '■', '\u{A0}',
];

/// Windows-1251, upper half. Unlike CP866 the alphabet is one contiguous run
/// from 0xC0, so Cyrillic text in it looks like a solid block of high bytes.
const CP1251_HIGH: [char; 128] = [
    // 0x80..0x8F
    'Ђ', 'Ѓ', '‚', 'ѓ', '„', '…', '†', '‡', '€', '‰', 'Љ', '‹', 'Њ', 'Ќ', 'Ћ', 'Џ',
    // 0x90..0x9F — 0x98 is unassigned; '\0' marks it and renders as a dot.
    'ђ', '‘', '’', '“', '”', '•', '–', '—', '\0', '™', 'љ', '›', 'њ', 'ќ', 'ћ', 'џ',
    // 0xA0..0xAF
    '\u{A0}', 'Ў', 'ў', 'Ј', '¤', 'Ґ', '¦', '§', 'Ё', '©', 'Є', '«', '¬', '\u{AD}', '®', 'Ї',
    // 0xB0..0xBF
    '°', '±', 'І', 'і', 'ґ', 'µ', '¶', '·', 'ё', '№', 'є', '»', 'ј', 'Ѕ', 'ѕ', 'ї',
    // 0xC0..0xCF
    'А', 'Б', 'В', 'Г', 'Д', 'Е', 'Ж', 'З', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О', 'П',
    // 0xD0..0xDF
    'Р', 'С', 'Т', 'У', 'Ф', 'Х', 'Ц', 'Ч', 'Ш', 'Щ', 'Ъ', 'Ы', 'Ь', 'Э', 'Ю', 'Я',
    // 0xE0..0xEF
    'а', 'б', 'в', 'г', 'д', 'е', 'ж', 'з', 'и', 'й', 'к', 'л', 'м', 'н', 'о', 'п',
    // 0xF0..0xFF
    'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ъ', 'ы', 'ь', 'э', 'ю', 'я',
];

/// KOI8-R, upper half. The alphabet is ordered so that dropping the high bit
/// leaves readable Latin transliteration — that is why the letters look
/// scrambled in table order.
const KOI8R_HIGH: [char; 128] = [
    // 0x80..0x8F
    '─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '▀', '▄', '█', '▌', '▐',
    // 0x90..0x9F
    '░', '▒', '▓', '⌠', '■', '∙', '√', '≈', '≤', '≥', '\u{A0}', '⌡', '°', '²', '·', '÷',
    // 0xA0..0xAF
    '═', '║', '╒', 'ё', '╓', '╔', '╕', '╖', '╗', '╘', '╙', '╚', '╛', '╜', '╝', '╞',
    // 0xB0..0xBF
    '╟', '╠', '╡', 'Ё', '╢', '╣', '╤', '╥', '╦', '╧', '╨', '╩', '╪', '╫', '╬', '©',
    // 0xC0..0xCF
    'ю', 'а', 'б', 'ц', 'д', 'е', 'ф', 'г', 'х', 'и', 'й', 'к', 'л', 'м', 'н', 'о',
    // 0xD0..0xDF
    'п', 'я', 'р', 'с', 'т', 'у', 'ж', 'в', 'ь', 'ы', 'з', 'ш', 'э', 'щ', 'ч', 'ъ',
    // 0xE0..0xEF
    'Ю', 'А', 'Б', 'Ц', 'Д', 'Е', 'Ф', 'Г', 'Х', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О',
    // 0xF0..0xFF
    'П', 'Я', 'Р', 'С', 'Т', 'У', 'Ж', 'В', 'Ь', 'Ы', 'З', 'Ш', 'Э', 'Щ', 'Ч', 'Ъ',
];

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(enc: Encoding, bytes: &[u8]) -> String {
        let mut out = Vec::new();
        decode(enc, bytes, &mut out);
        assert_eq!(out.len(), bytes.len(), "one cell per byte");
        out.into_iter().collect()
    }

    #[test]
    fn every_encoding_yields_one_cell_per_byte() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        for enc in Encoding::ALL {
            let s = cells(enc, &bytes);
            assert_eq!(s.chars().count(), 256, "{}", enc.name());
        }
    }

    #[test]
    fn ascii_hides_control_bytes() {
        assert_eq!(cells(Encoding::Ascii, b"\x00A\x1F~\x7F"), ".A.~.");
    }

    #[test]
    fn cp437_shows_the_dos_glyphs() {
        assert_eq!(cells(Encoding::Cp437, &[0x01, 0x03, 0xB2, b'A']), "☺♥▓A");
    }

    #[test]
    fn dos_cyrillic_decodes() {
        // "Привет" in CP866.
        assert_eq!(cells(Encoding::Cp866, &[0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2]), "Привет");
    }

    #[test]
    fn windows_cyrillic_decodes() {
        assert_eq!(cells(Encoding::Cp1251, &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]), "Привет");
    }

    #[test]
    fn koi8r_decodes() {
        assert_eq!(cells(Encoding::Koi8r, &[0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4]), "Привет");
    }

    #[test]
    fn unassigned_bytes_become_dots() {
        assert_eq!(cells(Encoding::Cp1251, &[0x98]), ".");
    }

    #[test]
    fn utf8_draws_a_char_on_its_lead_byte_and_dots_the_rest() {
        // Each Cyrillic letter is two bytes, so the text is twice as wide as
        // it reads — that is the price of keeping cells aligned with offsets.
        assert_eq!(cells(Encoding::Utf8, "Привет".as_bytes()), "П·р·и·в·е·т·");
        assert_eq!(cells(Encoding::Utf8, b"ok"), "ok");
    }

    #[test]
    fn utf8_resyncs_after_garbage() {
        // Stray continuation byte, then a valid letter.
        assert_eq!(cells(Encoding::Utf8, &[0xB0, 0xD0, 0x9F]), ".П·");
        // Sequence cut off by the end of the slice.
        assert_eq!(cells(Encoding::Utf8, &[0xD0]), ".");
    }

    #[test]
    fn utf8_rejects_overlong_and_out_of_range_leads() {
        assert_eq!(cells(Encoding::Utf8, &[0xC0, 0xAF]), "..");
        assert_eq!(cells(Encoding::Utf8, &[0xF5, 0x80, 0x80, 0x80]), "....");
    }

    #[test]
    fn wide_characters_are_not_drawn_since_they_would_shift_the_grid() {
        // A CJK ideograph is three bytes but two columns wide, so the letter
        // itself is refused; the continuation cells still mark the sequence.
        assert_eq!(cells(Encoding::Utf8, "漢".as_bytes()), ".··");
    }

    #[test]
    fn search_text_round_trips_through_every_code_page() {
        for enc in [Encoding::Cp866, Encoding::Cp1251, Encoding::Koi8r] {
            let bytes = encode_text(enc, "Привет, мир").unwrap();
            assert_eq!(bytes.len(), 11, "{}", enc.name());
            assert_eq!(cells(enc, &bytes), "Привет, мир", "{}", enc.name());
        }
    }

    #[test]
    fn search_text_in_ascii_or_utf8_view_is_utf8_bytes() {
        assert_eq!(encode_text(Encoding::Utf8, "П").unwrap(), vec![0xD0, 0x9F]);
        assert_eq!(encode_text(Encoding::Ascii, "П").unwrap(), vec![0xD0, 0x9F]);
    }

    #[test]
    fn encoding_reports_the_char_it_cannot_represent() {
        assert_eq!(encode_text(Encoding::Cp437, "Привет"), Err('П'));
        assert_eq!(encode_text(Encoding::Cp866, "漢"), Err('漢'));
    }

    #[test]
    fn ascii_encodes_the_same_way_in_every_code_page() {
        for enc in Encoding::ALL {
            assert_eq!(encode_text(enc, "REC.").unwrap(), b"REC.".to_vec(), "{}", enc.name());
        }
    }

    #[test]
    fn cycling_encodings_wraps_both_ways() {
        assert_eq!(Encoding::Ascii.prev().name(), "UTF-8");
        assert_eq!(Encoding::Utf8.next().name(), "ASCII");
        let mut e = Encoding::Ascii;
        for _ in 0..Encoding::ALL.len() {
            e = e.next();
        }
        assert!(e == Encoding::Ascii);
    }
}
