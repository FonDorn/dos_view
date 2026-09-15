//! Byte pattern with wildcards.
//!
//! The same type serves three things: search, splitting lines into records and
//! highlighting. The syntax is shared: either text, or `x:` followed by hex
//! pairs where `??` means any byte.
//!
//! Text is encoded with the current code page, so typing Cyrillic while
//! viewing a CP866 file searches for CP866 bytes, not UTF-8 ones.
//!
//! Wildcards break ordinary fast search: memmem can only look for an exact
//! sequence. So the longest solid run of known bytes inside the pattern is
//! picked as an "anchor", the file is sieved with it at full memmem speed, and
//! the full match with wildcards is only checked at the points it finds.

use std::fmt;

use memchr::memmem;

use crate::encoding::{self, Encoding};

/// Why a pattern typed by the user did not parse.
#[derive(Debug, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    BadHex,
    /// The character cannot be written in the current encoding.
    Unrepresentable(char, Encoding),
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Empty => write!(f, "Pattern is empty"),
            PatternError::BadHex => write!(f, "Bad hex: expected pairs like x:4D 5A or x:4D ??"),
            PatternError::Unrepresentable(c, enc) => {
                write!(f, "'{}' is not in {}", c, enc.name())
            }
        }
    }
}

#[derive(Debug)]
pub struct Pattern {
    items: Vec<Option<u8>>,
    /// Where the anchor sits inside the pattern.
    anchor_off: usize,
    anchor: Vec<u8>,
    /// The text as typed — shown in the title bar.
    src: String,
}

impl Pattern {
    /// Parse user input, encoding text with `enc`.
    pub fn parse(s: &str, enc: Encoding) -> Result<Self, PatternError> {
        let items: Vec<Option<u8>> = if let Some(rest) =
            s.strip_prefix("x:").or_else(|| s.strip_prefix("X:"))
        {
            let chars: Vec<char> = rest.chars().filter(|c| !c.is_whitespace()).collect();
            if chars.is_empty() || chars.len() % 2 != 0 {
                return Err(PatternError::BadHex);
            }
            let mut out = Vec::with_capacity(chars.len() / 2);
            for pair in chars.chunks(2) {
                if pair[0] == '?' && pair[1] == '?' {
                    out.push(None);
                } else {
                    let hi = pair[0].to_digit(16).ok_or(PatternError::BadHex)?;
                    let lo = pair[1].to_digit(16).ok_or(PatternError::BadHex)?;
                    out.push(Some((hi * 16 + lo) as u8));
                }
            }
            out
        } else if s.is_empty() {
            return Err(PatternError::Empty);
        } else {
            encoding::encode_text(enc, s)
                .map_err(|c| PatternError::Unrepresentable(c, enc))?
                .into_iter()
                .map(Some)
                .collect()
        };

        let (anchor_off, anchor) = longest_literal_run(&items);
        Ok(Self {
            items,
            anchor_off,
            anchor,
            src: s.to_string(),
        })
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn src(&self) -> &str {
        &self.src
    }

    /// Does the pattern match when laid over `hay` starting at `pos`?
    fn matches_at(&self, hay: &[u8], pos: usize) -> bool {
        if pos + self.items.len() > hay.len() {
            return false;
        }
        self.items
            .iter()
            .zip(&hay[pos..])
            .all(|(item, &b)| match item {
                Some(x) => *x == b,
                None => true,
            })
    }

    /// Every match in the buffer, by ascending offset.
    ///
    /// Matches do not overlap: after one is found the search continues past its
    /// end. Otherwise a pattern like `AA` over a run of identical bytes would
    /// match on every byte and record splitting would degenerate.
    pub fn find_all(&self, hay: &[u8]) -> Vec<usize> {
        let n = self.items.len();
        let mut out = Vec::new();
        if n == 0 || hay.len() < n {
            return out;
        }

        // A pattern that is nothing but wildcards matches everywhere.
        if self.anchor.is_empty() {
            let mut i = 0;
            while i + n <= hay.len() {
                out.push(i);
                i += n;
            }
            return out;
        }

        let finder = memmem::Finder::new(&self.anchor);
        let mut from = 0usize;
        while from < hay.len() {
            let rel = match finder.find(&hay[from..]) {
                Some(r) => r,
                None => break,
            };
            let apos = from + rel;
            from = apos + 1;
            if apos < self.anchor_off {
                continue;
            }
            let cand = apos - self.anchor_off;
            if self.matches_at(hay, cand) {
                out.push(cand);
                from = from.max(cand + n);
            }
        }
        out
    }
}

/// The longest solid run of known bytes and where it starts in the pattern.
fn longest_literal_run(items: &[Option<u8>]) -> (usize, Vec<u8>) {
    let (mut best_start, mut best_len) = (0usize, 0usize);
    let (mut cur_start, mut cur_len) = (0usize, 0usize);
    for (i, item) in items.iter().enumerate() {
        match item {
            Some(_) => {
                if cur_len == 0 {
                    cur_start = i;
                }
                cur_len += 1;
                if cur_len > best_len {
                    best_len = cur_len;
                    best_start = cur_start;
                }
            }
            None => cur_len = 0,
        }
    }
    let anchor = items[best_start..best_start + best_len]
        .iter()
        .map(|x| x.unwrap())
        .collect();
    (best_start, anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Most tests do not care about encoding: ASCII input is the same bytes
    /// in every code page.
    fn pat(s: &str) -> Pattern {
        Pattern::parse(s, Encoding::Ascii).unwrap()
    }

    #[test]
    fn plain_text_is_a_literal_pattern() {
        let p = pat("REC");
        assert_eq!(p.len(), 3);
        assert_eq!(p.find_all(b"--REC--REC"), vec![2, 7]);
    }

    #[test]
    fn hex_input_without_wildcards() {
        let p = pat("x:89 50 4E 47");
        assert_eq!(p.find_all(&[0, 0x89, 0x50, 0x4E, 0x47, 0]), vec![1]);
    }

    #[test]
    fn wildcards_match_any_byte() {
        let p = pat("x:89 ?? 4E 47");
        assert_eq!(p.len(), 4);
        assert_eq!(p.find_all(&[0x89, 0xFF, 0x4E, 0x47]), vec![0]);
        assert_eq!(p.find_all(&[0x89, 0x00, 0x4E, 0x47]), vec![0]);
        assert!(p.find_all(&[0x89, 0x00, 0x4E, 0x48]).is_empty());
    }

    #[test]
    fn anchor_is_the_longest_literal_run() {
        // The anchor has to be "4E 47 AA", not the lone 89.
        let p = pat("x:89 ?? 4E 47 AA");
        assert_eq!(p.anchor, vec![0x4E, 0x47, 0xAA]);
        assert_eq!(p.anchor_off, 2);
        let hay = [0x00, 0x89, 0x11, 0x4E, 0x47, 0xAA, 0x00];
        assert_eq!(p.find_all(&hay), vec![1]);
    }

    #[test]
    fn wildcard_at_the_start_does_not_underflow() {
        let p = pat("x:?? 41 42");
        // "AB" at the very start would match from offset -1: that has to be
        // dropped, not wrapped around by an underflowing subtraction.
        assert_eq!(p.find_all(b"AB--\x00AB"), vec![4]);
    }

    #[test]
    fn all_wildcards_match_everywhere() {
        let p = pat("x:?? ??");
        assert_eq!(p.find_all(&[1, 2, 3, 4, 5]), vec![0, 2]);
    }

    #[test]
    fn matches_do_not_overlap() {
        let p = pat("AA");
        assert_eq!(p.find_all(b"AAAAAA"), vec![0, 2, 4]);
    }

    #[test]
    fn text_is_encoded_with_the_current_code_page() {
        // The same query hunts for different bytes depending on the view.
        let cp1251 = Pattern::parse("Привет", Encoding::Cp1251).unwrap();
        assert_eq!(cp1251.len(), 6);
        assert_eq!(cp1251.find_all(&[0, 0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]), vec![1]);

        let cp866 = Pattern::parse("Привет", Encoding::Cp866).unwrap();
        assert_eq!(cp866.find_all(&[0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2]), vec![0]);
        // CP1251 bytes must not match a CP866 query.
        assert!(cp866.find_all(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]).is_empty());

        let utf8 = Pattern::parse("Привет", Encoding::Utf8).unwrap();
        assert_eq!(utf8.len(), 12);
        assert_eq!(utf8.find_all("--Привет".as_bytes()), vec![2]);
    }

    #[test]
    fn hex_input_ignores_the_encoding() {
        for enc in Encoding::ALL {
            let p = Pattern::parse("x:CF F0", enc).unwrap();
            assert_eq!(p.find_all(&[0xCF, 0xF0]), vec![0]);
        }
    }

    #[test]
    fn broken_input_is_rejected() {
        let e = |s| Pattern::parse(s, Encoding::Ascii).unwrap_err();
        assert_eq!(e(""), PatternError::Empty);
        assert_eq!(e("x:ABC"), PatternError::BadHex);
        assert_eq!(e("x:zz"), PatternError::BadHex);
        assert_eq!(e("x:?5"), PatternError::BadHex);
    }

    #[test]
    fn a_char_missing_from_the_code_page_is_named_in_the_error() {
        assert_eq!(
            Pattern::parse("Привет", Encoding::Cp437).unwrap_err(),
            PatternError::Unrepresentable('П', Encoding::Cp437)
        );
    }
}
