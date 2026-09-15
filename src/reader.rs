//! A reading window over a file of any size.
//!
//! The file is never read whole. One buffer lives in memory (1 MiB by default)
//! and travels along with the view position. Opening a file costs one `open`
//! plus one `metadata`, so open time does not depend on file size at all.

use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::pattern::Pattern;

/// Window buffer size. 1 MiB is enough that scrolling a few screens either way
/// never touches the disk.
const CHUNK: usize = 1 << 20;

/// Block size for streaming search.
const SEARCH_BLOCK: usize = 1 << 20;

/// What "the file changed" is decided on. Cheap to take and cheap to compare,
/// which is what lets it be checked on an idle timer.
#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(md: &Metadata) -> Self {
        Stamp {
            len: md.len(),
            modified: md.modified().ok(),
        }
    }
}

pub struct FileWindow {
    file: File,
    /// Kept so the file can be reopened: an editor that saves by writing a new
    /// file and renaming it over the old one leaves our descriptor pointing at
    /// the replaced inode, where no further change would ever show up.
    path: PathBuf,
    len: u64,
    stamp: Stamp,
    buf: Vec<u8>,
    /// File offset that buf[0] corresponds to.
    buf_start: u64,
}

impl FileWindow {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let md = file.metadata()?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            len: md.len(),
            stamp: Stamp::of(&md),
            buf: Vec::new(),
            buf_start: 0,
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Reopen the file and see whether it changed underneath us.
    ///
    /// Returns true when the length or the modification time moved, in which
    /// case the window buffer is dropped so the next read comes from disk.
    /// The caller is left to clamp whatever positions it holds: the file may
    /// have shrunk.
    pub fn refresh(&mut self) -> io::Result<bool> {
        let file = File::open(&self.path)?;
        let md = file.metadata()?;
        let stamp = Stamp::of(&md);
        if stamp == self.stamp {
            return Ok(false);
        }
        self.file = file;
        self.len = md.len();
        self.stamp = stamp;
        self.buf.clear();
        self.buf_start = 0;
        Ok(true)
    }

    /// Return up to `want` bytes starting at offset `start`.
    ///
    /// The result may be shorter than requested (end of file). The call is
    /// cheap when the requested range is already in the buffer — during normal
    /// scrolling that is the overwhelming majority of calls.
    pub fn read_at(&mut self, start: u64, want: usize) -> io::Result<&[u8]> {
        if start >= self.len || want == 0 {
            return Ok(&[]);
        }
        let avail = ((self.len - start).min(want as u64)) as usize;

        if !self.covers(start, avail) {
            self.refill(start, avail)?;
        }

        // buf_start is always <= start after a refill, but check anyway: the
        // file could have been truncated by another process between metadata()
        // and read().
        if start < self.buf_start {
            return Ok(&[]);
        }
        let off = (start - self.buf_start) as usize;
        if off >= self.buf.len() {
            return Ok(&[]);
        }
        let end = (off + avail).min(self.buf.len());
        Ok(&self.buf[off..end])
    }

    fn covers(&self, start: u64, len: usize) -> bool {
        start >= self.buf_start
            && start.saturating_add(len as u64) <= self.buf_start + self.buf.len() as u64
    }

    /// Re-read the buffer so that it covers the requested range.
    ///
    /// The requested piece is placed roughly in the middle of the window, so
    /// scrolling either way stays equally cheap.
    fn refill(&mut self, start: u64, need: usize) -> io::Result<()> {
        let cap = CHUNK.max(need);
        let slack = (cap - need) as u64 / 2;
        let new_start = start.saturating_sub(slack);

        self.buf.resize(cap, 0);
        self.file.seek(SeekFrom::Start(new_start))?;

        let filled = read_full(&mut self.file, &mut self.buf[..cap])?;
        self.buf.truncate(filled);
        self.buf_start = new_start;
        Ok(())
    }

    /// Find up to `n` matches of the pattern starting at offset `from`, in
    /// ascending order.
    ///
    /// Reads in blocks overlapping by `len(pattern) - 1` bytes, so matches on a
    /// block seam are not lost and memory does not depend on file size. Stops
    /// as soon as it has `n` — which is why "step down one record" does not
    /// read the file to the end.
    pub fn find_forward(&mut self, from: u64, pat: &Pattern, n: usize) -> io::Result<Vec<u64>> {
        let mut out = Vec::new();
        if n == 0 || pat.len() == 0 || pat.len() as u64 > self.len {
            return Ok(out);
        }
        let overlap = pat.len() - 1;
        let mut block = vec![0u8; SEARCH_BLOCK + overlap];
        let mut pos = from.min(self.len);

        while pos < self.len && out.len() < n {
            let want = (SEARCH_BLOCK + overlap).min((self.len - pos) as usize);
            self.file.seek(SeekFrom::Start(pos))?;
            let filled = read_full(&mut self.file, &mut block[..want])?;
            if filled == 0 {
                break;
            }
            for i in pat.find_all(&block[..filled]) {
                out.push(pos + i as u64);
                if out.len() >= n {
                    break;
                }
            }
            if filled <= overlap {
                break;
            }
            pos += (filled - overlap) as u64;
        }
        Ok(out)
    }

    /// Find up to `n` matches that start strictly before `before`, in
    /// descending order.
    ///
    /// Needed for paging up in record mode: stepping to the previous record
    /// means searching backwards, not only forwards.
    pub fn find_backward(&mut self, before: u64, pat: &Pattern, n: usize) -> io::Result<Vec<u64>> {
        let mut out = Vec::new();
        if n == 0 || pat.len() == 0 || pat.len() as u64 > self.len {
            return Ok(out);
        }
        let overlap = (pat.len() - 1) as u64;
        let mut end = before.min(self.len);

        while end > 0 && out.len() < n {
            let start = end.saturating_sub(SEARCH_BLOCK as u64);
            // Read a little past the block edge, otherwise a match that starts
            // inside the block but ends past it would be lost.
            let read_end = (end + overlap).min(self.len);
            let mut block = vec![0u8; (read_end - start) as usize];
            self.file.seek(SeekFrom::Start(start))?;
            let filled = read_full(&mut self.file, &mut block)?;

            let mut found: Vec<u64> = pat
                .find_all(&block[..filled])
                .into_iter()
                .map(|i| start + i as u64)
                .filter(|&o| o < end)
                .collect();
            found.reverse();
            for o in found {
                out.push(o);
                if out.len() >= n {
                    break;
                }
            }
            if start == 0 {
                break;
            }
            end = start;
        }
        Ok(out)
    }

    /// The first match at or after `from`.
    pub fn search(&mut self, from: u64, pat: &Pattern) -> io::Result<Option<u64>> {
        Ok(self.find_forward(from, pat, 1)?.first().copied())
    }
}

/// Read until the buffer is full or the file runs out.
/// A single `read` is not obliged to return everything asked for — that is the
/// usual cause of "bytes go missing sometimes" in naive implementations.
fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Encoding;
    use crate::pattern::Pattern;
    use crate::testutil::Dir;
    use std::io::Write;

    fn pat(s: &str) -> Pattern {
        Pattern::parse(s, Encoding::Ascii).unwrap()
    }

    /// A file larger than the window buffer, so the buffer really does move.
    fn big_file(len: usize) -> (Dir, std::path::PathBuf) {
        let dir = Dir::new();
        let path = dir.path().join("data.bin");
        let mut f = File::create(&path).unwrap();
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        f.write_all(&data).unwrap();
        f.sync_all().unwrap();
        (dir, path)
    }

    #[test]
    fn reads_across_window_boundaries() {
        let len = CHUNK * 3 + 777;
        let (_d, path) = big_file(len);
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.len(), len as u64);

        // Jump back and forth to force the buffer to move.
        for &off in &[0u64, 10, CHUNK as u64 - 3, CHUNK as u64 * 2 + 5, 42, len as u64 - 10] {
            let got = w.read_at(off, 16).unwrap();
            let expect: Vec<u8> = (0..got.len())
                .map(|i| ((off as usize + i) % 251) as u8)
                .collect();
            assert_eq!(got, &expect[..], "offset {off}");
        }
    }

    #[test]
    fn reads_past_eof_are_clamped_not_panics() {
        let (_d, path) = big_file(100);
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.read_at(90, 64).unwrap().len(), 10);
        assert!(w.read_at(100, 16).unwrap().is_empty());
        assert!(w.read_at(u64::MAX, 16).unwrap().is_empty());
    }

    #[test]
    fn finds_a_match_that_straddles_two_search_blocks() {
        let len = SEARCH_BLOCK + 64;
        let dir = Dir::new();
        let path = dir.path().join("straddle.bin");
        let mut data = vec![0u8; len];
        // Put the needle exactly on the block seam — the classic spot where a
        // naive search loses it.
        let needle = b"NEEDLE-ON-THE-EDGE";
        let pos = SEARCH_BLOCK - needle.len() / 2;
        data[pos..pos + needle.len()].copy_from_slice(needle);
        File::create(&path).unwrap().write_all(&data).unwrap();

        let p = pat("NEEDLE-ON-THE-EDGE");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.search(0, &p).unwrap(), Some(pos as u64));
        assert_eq!(w.search(pos as u64 + 1, &p).unwrap(), None);
    }

    #[test]
    fn search_respects_the_starting_offset() {
        let dir = Dir::new();
        let path = dir.path().join("multi.bin");
        File::create(&path).unwrap().write_all(b"--AB----AB--").unwrap();
        let p = pat("AB");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.search(0, &p).unwrap(), Some(2));
        assert_eq!(w.search(3, &p).unwrap(), Some(8));
        assert_eq!(w.search(9, &p).unwrap(), None);
    }

    #[test]
    fn cyrillic_search_finds_code_page_bytes() {
        let dir = Dir::new();
        let path = dir.path().join("cp1251.bin");
        // "-- Привет --" written in Windows-1251.
        let mut data = b"-- ".to_vec();
        data.extend_from_slice(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
        data.extend_from_slice(b" --");
        File::create(&path).unwrap().write_all(&data).unwrap();

        let mut w = FileWindow::open(&path).unwrap();
        let p = Pattern::parse("Привет", Encoding::Cp1251).unwrap();
        assert_eq!(w.search(0, &p).unwrap(), Some(3));
        // The same query as UTF-8 bytes is simply not in this file.
        let utf8 = Pattern::parse("Привет", Encoding::Utf8).unwrap();
        assert_eq!(w.search(0, &utf8).unwrap(), None);
    }

    #[test]
    fn backward_search_walks_records_in_reverse() {
        let dir = Dir::new();
        let path = dir.path().join("back.bin");
        File::create(&path).unwrap().write_all(b"--AB--AB--AB--").unwrap();
        let p = pat("AB");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.find_backward(14, &p, 3).unwrap(), vec![10, 6, 2]);
        assert_eq!(w.find_backward(10, &p, 1).unwrap(), vec![6]);
        assert!(w.find_backward(2, &p, 1).unwrap().is_empty());
    }

    #[test]
    fn backward_search_spans_blocks_and_finds_edge_matches() {
        let len = SEARCH_BLOCK * 2 + 100;
        let dir = Dir::new();
        let path = dir.path().join("backbig.bin");
        let mut data = vec![0u8; len];
        let needle = b"EDGE-CASE-MARK";
        // one needle on the block seam, another closer to the end
        let p1 = SEARCH_BLOCK - needle.len() / 2;
        let p2 = SEARCH_BLOCK * 2 + 10;
        data[p1..p1 + needle.len()].copy_from_slice(needle);
        data[p2..p2 + needle.len()].copy_from_slice(needle);
        File::create(&path).unwrap().write_all(&data).unwrap();

        let p = pat("EDGE-CASE-MARK");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(
            w.find_backward(len as u64, &p, 2).unwrap(),
            vec![p2 as u64, p1 as u64]
        );
    }

    #[test]
    fn refresh_picks_up_a_file_that_changed_underneath_us() {
        let dir = Dir::new();
        let path = dir.file("live.bin", b"first");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.read_at(0, 8).unwrap(), b"first");
        assert!(!w.refresh().unwrap(), "nothing changed yet");

        // Appending: the new length has to show up, and the stale window
        // buffer must not be served instead of the new bytes.
        std::fs::write(&path, b"first-and-more").unwrap();
        assert!(w.refresh().unwrap());
        assert_eq!(w.len(), 14);
        assert_eq!(w.read_at(0, 32).unwrap(), b"first-and-more");

        // Truncating: reads past the new end are clamped, not stale.
        std::fs::write(&path, b"hi").unwrap();
        assert!(w.refresh().unwrap());
        assert_eq!(w.len(), 2);
        assert_eq!(w.read_at(0, 32).unwrap(), b"hi");
    }

    #[test]
    fn refresh_follows_a_file_replaced_by_rename() {
        // How most editors save: write a new file, rename it over the old one.
        // Our descriptor then points at an inode nobody writes to again.
        let dir = Dir::new();
        let path = dir.file("target.bin", b"old contents");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.read_at(0, 32).unwrap(), b"old contents");

        let tmp = dir.file("target.bin.tmp", b"brand new contents");
        std::fs::rename(&tmp, &path).unwrap();
        assert!(w.refresh().unwrap());
        assert_eq!(w.read_at(0, 32).unwrap(), b"brand new contents");
    }

    #[test]
    fn forward_search_stops_as_soon_as_it_has_enough() {
        let dir = Dir::new();
        let path = dir.path().join("fwd.bin");
        File::create(&path).unwrap().write_all(b"AB-AB-AB-AB").unwrap();
        let p = pat("AB");
        let mut w = FileWindow::open(&path).unwrap();
        assert_eq!(w.find_forward(0, &p, 2).unwrap(), vec![0, 3]);
        assert_eq!(w.find_forward(1, &p, 9).unwrap(), vec![3, 6, 9]);
    }
}
