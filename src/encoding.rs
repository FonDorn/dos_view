//! Byte-to-glyph decoding: N bytes always become exactly N screen cells.
//!
//! One byte = one cell is the invariant the whole layout rests on. The text
//! column has to line up with the byte column, and every cell has to map back
//! to a file offset. Single-byte code pages satisfy that for free; UTF-8 and
//! UTF-16 do not, so a multi-byte character is drawn on the cell of its
//! leading byte and its continuation bytes get `·`.
//!
//! The tables are generated from the reference mappings rather than pulled
//! from a crate: it is 128 chars per code page, and in exchange there is full
//! control over what shows up in place of NUL, NBSP and undefined bytes.

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
    /// DOS Western Europe.
    Cp850,
    /// DOS Central Europe.
    Cp852,
    /// DOS Cyrillic — what Far shows by default on Russian text.
    Cp866,
    /// Windows Central Europe.
    Cp1250,
    /// Windows Cyrillic.
    Cp1251,
    /// Windows Western Europe.
    Cp1252,
    /// Latin-1.
    Iso8859_1,
    /// Latin-2, Central Europe.
    Iso8859_2,
    /// ISO Cyrillic.
    Iso8859_5,
    /// Latin-9: Latin-1 with the euro sign.
    Iso8859_15,
    /// Cyrillic in older Unix files and mail.
    Koi8R,
    /// The Ukrainian variant of KOI8.
    Koi8U,
    /// Classic Mac OS Western.
    MacRoman,
    /// Classic Mac OS Cyrillic.
    MacCyrillic,
    /// UTF-8, decoded per character; continuation bytes get `·`.
    Utf8,
    /// UTF-16, little endian — Windows text files and PE resources.
    Utf16Le,
    /// UTF-16, big endian.
    Utf16Be,
}

impl Encoding {
    /// Cycling order for the code page key: grouped by family, so stepping
    /// through it walks DOS, then Windows, then ISO, then KOI8, then Mac, then
    /// Unicode.
    pub const ALL: [Encoding; 19] = [
        Encoding::Ascii,
        Encoding::Cp437,
        Encoding::Cp850,
        Encoding::Cp852,
        Encoding::Cp866,
        Encoding::Cp1250,
        Encoding::Cp1251,
        Encoding::Cp1252,
        Encoding::Iso8859_1,
        Encoding::Iso8859_2,
        Encoding::Iso8859_5,
        Encoding::Iso8859_15,
        Encoding::Koi8R,
        Encoding::Koi8U,
        Encoding::MacRoman,
        Encoding::MacCyrillic,
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
    ];

    /// Width of the longest name. The status bar pads to this so switching
    /// code pages does not shuffle everything next to it.
    pub const NAME_WIDTH: usize = 11;

    pub fn name(self) -> &'static str {
        match self {
            Encoding::Ascii => "ASCII",
            Encoding::Cp437 => "CP437",
            Encoding::Cp850 => "CP850",
            Encoding::Cp852 => "CP852",
            Encoding::Cp866 => "CP866",
            Encoding::Cp1250 => "CP1250",
            Encoding::Cp1251 => "CP1251",
            Encoding::Cp1252 => "CP1252",
            Encoding::Iso8859_1 => "ISO-8859-1",
            Encoding::Iso8859_2 => "ISO-8859-2",
            Encoding::Iso8859_5 => "ISO-8859-5",
            Encoding::Iso8859_15 => "ISO-8859-15",
            Encoding::Koi8R => "KOI8-R",
            Encoding::Koi8U => "KOI8-U",
            Encoding::MacRoman => "MacRoman",
            Encoding::MacCyrillic => "MacCyrillic",
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::Utf16Be => "UTF-16BE",
        }
    }

    /// Where it sits in `ALL` — the picker needs it to open on the current page.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&e| e == self).unwrap_or(0)
    }

    pub fn next(self) -> Encoding {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Encoding {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Glyph drawn on the cells of a continuation byte.
pub const CONTINUATION: char = '·';

/// How many extra bytes past the end of a line are worth reading so that a
/// character straddling the line break can still be decoded. The longest
/// sequence either Unicode encoding produces is four bytes.
pub const LOOKAHEAD: usize = 3;

/// Decode `bytes` into `out`, one glyph per byte.
///
/// `start` is the file offset `bytes[0]` came from. The single-byte pages and
/// UTF-8 do not care, but UTF-16 has to know whether it is looking at the low
/// or the high byte of a code unit, and that is decided by the offset's parity
/// in the file — not by wherever the caller happened to start reading.
///
/// `out` is cleared first and always ends up the same length as `bytes`, which
/// is what lets the caller index cells by byte offset.
pub fn decode(enc: Encoding, start: u64, bytes: &[u8], out: &mut Vec<char>) {
    out.clear();
    out.reserve(bytes.len());
    match enc {
        // CP437 is the one table that covers all 256 bytes on purpose: showing
        // a glyph for every control byte is the whole point of the DOS view.
        Encoding::Cp437 => out.extend(bytes.iter().map(|&b| CP437[b as usize])),
        Encoding::Ascii => out.extend(bytes.iter().map(|&b| ascii_cell(b))),
        Encoding::Utf8 => decode_utf8(bytes, out),
        Encoding::Utf16Le => decode_utf16(start, bytes, out, true),
        Encoding::Utf16Be => decode_utf16(start, bytes, out, false),
        _ => {
            let high = high_table(enc).expect("single-byte encoding has a table");
            out.extend(bytes.iter().map(|&b| match b {
                0x00..=0x7F => ascii_cell(b),
                _ => one_cell_or_dot(high[b as usize - 0x80]),
            }));
        }
    }
}

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

fn decode_utf16(start: u64, bytes: &[u8], out: &mut Vec<char>, little: bool) {
    let unit = |i: usize| -> u32 {
        let (lo, hi) = (bytes[i] as u32, bytes[i + 1] as u32);
        if little {
            lo | (hi << 8)
        } else {
            (lo << 8) | hi
        }
    };

    let mut i = 0;
    // A slice that begins on an odd file offset begins in the middle of a code
    // unit. Mark that byte for what it is and carry on in step with the file.
    if start % 2 == 1 && !bytes.is_empty() {
        out.push(CONTINUATION);
        i = 1;
    }

    while i + 1 < bytes.len() {
        let u = unit(i);
        // A character outside the BMP is written as two units, and losing
        // track of the pair would turn a perfectly good emoji into four dots.
        if (0xD800..0xDC00).contains(&u) && i + 3 < bytes.len() {
            let low = unit(i + 2);
            if (0xDC00..0xE000).contains(&low) {
                let c = 0x10000 + ((u - 0xD800) << 10) + (low - 0xDC00);
                out.push(char::from_u32(c).map(one_cell_or_dot).unwrap_or('.'));
                for _ in 0..3 {
                    out.push(CONTINUATION);
                }
                i += 4;
                continue;
            }
        }
        // An unpaired surrogate is not a character; say so rather than draw
        // something that is not there.
        let cell = if (0xD800..0xE000).contains(&u) {
            '.'
        } else {
            char::from_u32(u).map(one_cell_or_dot).unwrap_or('.')
        };
        out.push(cell);
        out.push(CONTINUATION);
        i += 2;
    }

    // A lone byte at the end: half a code unit, nothing to draw yet.
    if i < bytes.len() {
        out.push('.');
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
/// A whitelist, and deliberately a narrow one. It was a blacklist of the wide
/// and zero-width blocks first, which is the natural way round to think about
/// it and quietly wrong: nine and a half thousand code points in the BMP alone
/// are not one column wide, and the list of blocks they live in is nothing
/// anyone will get right by hand. Reading arbitrary bytes as UTF-16 turns up
/// such a character every few rows — a combining mark, a bidi control, an
/// emoji — and one of them is enough to make a row that the terminal draws
/// narrower than the viewer padded it, so the background stops short of the
/// edge and the columns stop lining up.
///
/// So: the ranges below are letters, punctuation and symbols that are one
/// column, left to right, and combine with nothing. Between them they cover
/// every glyph our own code pages can produce (there is a test for that) plus
/// Latin, Greek and Cyrillic text in any Unicode encoding. Everything else —
/// CJK, Arabic, Hebrew, emoji, marks, invisibles — is drawn as `.`, which for
/// a byte viewer is the honest answer: the bytes are there in the dump, and
/// the grid they sit in survives.
fn one_cell(c: char) -> bool {
    matches!(c as u32,
        0x0020..=0x007E                     // ASCII printable
        | 0x00A0..=0x00AC | 0x00AE..=0x00FF // Latin-1, less the soft hyphen
        | 0x0100..=0x024F                   // Latin Extended-A and -B
        | 0x02C6..=0x02DD                   // the spacing diacritics the Windows pages use
        | 0x0370..=0x03FF                   // Greek
        | 0x0400..=0x0482 | 0x048A..=0x052F // Cyrillic, less its combining marks
        | 0x2010..=0x2027                   // dashes, quotes, bullet, ellipsis
        | 0x2030..=0x205E                   // per mille, ‼, angle quotes, fractions
        | 0x2070..=0x209C                   // superscripts and subscripts
        | 0x20A0..=0x20BF                   // currency signs
        | 0x2100..=0x218F                   // letterlike forms and numerals
        | 0x2190..=0x22FF                   // arrows and mathematics
        | 0x2300..=0x2319 | 0x231C..=0x2321 // ⌂ ⌐ ⌠ ⌡, around the watch and hourglass
        | 0x2500..=0x259F                   // box drawing and blocks
        | 0x25A0..=0x25F7                   // geometric shapes
        | 0x263A..=0x263C                   // ☺ ☻ ☼
        | 0x2640 | 0x2642                   // ♀ ♂
        | 0x2660..=0x266F                   // card suits and notes
        | 0xFB00..=0xFB06                   // ﬁ ﬂ and the rest of the ligatures
    )
}

/// The 0x80..0xFF half of a single-byte code page, or `None` for the
/// encodings that do not have one.
fn high_table(enc: Encoding) -> Option<&'static [char]> {
    Some(match enc {
        Encoding::Cp437 => &CP437[0x80..],
        Encoding::Cp850 => &CP850_HIGH,
        Encoding::Cp852 => &CP852_HIGH,
        Encoding::Cp866 => &CP866_HIGH,
        Encoding::Cp1250 => &CP1250_HIGH,
        Encoding::Cp1251 => &CP1251_HIGH,
        Encoding::Cp1252 => &CP1252_HIGH,
        Encoding::Iso8859_1 => &ISO8859_1_HIGH,
        Encoding::Iso8859_2 => &ISO8859_2_HIGH,
        Encoding::Iso8859_5 => &ISO8859_5_HIGH,
        Encoding::Iso8859_15 => &ISO8859_15_HIGH,
        Encoding::Koi8R => &KOI8R_HIGH,
        Encoding::Koi8U => &KOI8U_HIGH,
        Encoding::MacRoman => &MACROMAN_HIGH,
        Encoding::MacCyrillic => &MACCYRILLIC_HIGH,
        Encoding::Ascii | Encoding::Utf8 | Encoding::Utf16Le | Encoding::Utf16Be => return None,
    })
}

/// Encode one character the way the file would hold it, or `None` if this
/// encoding cannot represent it.
pub fn encode_char(enc: Encoding, c: char) -> Option<u8> {
    // Every single-byte encoding here agrees with ASCII below 0x80, so the low
    // half needs no table lookup — and looking it up would be wrong for CP437,
    // whose table maps control bytes to glyphs.
    if (c as u32) < 0x80 {
        return Some(c as u8);
    }
    let high = high_table(enc)?;
    high.iter().position(|&t| t == c).map(|i| (i + 0x80) as u8)
}

/// Encode a search string into file bytes. On failure returns the character
/// that does not fit the current encoding, so the caller can name it.
pub fn encode_text(enc: Encoding, s: &str) -> Result<Vec<u8>, char> {
    match enc {
        // UTF-8 is also the fallback for the ASCII view: ASCII cannot express
        // anything above 0x7F, and text typed while looking at an ASCII dump is
        // almost always UTF-8 in the file.
        Encoding::Ascii | Encoding::Utf8 => Ok(s.as_bytes().to_vec()),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let little = enc == Encoding::Utf16Le;
            let mut out = Vec::with_capacity(s.len() * 2);
            for u in s.encode_utf16() {
                let [lo, hi] = [(u & 0xFF) as u8, (u >> 8) as u8];
                if little {
                    out.extend_from_slice(&[lo, hi]);
                } else {
                    out.extend_from_slice(&[hi, lo]);
                }
            }
            Ok(out)
        }
        _ => {
            let mut out = Vec::with_capacity(s.len());
            for c in s.chars() {
                out.push(encode_char(enc, c).ok_or(c)?);
            }
            Ok(out)
        }
    }
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


/// Code page 850, upper half. DOS Western Europe: CP437 with the
/// accented letters Western languages actually needed in place of some of the
/// box drawing.
const CP850_HIGH: [char; 128] = [
    // 0x80..0x8F
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    // 0x90..0x9F
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', 'ø', '£', 'Ø', '×', 'ƒ',
    // 0xA0..0xAF
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '®', '¬', '½', '¼', '¡', '«', '»',
    // 0xB0..0xBF
    '░', '▒', '▓', '│', '┤', 'Á', 'Â', 'À', '©', '╣', '║', '╗', '╝', '¢', '¥', '┐',
    // 0xC0..0xCF
    '└', '┴', '┬', '├', '─', '┼', 'ã', 'Ã', '╚', '╔', '╩', '╦', '╠', '═', '╬', '¤',
    // 0xD0..0xDF
    'ð', 'Ð', 'Ê', 'Ë', 'È', 'ı', 'Í', 'Î', 'Ï', '┘', '┌', '█', '▄', '¦', 'Ì', '▀',
    // 0xE0..0xEF
    'Ó', 'ß', 'Ô', 'Ò', 'õ', 'Õ', 'µ', 'þ', 'Þ', 'Ú', 'Û', 'Ù', 'ý', 'Ý', '¯', '´',
    // 0xF0..0xFF
    '\u{AD}', '±', '‗', '¾', '¶', '§', '÷', '¸', '°', '¨', '·', '¹', '³', '²', '■', '\u{A0}',
];

/// Code page 852, upper half. DOS Central Europe — Polish, Czech,
/// Hungarian and the rest of the Latin-2 world.
const CP852_HIGH: [char; 128] = [
    // 0x80..0x8F
    'Ç', 'ü', 'é', 'â', 'ä', 'ů', 'ć', 'ç', 'ł', 'ë', 'Ő', 'ő', 'î', 'Ź', 'Ä', 'Ć',
    // 0x90..0x9F
    'É', 'Ĺ', 'ĺ', 'ô', 'ö', 'Ľ', 'ľ', 'Ś', 'ś', 'Ö', 'Ü', 'Ť', 'ť', 'Ł', '×', 'č',
    // 0xA0..0xAF
    'á', 'í', 'ó', 'ú', 'Ą', 'ą', 'Ž', 'ž', 'Ę', 'ę', '¬', 'ź', 'Č', 'ş', '«', '»',
    // 0xB0..0xBF
    '░', '▒', '▓', '│', '┤', 'Á', 'Â', 'Ě', 'Ş', '╣', '║', '╗', '╝', 'Ż', 'ż', '┐',
    // 0xC0..0xCF
    '└', '┴', '┬', '├', '─', '┼', 'Ă', 'ă', '╚', '╔', '╩', '╦', '╠', '═', '╬', '¤',
    // 0xD0..0xDF
    'đ', 'Đ', 'Ď', 'Ë', 'ď', 'Ň', 'Í', 'Î', 'ě', '┘', '┌', '█', '▄', 'Ţ', 'Ů', '▀',
    // 0xE0..0xEF
    'Ó', 'ß', 'Ô', 'Ń', 'ń', 'ň', 'Š', 'š', 'Ŕ', 'Ú', 'ŕ', 'Ű', 'ý', 'Ý', 'ţ', '´',
    // 0xF0..0xFF
    '\u{AD}', '˝', '˛', 'ˇ', '˘', '§', '÷', '¸', '°', '¨', '˙', 'ű', 'Ř', 'ř', '■', '\u{A0}',
];

/// Windows-1250, upper half. Central European Windows: same layout
/// idea as 1252, different letters.
const CP1250_HIGH: [char; 128] = [
    // 0x80..0x8F
    '€', '\0', '‚', '\0', '„', '…', '†', '‡', '\0', '‰', 'Š', '‹', 'Ś', 'Ť', 'Ž', 'Ź',
    // 0x90..0x9F
    '\0', '‘', '’', '“', '”', '•', '–', '—', '\0', '™', 'š', '›', 'ś', 'ť', 'ž', 'ź',
    // 0xA0..0xAF
    '\u{A0}', 'ˇ', '˘', 'Ł', '¤', 'Ą', '¦', '§', '¨', '©', 'Ş', '«', '¬', '\u{AD}', '®', 'Ż',
    // 0xB0..0xBF
    '°', '±', '˛', 'ł', '´', 'µ', '¶', '·', '¸', 'ą', 'ş', '»', 'Ľ', '˝', 'ľ', 'ż',
    // 0xC0..0xCF
    'Ŕ', 'Á', 'Â', 'Ă', 'Ä', 'Ĺ', 'Ć', 'Ç', 'Č', 'É', 'Ę', 'Ë', 'Ě', 'Í', 'Î', 'Ď',
    // 0xD0..0xDF
    'Đ', 'Ń', 'Ň', 'Ó', 'Ô', 'Ő', 'Ö', '×', 'Ř', 'Ů', 'Ú', 'Ű', 'Ü', 'Ý', 'Ţ', 'ß',
    // 0xE0..0xEF
    'ŕ', 'á', 'â', 'ă', 'ä', 'ĺ', 'ć', 'ç', 'č', 'é', 'ę', 'ë', 'ě', 'í', 'î', 'ď',
    // 0xF0..0xFF
    'đ', 'ń', 'ň', 'ó', 'ô', 'ő', 'ö', '÷', 'ř', 'ů', 'ú', 'ű', 'ü', 'ý', 'ţ', '˙',
];

/// Windows-1252, upper half. Western European Windows, and what a
/// great deal of mislabelled "Latin-1" actually is: 0x80..0x9F carry the smart
/// quotes and dashes that ISO-8859-1 leaves as control codes.
const CP1252_HIGH: [char; 128] = [
    // 0x80..0x8F
    '€', '\0', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\0', 'Ž', '\0',
    // 0x90..0x9F
    '\0', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\0', 'ž', 'Ÿ',
    // 0xA0..0xAF
    '\u{A0}', '¡', '¢', '£', '¤', '¥', '¦', '§', '¨', '©', 'ª', '«', '¬', '\u{AD}', '®', '¯',
    // 0xB0..0xBF
    '°', '±', '²', '³', '´', 'µ', '¶', '·', '¸', '¹', 'º', '»', '¼', '½', '¾', '¿',
    // 0xC0..0xCF
    'À', 'Á', 'Â', 'Ã', 'Ä', 'Å', 'Æ', 'Ç', 'È', 'É', 'Ê', 'Ë', 'Ì', 'Í', 'Î', 'Ï',
    // 0xD0..0xDF
    'Ð', 'Ñ', 'Ò', 'Ó', 'Ô', 'Õ', 'Ö', '×', 'Ø', 'Ù', 'Ú', 'Û', 'Ü', 'Ý', 'Þ', 'ß',
    // 0xE0..0xEF
    'à', 'á', 'â', 'ã', 'ä', 'å', 'æ', 'ç', 'è', 'é', 'ê', 'ë', 'ì', 'í', 'î', 'ï',
    // 0xF0..0xFF
    'ð', 'ñ', 'ò', 'ó', 'ô', 'õ', 'ö', '÷', 'ø', 'ù', 'ú', 'û', 'ü', 'ý', 'þ', 'ÿ',
];

/// ISO-8859-1, upper half. Latin-1: 0x80..0x9F are C1 control
/// codes, which is the difference from Windows-1252.
const ISO8859_1_HIGH: [char; 128] = [
    // 0x80..0x8F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0x90..0x9F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0xA0..0xAF
    '\u{A0}', '¡', '¢', '£', '¤', '¥', '¦', '§', '¨', '©', 'ª', '«', '¬', '\u{AD}', '®', '¯',
    // 0xB0..0xBF
    '°', '±', '²', '³', '´', 'µ', '¶', '·', '¸', '¹', 'º', '»', '¼', '½', '¾', '¿',
    // 0xC0..0xCF
    'À', 'Á', 'Â', 'Ã', 'Ä', 'Å', 'Æ', 'Ç', 'È', 'É', 'Ê', 'Ë', 'Ì', 'Í', 'Î', 'Ï',
    // 0xD0..0xDF
    'Ð', 'Ñ', 'Ò', 'Ó', 'Ô', 'Õ', 'Ö', '×', 'Ø', 'Ù', 'Ú', 'Û', 'Ü', 'Ý', 'Þ', 'ß',
    // 0xE0..0xEF
    'à', 'á', 'â', 'ã', 'ä', 'å', 'æ', 'ç', 'è', 'é', 'ê', 'ë', 'ì', 'í', 'î', 'ï',
    // 0xF0..0xFF
    'ð', 'ñ', 'ò', 'ó', 'ô', 'õ', 'ö', '÷', 'ø', 'ù', 'ú', 'û', 'ü', 'ý', 'þ', 'ÿ',
];

/// ISO-8859-2, upper half. Latin-2, the ISO take on Central Europe.
const ISO8859_2_HIGH: [char; 128] = [
    // 0x80..0x8F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0x90..0x9F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0xA0..0xAF
    '\u{A0}', 'Ą', '˘', 'Ł', '¤', 'Ľ', 'Ś', '§', '¨', 'Š', 'Ş', 'Ť', 'Ź', '\u{AD}', 'Ž', 'Ż',
    // 0xB0..0xBF
    '°', 'ą', '˛', 'ł', '´', 'ľ', 'ś', 'ˇ', '¸', 'š', 'ş', 'ť', 'ź', '˝', 'ž', 'ż',
    // 0xC0..0xCF
    'Ŕ', 'Á', 'Â', 'Ă', 'Ä', 'Ĺ', 'Ć', 'Ç', 'Č', 'É', 'Ę', 'Ë', 'Ě', 'Í', 'Î', 'Ď',
    // 0xD0..0xDF
    'Đ', 'Ń', 'Ň', 'Ó', 'Ô', 'Ő', 'Ö', '×', 'Ř', 'Ů', 'Ú', 'Ű', 'Ü', 'Ý', 'Ţ', 'ß',
    // 0xE0..0xEF
    'ŕ', 'á', 'â', 'ă', 'ä', 'ĺ', 'ć', 'ç', 'č', 'é', 'ę', 'ë', 'ě', 'í', 'î', 'ď',
    // 0xF0..0xFF
    'đ', 'ń', 'ň', 'ó', 'ô', 'ő', 'ö', '÷', 'ř', 'ů', 'ú', 'ű', 'ü', 'ý', 'ţ', '˙',
];

/// ISO-8859-5, upper half. The ISO Cyrillic that almost nobody used;
/// the world went with KOI8 and then CP1251.
const ISO8859_5_HIGH: [char; 128] = [
    // 0x80..0x8F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0x90..0x9F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0xA0..0xAF
    '\u{A0}', 'Ё', 'Ђ', 'Ѓ', 'Є', 'Ѕ', 'І', 'Ї', 'Ј', 'Љ', 'Њ', 'Ћ', 'Ќ', '\u{AD}', 'Ў', 'Џ',
    // 0xB0..0xBF
    'А', 'Б', 'В', 'Г', 'Д', 'Е', 'Ж', 'З', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О', 'П',
    // 0xC0..0xCF
    'Р', 'С', 'Т', 'У', 'Ф', 'Х', 'Ц', 'Ч', 'Ш', 'Щ', 'Ъ', 'Ы', 'Ь', 'Э', 'Ю', 'Я',
    // 0xD0..0xDF
    'а', 'б', 'в', 'г', 'д', 'е', 'ж', 'з', 'и', 'й', 'к', 'л', 'м', 'н', 'о', 'п',
    // 0xE0..0xEF
    'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ъ', 'ы', 'ь', 'э', 'ю', 'я',
    // 0xF0..0xFF
    '№', 'ё', 'ђ', 'ѓ', 'є', 'ѕ', 'і', 'ї', 'ј', 'љ', 'њ', 'ћ', 'ќ', '§', 'ў', 'џ',
];

/// ISO-8859-15, upper half. Latin-9: Latin-1 with the euro sign and
/// a few letters French and Finnish were missing.
const ISO8859_15_HIGH: [char; 128] = [
    // 0x80..0x8F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0x90..0x9F
    '', '', '', '', '', '', '', '', '', '', '', '', '', '', '', '',
    // 0xA0..0xAF
    '\u{A0}', '¡', '¢', '£', '€', '¥', 'Š', '§', 'š', '©', 'ª', '«', '¬', '\u{AD}', '®', '¯',
    // 0xB0..0xBF
    '°', '±', '²', '³', 'Ž', 'µ', '¶', '·', 'ž', '¹', 'º', '»', 'Œ', 'œ', 'Ÿ', '¿',
    // 0xC0..0xCF
    'À', 'Á', 'Â', 'Ã', 'Ä', 'Å', 'Æ', 'Ç', 'È', 'É', 'Ê', 'Ë', 'Ì', 'Í', 'Î', 'Ï',
    // 0xD0..0xDF
    'Ð', 'Ñ', 'Ò', 'Ó', 'Ô', 'Õ', 'Ö', '×', 'Ø', 'Ù', 'Ú', 'Û', 'Ü', 'Ý', 'Þ', 'ß',
    // 0xE0..0xEF
    'à', 'á', 'â', 'ã', 'ä', 'å', 'æ', 'ç', 'è', 'é', 'ê', 'ë', 'ì', 'í', 'î', 'ï',
    // 0xF0..0xFF
    'ð', 'ñ', 'ò', 'ó', 'ô', 'õ', 'ö', '÷', 'ø', 'ù', 'ú', 'û', 'ü', 'ý', 'þ', 'ÿ',
];

/// KOI8-U, upper half. KOI8-R with four box drawing cells traded for
/// the Ukrainian letters ґ є і ї.
const KOI8U_HIGH: [char; 128] = [
    // 0x80..0x8F
    '─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '▀', '▄', '█', '▌', '▐',
    // 0x90..0x9F
    '░', '▒', '▓', '⌠', '■', '∙', '√', '≈', '≤', '≥', '\u{A0}', '⌡', '°', '²', '·', '÷',
    // 0xA0..0xAF
    '═', '║', '╒', 'ё', 'є', '╔', 'і', 'ї', '╗', '╘', '╙', '╚', '╛', 'ґ', '╝', '╞',
    // 0xB0..0xBF
    '╟', '╠', '╡', 'Ё', 'Є', '╣', 'І', 'Ї', '╦', '╧', '╨', '╩', '╪', 'Ґ', '╬', '©',
    // 0xC0..0xCF
    'ю', 'а', 'б', 'ц', 'д', 'е', 'ф', 'г', 'х', 'и', 'й', 'к', 'л', 'м', 'н', 'о',
    // 0xD0..0xDF
    'п', 'я', 'р', 'с', 'т', 'у', 'ж', 'в', 'ь', 'ы', 'з', 'ш', 'э', 'щ', 'ч', 'ъ',
    // 0xE0..0xEF
    'Ю', 'А', 'Б', 'Ц', 'Д', 'Е', 'Ф', 'Г', 'Х', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О',
    // 0xF0..0xFF
    'П', 'Я', 'Р', 'С', 'Т', 'У', 'Ж', 'В', 'Ь', 'Ы', 'З', 'Ш', 'Э', 'Щ', 'Ч', 'Ъ',
];

/// Mac OS Roman, upper half. What classic Mac text files hold.
const MACROMAN_HIGH: [char; 128] = [
    // 0x80..0x8F
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è',
    // 0x90..0x9F
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü',
    // 0xA0..0xAF
    '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø',
    // 0xB0..0xBF
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø',
    // 0xC0..0xCF
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ',
    // 0xD0..0xDF
    '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ',
    // 0xE0..0xEF
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
    // 0xF0..0xFF
    '', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

/// Mac OS Cyrillic, upper half.
const MACCYRILLIC_HIGH: [char; 128] = [
    // 0x80..0x8F
    'А', 'Б', 'В', 'Г', 'Д', 'Е', 'Ж', 'З', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О', 'П',
    // 0x90..0x9F
    'Р', 'С', 'Т', 'У', 'Ф', 'Х', 'Ц', 'Ч', 'Ш', 'Щ', 'Ъ', 'Ы', 'Ь', 'Э', 'Ю', 'Я',
    // 0xA0..0xAF
    '†', '°', 'Ґ', '£', '§', '•', '¶', 'І', '®', '©', '™', 'Ђ', 'ђ', '≠', 'Ѓ', 'ѓ',
    // 0xB0..0xBF
    '∞', '±', '≤', '≥', 'і', 'µ', 'ґ', 'Ј', 'Є', 'є', 'Ї', 'ї', 'Љ', 'љ', 'Њ', 'њ',
    // 0xC0..0xCF
    'ј', 'Ѕ', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{A0}', 'Ћ', 'ћ', 'Ќ', 'ќ', 'ѕ',
    // 0xD0..0xDF
    '–', '—', '“', '”', '‘', '’', '÷', '„', 'Ў', 'ў', 'Џ', 'џ', '№', 'Ё', 'ё', 'я',
    // 0xE0..0xEF
    'а', 'б', 'в', 'г', 'д', 'е', 'ж', 'з', 'и', 'й', 'к', 'л', 'м', 'н', 'о', 'п',
    // 0xF0..0xFF
    'р', 'с', 'т', 'у', 'ф', 'х', 'ц', 'ч', 'ш', 'щ', 'ъ', 'ы', 'ь', 'э', 'ю', '€',
];


#[cfg(test)]
mod tests {
    use super::*;

    /// Decode as if the bytes came from the very start of the file.
    fn cells(enc: Encoding, bytes: &[u8]) -> String {
        cells_at(enc, 0, bytes)
    }

    fn cells_at(enc: Encoding, start: u64, bytes: &[u8]) -> String {
        let mut out = Vec::new();
        decode(enc, start, bytes, &mut out);
        assert_eq!(out.len(), bytes.len(), "one cell per byte");
        out.into_iter().collect()
    }

    #[test]
    fn every_encoding_yields_one_cell_per_byte() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        for enc in Encoding::ALL {
            // Both parities: UTF-16 decodes differently depending on where in
            // the file the slice starts, and must still land one cell a byte.
            for start in [0u64, 1] {
                let s = cells_at(enc, start, &bytes);
                assert_eq!(s.chars().count(), 256, "{} at {}", enc.name(), start);
            }
        }
    }

    #[test]
    fn names_fit_the_width_the_status_bar_reserves() {
        let longest = Encoding::ALL.iter().map(|e| e.name().len()).max().unwrap();
        assert_eq!(longest, Encoding::NAME_WIDTH);
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
    fn every_cyrillic_code_page_decodes_its_own_bytes() {
        let cases = [
            (Encoding::Cp866, [0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2]),
            (Encoding::Cp1251, [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]),
            (Encoding::Koi8R, [0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4]),
            (Encoding::Koi8U, [0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4]),
            (Encoding::Iso8859_5, [0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2]),
            (Encoding::MacCyrillic, [0x8F, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]),
        ];
        for (enc, bytes) in cases {
            assert_eq!(cells(enc, &bytes), "Привет", "{}", enc.name());
        }
    }

    #[test]
    fn the_western_code_pages_differ_where_they_are_supposed_to() {
        // What mislabelled "Latin-1" usually really is: 0x80..0x9F carries
        // smart quotes in Windows-1252 and C1 control codes in ISO-8859-1.
        assert_eq!(cells(Encoding::Cp1252, &[0x93, 0x94]), "“”");
        assert_eq!(cells(Encoding::Iso8859_1, &[0x93, 0x94]), "..");
        // Latin-9 spends one of Latin-1's spare slots on the euro sign.
        assert_eq!(cells(Encoding::Iso8859_15, &[0xA4]), "€");
        assert_eq!(cells(Encoding::Iso8859_1, &[0xA4]), "¤");
        // The DOS pages keep CP437's box drawing and swap letters into it.
        assert_eq!(cells(Encoding::Cp850, &[0x84, 0x94]), "äö");
        assert_eq!(cells(Encoding::Cp852, &[0xA5]), "ą");
        assert_eq!(cells(Encoding::Cp1250, &[0xE1]), "á");
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
    fn utf16_decodes_both_byte_orders() {
        let le = [0x1F, 0x04, 0x40, 0x04, 0x38, 0x04];
        assert_eq!(cells(Encoding::Utf16Le, &le), "П·р·и·");
        let be = [0x04, 0x1F, 0x04, 0x40, 0x04, 0x38];
        assert_eq!(cells(Encoding::Utf16Be, &be), "П·р·и·");
        // Read the other way round it is a different alphabet entirely.
        assert_ne!(cells(Encoding::Utf16Be, &le), "П·р·и·");
    }

    #[test]
    fn utf16_takes_its_alignment_from_the_file_not_the_slice() {
        // The same four bytes read one byte further into the file pair up
        // differently, and mean something else entirely. Parity has to come
        // from the file, not from wherever the caller started reading.
        let bytes = [0xFF, 0x00, 0x41, 0x00];
        assert_eq!(cells_at(Encoding::Utf16Be, 0, &bytes), ".·.·");
        assert_eq!(cells_at(Encoding::Utf16Be, 1, &bytes), "·A·.");
    }

    #[test]
    fn utf16_keeps_a_surrogate_pair_together() {
        // U+1F600 over four bytes: one character, so one cell and three
        // continuations. The emoji itself is two columns wide and refused like
        // any other wide character, but the pair is still read as the single
        // character it is.
        assert_eq!(cells(Encoding::Utf16Le, &[0x3D, 0xD8, 0x00, 0xDE]), ".···");
        // A high surrogate with nothing after it is not a character at all.
        assert_eq!(cells(Encoding::Utf16Le, &[0x00, 0x01, 0x3D, 0xD8]), "Ā·.·");
    }

    #[test]
    fn utf16_marks_an_unpaired_surrogate_and_a_half_unit() {
        assert_eq!(cells(Encoding::Utf16Le, &[0x3D, 0xD8, 0x41, 0x00]), ".·A·");
        assert_eq!(cells(Encoding::Utf16Le, &[0x41]), ".");
    }

    #[test]
    fn wide_characters_are_not_drawn_since_they_would_shift_the_grid() {
        // A CJK ideograph is three bytes but two columns wide, so the letter
        // itself is refused; the continuation cells still mark the sequence.
        assert_eq!(cells(Encoding::Utf8, "漢".as_bytes()), ".··");
    }

    /// A cell the terminal draws in one column: either a character we vouch
    /// for, or one of the two stand-ins.
    fn one_column(c: char) -> bool {
        one_cell(c) || c == '.' || c == CONTINUATION
    }

    #[test]
    fn every_glyph_our_own_code_pages_can_produce_is_drawable() {
        for enc in Encoding::ALL {
            let high = match high_table(enc) {
                Some(t) => t,
                None => continue,
            };
            for (i, &c) in high.iter().enumerate() {
                // Undefined slots, the C1 controls ISO-8859-1 keeps in its
                // upper half, the zero-width soft hyphen, and MacRoman's
                // private-use Apple logo are all meant to come out as dots.
                let stands_for_nothing = c == '\0'
                    || c.is_control()
                    || c == '\u{AD}'
                    || ('\u{E000}'..='\u{F8FF}').contains(&c);
                assert!(
                    stands_for_nothing || one_cell(c),
                    "{} byte {:#04X} is {:?}, which the viewer would refuse to draw",
                    enc.name(),
                    i + 0x80,
                    c
                );
            }
        }
        // CP437's lower half is glyphs rather than control codes, and all of
        // them have to make it to the screen — they are the DOS look.
        for &c in &CP437[..0x80] {
            assert!(one_cell(c), "CP437 {:?}", c);
        }
    }

    #[test]
    fn no_encoding_can_produce_a_cell_that_is_not_one_column() {
        // The invariant the whole layout stands on, swept rather than argued.
        // A cell the terminal draws in two columns, or in none, shifts
        // everything after it: the row stops matching the width the viewer
        // padded it to, and the background ends short of the edge.
        let mut cells = Vec::new();

        for enc in [Encoding::Utf16Le, Encoding::Utf16Be] {
            for u in 0..=0xFFFFu32 {
                let bytes = (u as u16).to_le_bytes();
                decode(enc, 0, &bytes, &mut cells);
                for &c in &cells {
                    assert!(one_column(c), "{} unit {:#06X} -> {:?}", enc.name(), u, c);
                }
            }
        }

        let mut buf = [0u8; 4];
        for u in 0..=0xFFFFu32 {
            if let Some(ch) = char::from_u32(u) {
                let s = ch.encode_utf8(&mut buf);
                decode(Encoding::Utf8, 0, s.as_bytes(), &mut cells);
                for &c in &cells {
                    assert!(one_column(c), "UTF-8 {:?} -> {:?}", ch, c);
                }
            }
        }

        let all_bytes: Vec<u8> = (0..=255u8).collect();
        for enc in Encoding::ALL {
            decode(enc, 0, &all_bytes, &mut cells);
            for &c in &cells {
                assert!(one_column(c), "{} -> {:?}", enc.name(), c);
            }
        }
    }

    #[test]
    fn search_text_round_trips_through_every_single_byte_page() {
        for enc in Encoding::ALL {
            if high_table(enc).is_none() {
                continue;
            }
            let bytes = match encode_text(enc, "Hello, world") {
                Ok(b) => b,
                Err(c) => panic!("{} cannot hold {:?}", enc.name(), c),
            };
            assert_eq!(bytes.len(), 12, "{}", enc.name());
            assert_eq!(cells(enc, &bytes), "Hello, world", "{}", enc.name());
        }
    }

    #[test]
    fn cyrillic_search_text_round_trips_through_the_cyrillic_pages() {
        for enc in [
            Encoding::Cp866,
            Encoding::Cp1251,
            Encoding::Koi8R,
            Encoding::Koi8U,
            Encoding::Iso8859_5,
            Encoding::MacCyrillic,
        ] {
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
    fn search_text_in_a_utf16_view_is_utf16_bytes() {
        // Which is how you find a string in a Windows binary at all.
        assert_eq!(encode_text(Encoding::Utf16Le, "Ok").unwrap(), vec![0x4F, 0, 0x6B, 0]);
        assert_eq!(encode_text(Encoding::Utf16Be, "Ok").unwrap(), vec![0, 0x4F, 0, 0x6B]);
        let cyr = encode_text(Encoding::Utf16Le, "Привет").unwrap();
        assert_eq!(cyr.len(), 12);
        assert_eq!(cells(Encoding::Utf16Le, &cyr), "П·р·и·в·е·т·");
    }

    #[test]
    fn encoding_reports_the_char_it_cannot_represent() {
        assert_eq!(encode_text(Encoding::Cp437, "Привет"), Err('П'));
        assert_eq!(encode_text(Encoding::Cp866, "漢"), Err('漢'));
        assert_eq!(encode_text(Encoding::Iso8859_1, "ł"), Err('ł'));
    }

    #[test]
    fn ascii_encodes_the_same_way_in_every_code_page() {
        for enc in Encoding::ALL {
            if matches!(enc, Encoding::Utf16Le | Encoding::Utf16Be) {
                continue; // two bytes per character there, by definition
            }
            assert_eq!(encode_text(enc, "REC.").unwrap(), b"REC.".to_vec(), "{}", enc.name());
        }
    }

    #[test]
    fn cycling_encodings_wraps_both_ways() {
        assert_eq!(Encoding::Ascii.prev().name(), "UTF-16BE");
        assert_eq!(Encoding::Utf16Be.next().name(), "ASCII");
        let mut e = Encoding::Ascii;
        for _ in 0..Encoding::ALL.len() {
            e = e.next();
        }
        assert!(e == Encoding::Ascii);
    }
}
