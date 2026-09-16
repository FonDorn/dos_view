//! dosview — a DOS-style file viewer for the terminal.
//!
//! An offset column on the left, the data on the right in one of three modes
//! switched with 1..3, decoded with one of six code pages switched with 4.
//! A block cursor marks the byte you are on. File size does not matter: only
//! what is on screen is ever read.

mod encoding;
mod pattern;
mod reader;
#[cfg(test)]
mod testutil;
mod view;

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute, queue,
    style::{Color, Print, SetBackgroundColor, SetForegroundColor},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

use encoding::Encoding;
use pattern::Pattern;
use reader::FileWindow;
use view::Mode;

/// How long to sit on the keyboard before looking at the file again. Short
/// enough that an edit in another window shows up while you watch, long
/// enough to be free.
const IDLE_POLL: Duration = Duration::from_millis(400);

// ── DOS palette ──────────────────────────────────────────────────────────────
// RGB rather than Color::Blue: terminals disagree about what the named colours
// mean, and "the blue from a DOS text mode screen" only comes out right when
// it is spelled out.

const BG: Color = Color::Rgb { r: 0, g: 0, b: 168 };
const FG: Color = Color::Rgb { r: 170, g: 170, b: 170 };
const FG_OFFSET: Color = Color::Rgb { r: 85, g: 255, b: 255 };
const FG_TEXTCOL: Color = Color::Rgb { r: 255, g: 255, b: 85 };
const FG_TRUNC: Color = Color::Rgb { r: 255, g: 85, b: 85 };
const BAR_BG: Color = Color::Rgb { r: 0, g: 168, b: 168 };
const BAR_FG: Color = Color::Rgb { r: 0, g: 0, b: 0 };
// Match highlight: black on yellow, as loud as it gets over the blue.
const HL_BG: Color = Color::Rgb { r: 255, g: 255, b: 85 };
const HL_FG: Color = Color::Rgb { r: 0, g: 0, b: 0 };
// The cursor: a white block, the way a DOS text mode cursor sits on a cell.
// Deliberately not yellow — it has to stay visible on top of a match.
const CUR_BG: Color = Color::Rgb { r: 255, g: 255, b: 255 };
const CUR_FG: Color = Color::Rgb { r: 0, g: 0, b: 168 };

/// How a byte's cell is painted. The cursor wins over a match, which wins over
/// plain text.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Style {
    Plain,
    Match,
    Cursor,
}

enum Input {
    Normal,
    Goto(String),
    Search(String),
    Record(String),
    /// The code page picker, holding what is currently highlighted in it.
    ///
    /// There are nineteen pages and stepping through them with 3 is fine for
    /// the neighbours and tedious for the far side, so there is a list to pick
    /// from. It is a dialog rather than a long press on 3 because a terminal
    /// cannot tell an application how long a key was held: it reports a key,
    /// and at best a repeat — never a duration.
    Pick(usize),
}

/// One row of the layout: the bytes it logically holds, before the window
/// width is applied.
///
/// In the plain modes a row is `bpl` bytes of the grid. With record splitting
/// and wrapping off, a row is a whole record however long it is; with wrapping
/// on, a row is one `bpl`-sized slice of a record.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Row {
    start: u64,
    len: u64,
}

impl Row {
    /// Last byte of the row, or `start` for an empty file.
    fn last(&self) -> u64 {
        self.start + self.len.saturating_sub(1)
    }
}

/// A row clipped to the window: what actually gets drawn.
struct Line {
    /// First byte drawn.
    off: u64,
    /// How many bytes are drawn.
    len: usize,
    /// Bytes of the row scrolled off to the left.
    skipped: u64,
    /// Bytes of the row that did not fit on the right.
    hidden: u64,
}

struct App {
    reader: FileWindow,
    name: String,
    mode: Mode,
    /// Code page used for the text cells, and for encoding text typed into the
    /// search and record prompts.
    enc: Encoding,
    /// Offset of the first visible byte; always the start of a row.
    top: u64,
    /// Where the byte grid is anchored, 0..bpl. Sliding the view sideways in
    /// the plain modes moves this off zero, which is how a structure that does
    /// not start on a line boundary gets lined up. It is state of its own
    /// rather than `top % bpl`: `top` moves for all sorts of reasons, and the
    /// anchor has to survive every one of them.
    align: u64,
    /// The byte the cursor sits on. This is what navigation moves — the view
    /// follows it instead of being steered directly, so lines never slide out
    /// from under each other.
    cur: u64,
    /// Bytes of each record scrolled off to the left. Only meaningful for
    /// records shown on a single line; wrapping makes it pointless.
    hoff: u64,
    /// Wrap a long record onto as many lines as it needs, instead of cutting
    /// it at the window edge.
    wrap: bool,
    /// A line width the user asked for, in bytes. `None` fills the window.
    fixed_bpl: Option<usize>,
    /// The widest line the window can hold right now — the ceiling the line
    /// width keys stop at. Measured by the last frame.
    fit_bpl: usize,
    input: Input,
    status: String,
    /// What we search for and what we highlight.
    needle: Option<Pattern>,
    /// Format pattern — "Format" is what the key bar calls it. When set, a row
    /// is one record of the file rather than a slice of the byte grid, and row
    /// length becomes variable.
    record: Option<Pattern>,
    highlight: bool,
    /// Geometry of the last frame. Key handling runs right after a draw, so
    /// these are the numbers the user is actually looking at.
    rows: usize,
    bpl: usize,
    width: usize,
    /// Scratch buffer for decoded text cells, reused across lines.
    glyphs: Vec<char>,
}

impl App {
    fn new(path: PathBuf) -> io::Result<Self> {
        let name = path.display().to_string();
        let reader = FileWindow::open(&path)?;
        Ok(Self {
            reader,
            name,
            mode: Mode::Hex,
            enc: Encoding::Ascii,
            top: 0,
            align: 0,
            cur: 0,
            hoff: 0,
            wrap: false,
            fixed_bpl: None,
            fit_bpl: 16,
            input: Input::Normal,
            status: String::new(),
            needle: None,
            record: None,
            highlight: true,
            rows: 1,
            bpl: 16,
            width: 80,
            glyphs: Vec::new(),
        })
    }

    // ── Layout geometry ──────────────────────────────────────────────────────
    //
    // Everything below derives from one rule: a row is a run of bytes, and the
    // row after a row starts right where it ends. `layout_rows` walks the rows
    // that fit on screen in one batch; `row_at` answers for a single offset,
    // which is all the cursor needs. The two must agree, so both are written
    // against the same rule rather than against each other.

    fn bpl64(&self) -> u64 {
        self.bpl.max(1) as u64
    }

    /// Start of the record containing `off`.
    fn record_start(&mut self, off: u64) -> io::Result<u64> {
        let pat = match self.record.as_ref() {
            Some(p) => p,
            None => return Ok(off),
        };
        Ok(self
            .reader
            .find_backward(off.saturating_add(1), pat, 1)?
            .first()
            .copied()
            .unwrap_or(0))
    }

    /// One past the last byte of the record starting at `rec`.
    fn record_end(&mut self, rec: u64) -> io::Result<u64> {
        let len = self.reader.len();
        let pat = match self.record.as_ref() {
            Some(p) => p,
            None => return Ok(len),
        };
        Ok(self
            .reader
            .find_forward(rec.saturating_add(1), pat, 1)?
            .first()
            .copied()
            .unwrap_or(len)
            .min(len))
    }

    /// The row that contains `off`.
    fn row_at(&mut self, off: u64) -> io::Result<Row> {
        let len = self.reader.len();
        let bpl = self.bpl64();
        let off = off.min(len.saturating_sub(1));

        if self.record.is_none() {
            let align = self.align;
            // Below the anchor sits one short row: the bytes the view was
            // shifted past still have to live somewhere.
            let start = if off < align {
                0
            } else {
                align + ((off - align) / bpl) * bpl
            };
            let full = if start < align { align } else { bpl };
            return Ok(Row {
                start,
                len: full.min(len.saturating_sub(start)),
            });
        }

        let rec = self.record_start(off)?;
        let end = self.record_end(rec)?;
        if self.wrap {
            let start = rec + ((off - rec) / bpl) * bpl;
            Ok(Row {
                start,
                len: bpl.min(end.saturating_sub(start)),
            })
        } else {
            Ok(Row {
                start: rec,
                len: end.saturating_sub(rec),
            })
        }
    }

    fn row_after(&mut self, row: Row) -> io::Result<Option<Row>> {
        let next = row.start + row.len.max(1);
        if next >= self.reader.len() {
            return Ok(None);
        }
        Ok(Some(self.row_at(next)?))
    }

    fn row_before(&mut self, row: Row) -> io::Result<Option<Row>> {
        if row.start == 0 {
            return Ok(None);
        }
        Ok(Some(self.row_at(row.start - 1)?))
    }

    /// The rows that fit on screen, from `top` down.
    ///
    /// The record modes collect every visible record start in one streaming
    /// pass rather than searching per row: that keeps a frame at one search no
    /// matter how tall the terminal is.
    fn layout_rows(&mut self, rows: usize) -> io::Result<Vec<Row>> {
        let len = self.reader.len();
        let bpl = self.bpl64();
        let mut out = Vec::with_capacity(rows);
        if rows == 0 {
            return Ok(out);
        }

        if self.record.is_none() {
            let mut row = self.row_at(self.top)?;
            loop {
                out.push(row);
                if out.len() >= rows {
                    break;
                }
                match self.row_after(row)? {
                    Some(next) => row = next,
                    None => break,
                }
            }
            return Ok(out);
        }

        let pat = self.record.as_ref().unwrap();
        let mut bounds = Vec::with_capacity(rows + 1);
        bounds.push(self.top.min(len));
        bounds.extend(self.reader.find_forward(self.top.saturating_add(1), pat, rows)?);

        'records: for i in 0..bounds.len() {
            let start = bounds[i];
            if start >= len && !(len == 0 && i == 0) {
                break;
            }
            let end = bounds.get(i + 1).copied().unwrap_or(len).min(len).max(start);
            let full = end - start;
            if !self.wrap {
                out.push(Row { start, len: full });
                if out.len() >= rows {
                    break;
                }
                continue;
            }
            let mut done = 0;
            loop {
                let take = bpl.min(full - done);
                out.push(Row {
                    start: start + done,
                    len: take,
                });
                if out.len() >= rows {
                    break 'records;
                }
                done += take.max(1);
                if done >= full {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Clip a row to the window: apply the sideways scroll and cut whatever
    /// still does not fit.
    fn clip(&self, row: Row) -> Line {
        let skipped = if self.record.is_some() && !self.wrap {
            self.hoff.min(row.len)
        } else {
            0
        };
        let shown = (row.len - skipped).min(self.bpl64());
        Line {
            off: row.start + skipped,
            len: shown as usize,
            skipped,
            hidden: row.len - skipped - shown,
        }
    }

    // ── Navigation ───────────────────────────────────────────────────────────

    /// Keep `top` from running off the end: the view stops once the file's last
    /// row is the bottom one on screen. That way a jump near the end does not
    /// leave a mostly empty screen, and the next key does not snap backwards.
    ///
    /// The limit is measured from the row the last byte is on, not from the
    /// file length. Those differ whenever the length is not a whole number of
    /// rows — and then measuring from the length stops the view a row short,
    /// so the final partial row can never be reached and a cursor on it has
    /// nowhere to be drawn.
    fn clamp_top(&mut self) {
        let len = self.reader.len();
        if self.record.is_some() {
            self.top = self.top.min(len);
            return;
        }
        if len == 0 {
            self.top = 0;
            return;
        }
        let bpl = self.bpl64();
        let align = self.align;
        let last = len - 1;
        let last_row = if last < align {
            0
        } else {
            align + ((last - align) / bpl) * bpl
        };
        let max = last_row.saturating_sub((self.rows as u64).saturating_sub(1) * bpl);
        self.top = self.top.min(max);
    }

    /// Scroll the view by whole rows, leaving the cursor where it is.
    fn scroll_rows(&mut self, n: usize, down: bool) -> io::Result<()> {
        for _ in 0..n {
            let row = self.row_at(self.top)?;
            let next = if down {
                self.row_after(row)?
            } else {
                self.row_before(row)?
            };
            match next {
                Some(r) => self.top = r.start,
                None => break,
            }
        }
        self.clamp_top();
        Ok(())
    }

    /// Bring the cursor into view, moving the screen as little as possible.
    ///
    /// A key step only ever overshoots by a row, so nudging covers it and the
    /// screen scrolls one line at a time the way it should. Anything further
    /// out is a jump — searches, goto — and there the cursor's row goes to the
    /// top instead.
    fn ensure_visible(&mut self) -> io::Result<()> {
        let len = self.reader.len();
        self.cur = self.cur.min(len.saturating_sub(1));
        self.clamp_top();

        for _ in 0..3 {
            let rows = self.layout_rows(self.rows)?;
            let (first, last) = match (rows.first(), rows.last()) {
                (Some(f), Some(l)) => (*f, *l),
                _ => break,
            };
            if self.cur < first.start {
                self.scroll_rows(1, false)?;
                continue;
            }
            if self.cur >= last.start + last.len.max(1) {
                self.scroll_rows(1, true)?;
                continue;
            }
            return self.fix_hoff();
        }

        let row = self.row_at(self.cur)?;
        self.top = row.start;
        self.clamp_top();
        self.fix_hoff()
    }

    /// Keep the cursor inside the sideways window of its row. This is what
    /// makes a long record readable without wrapping: walk the cursor right
    /// and the line follows.
    fn fix_hoff(&mut self) -> io::Result<()> {
        if self.record.is_none() || self.wrap {
            self.hoff = 0;
            return Ok(());
        }
        let bpl = self.bpl64();
        let row = self.row_at(self.cur)?;
        let rel = self.cur - row.start;
        if rel < self.hoff {
            self.hoff = rel;
        } else if rel >= self.hoff + bpl {
            self.hoff = rel - bpl + 1;
        }
        Ok(())
    }

    fn move_cursor(&mut self, delta: i64) -> io::Result<()> {
        self.cur = if delta >= 0 {
            self.cur.saturating_add(delta as u64)
        } else {
            self.cur.saturating_sub((-delta) as u64)
        };
        self.ensure_visible()
    }

    /// Move the cursor `n` rows, keeping its column where the next row is long
    /// enough to have one. Leaves the view alone: the callers decide whether
    /// it should follow or move with it.
    fn walk_cursor(&mut self, n: usize, down: bool) -> io::Result<()> {
        for _ in 0..n {
            let row = self.row_at(self.cur)?;
            let rel = self.cur - row.start;
            let next = if down {
                self.row_after(row)?
            } else {
                self.row_before(row)?
            };
            match next {
                Some(r) => self.cur = (r.start + rel).min(r.last()),
                // Already on the first or last row: go to its far end rather
                // than sit still, so the key always does something.
                None => {
                    self.cur = if down { row.last() } else { 0 };
                    break;
                }
            }
        }
        Ok(())
    }

    /// One row at a time: the cursor walks the screen and the view only
    /// follows once the cursor would leave it.
    fn step_rows(&mut self, n: usize, down: bool) -> io::Result<()> {
        self.walk_cursor(n, down)?;
        self.ensure_visible()
    }

    /// A page moves the view by as many rows as the cursor, so the cursor
    /// keeps its place on the screen instead of sliding to an edge. Both have
    /// to move before the view is settled, or settling the cursor first would
    /// scroll the screen and the page step would land twice as far.
    fn page_rows(&mut self, n: usize, down: bool) -> io::Result<()> {
        self.walk_cursor(n, down)?;
        self.scroll_rows(n, down)?;
        self.ensure_visible()
    }

    /// Jump to the start or the end of the current row — the record, in the
    /// record modes.
    fn go_row_edge(&mut self, end: bool) -> io::Result<()> {
        let row = self.row_at(self.cur)?;
        self.cur = if end { row.last() } else { row.start };
        self.ensure_visible()
    }

    fn go_to(&mut self, off: u64) -> io::Result<()> {
        self.cur = off;
        self.ensure_visible()
    }

    fn go_end(&mut self) -> io::Result<()> {
        let len = self.reader.len();
        self.cur = len.saturating_sub(1);
        let row = self.row_at(self.cur)?;
        self.top = row.start;
        // Back up a screen so the last row is the cursor's, not the only one.
        self.scroll_rows(self.rows.saturating_sub(1), false)?;
        self.ensure_visible()
    }

    /// Slide the byte grid sideways by one byte.
    ///
    /// This is the one movement that is not the cursor's: in the plain modes it
    /// re-anchors the grid so a structure that does not start on a line
    /// boundary can be lined up, and every line moves together. With records
    /// on a single line the grid is record-relative, so the same keys scroll
    /// the records sideways instead.
    fn shift_grid(&mut self, right: bool) -> io::Result<()> {
        if self.record.is_some() {
            if self.wrap {
                self.status = "Wrapped records have no sideways scroll — turn wrap off (9)".into();
                return Ok(());
            }
            let bpl = self.bpl64();
            self.hoff = if right {
                self.hoff.saturating_add(1)
            } else {
                self.hoff.saturating_sub(1)
            };
            // Drag the cursor along so it stays on screen.
            let row = self.row_at(self.cur)?;
            let rel = self.cur - row.start;
            if rel < self.hoff {
                self.cur = (row.start + self.hoff).min(row.last());
            } else if rel >= self.hoff + bpl {
                self.cur = (row.start + self.hoff + bpl - 1).min(row.last());
            }
            return Ok(());
        }

        let bpl = self.bpl64();
        self.align = if right {
            (self.align + 1) % bpl
        } else {
            (self.align + bpl - 1) % bpl
        };
        // Slide the view along with the grid, then settle it back onto a real
        // row start so every line moves by the same byte.
        self.top = if right {
            self.top.saturating_add(1)
        } else {
            self.top.saturating_sub(1)
        };
        let row = self.row_at(self.top)?;
        self.top = row.start;
        // The cursor points at a byte and the grid moved under it; only drag
        // it along if it fell off the top.
        self.cur = self.cur.max(self.top);
        self.ensure_visible()
    }

    // ── State the keys flip ──────────────────────────────────────────────────

    /// Make the data columns narrower or wider.
    ///
    /// Both columns show the same bytes, so the bytes on a line is the only
    /// dial there is: fewer of them and the dump — text column and all — takes
    /// up less of the window. The mode's own limits and the window width are
    /// the stops.
    fn resize_line(&mut self, wider: bool) {
        if self.mode == Mode::Text {
            self.status = "Line width is for the byte modes (1, 2)".into();
            return;
        }
        let (min, _) = view::limits(self.mode);
        let step = view::step(self.mode);
        let want = if wider {
            self.bpl + step
        } else {
            self.bpl.saturating_sub(step)
        };
        let want = want.clamp(min, self.fit_bpl.max(min));
        self.fixed_bpl = Some(want);
        self.status = if want == self.fit_bpl {
            format!("{} bytes per line — the whole window", want)
        } else {
            format!("{} bytes per line", want)
        };
    }

    fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        if self.wrap {
            self.hoff = 0;
        }
        self.status = match (self.wrap, self.record.is_some()) {
            (true, true) => "Records wrap onto as many lines as they need".into(),
            (false, true) => "One line per record; walk the cursor right to see the rest".into(),
            (true, false) => "Wrap on — applies once a record pattern is set (6)".into(),
            (false, false) => "Wrap off — applies once a record pattern is set (6)".into(),
        };
    }

    /// Open the code page list, if there is room on screen to draw it.
    fn open_picker(&mut self) {
        let (w, h) = (self.width, self.rows + 2);
        let (bw, bh) = picker_size();
        if w < bw + 2 || h < bh + 2 {
            self.status = "Window too small for the list — 3 steps through the pages".into();
            return;
        }
        self.input = Input::Pick(self.enc.index());
    }

    /// Switch code page.
    ///
    /// Patterns keep the text they were typed as, so they are re-encoded: a
    /// search for "Привет" has to mean the new code page's bytes from now on.
    /// When the text does not exist in the new code page the old bytes are
    /// kept — the file did not change just because the view did — and the
    /// status line says why the pattern no longer follows the text.
    fn set_encoding(&mut self, enc: Encoding) {
        self.enc = enc;
        self.status = format!("Encoding: {}", enc.name());
        for slot in [&mut self.needle, &mut self.record] {
            let old = match slot.take() {
                Some(p) => p,
                None => continue,
            };
            match Pattern::parse(old.src(), enc) {
                Ok(p) => *slot = Some(p),
                Err(e) => {
                    self.status = format!("{} — pattern left as it was", e);
                    *slot = Some(old);
                }
            }
        }
    }

    /// Pick up changes made to the file by someone else.
    ///
    /// `quiet` is the idle check, which only speaks up when something actually
    /// moved; the reload key reports either way.
    fn reload(&mut self, quiet: bool) -> io::Result<bool> {
        let changed = match self.reader.refresh() {
            Ok(c) => c,
            Err(e) => {
                if !quiet {
                    self.status = format!("Cannot reread the file: {}", e);
                }
                return Ok(false);
            }
        };
        if !changed {
            if !quiet {
                self.status = "File unchanged".into();
            }
            return Ok(false);
        }
        let len = self.reader.len();
        self.top = self.top.min(len);
        self.ensure_visible()?;
        self.status = format!("File changed on disk — reloaded, {} bytes", len);
        Ok(true)
    }

    // ── Drawing ──────────────────────────────────────────────────────────────

    fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        let (w, h) = terminal::size()?;
        if h < 3 || w < 24 {
            queue!(out, MoveTo(0, 0), SetBackgroundColor(BG), SetForegroundColor(FG))?;
            queue!(out, Print("Window too small"))?;
            return out.flush();
        }

        let lay = view::layout(self.mode, w, self.reader.len(), self.fixed_bpl);
        self.rows = (h as usize) - 2;
        self.width = w as usize;
        self.bpl = lay.bpl;
        self.fit_bpl = lay.fit;
        self.align %= self.bpl64();
        // The terminal may have been resized since the last frame, which moves
        // every row boundary; re-settle the view before measuring anything.
        self.ensure_visible()?;

        let readout = self.cursor_readout(lay.offset_digits)?;
        self.draw_title(out, w, &readout)?;

        let rows = self.layout_rows(self.rows)?;
        for row in 0..self.rows {
            queue!(out, MoveTo(0, (row + 1) as u16))?;
            match rows.get(row) {
                Some(&r) => {
                    let line = self.clip(r);
                    self.draw_line(out, w, &line, lay.offset_digits)?
                }
                None => blank(out, w as usize)?,
            }
        }

        self.draw_bottom(out, w, h)?;
        if let Input::Pick(sel) = self.input {
            self.draw_picker(out, w, h, sel)?;
        }
        out.flush()
    }

    /// The byte under the cursor, as hex, decimal and a text cell.
    ///
    /// Every field is a fixed width. A readout that changed length as the
    /// cursor moved would shove everything beside it back and forth on the
    /// title bar, which is unreadable while you are trying to read it.
    fn cursor_readout(&mut self, digits: usize) -> io::Result<String> {
        // "0x" + offset + " " + hex + " " + decimal + " " + quoted glyph
        let width = digits + 13;
        if self.reader.len() == 0 {
            return Ok(format!("{:<width$}", "empty file"));
        }
        // A few bytes of context so a UTF-8 character the cursor sits inside
        // still decodes.
        let back = (encoding::LOOKAHEAD as u64).min(self.cur);
        let data = self
            .reader
            .read_at(self.cur - back, (back as usize) + 1 + encoding::LOOKAHEAD)?;
        let idx = back as usize;
        let b = match data.get(idx) {
            Some(&b) => b,
            None => return Ok(format!("{:<width$}", "past the end")),
        };
        let mut glyphs = std::mem::take(&mut self.glyphs);
        encoding::decode(self.enc, self.cur - back, data, &mut glyphs);
        let glyph = glyphs.get(idx).copied().unwrap_or(' ');
        self.glyphs = glyphs;
        Ok(format!(
            "{:#0off$X} {:02X} {:>3} '{}'",
            self.cur,
            b,
            b,
            glyph,
            off = digits + 2
        ))
    }

    fn draw_line(
        &mut self,
        out: &mut impl Write,
        w: u16,
        ln: &Line,
        digits: usize,
    ) -> io::Result<()> {
        let mode = self.mode;
        let bpl = self.bpl;

        let mut offset_s = String::new();
        view::format_offset(&mut offset_s, ln.off, digits);
        // The trailing space of the offset column doubles as the marker for a
        // record scrolled sideways.
        offset_s.pop();

        // Read a little before the line: a match may have started on the
        // previous line and its tail needs highlighting too. The same context
        // also lets a multi-byte character that started earlier decode.
        // Only a search is highlighted. The format pattern starts every record
        // on screen, so painting it too would be a wall of yellow saying
        // nothing the line breaks do not already say.
        let lookback = match self.needle.as_ref().filter(|_| self.highlight) {
            Some(p) => p.len().saturating_sub(1),
            None => 0,
        }
        .max(encoding::LOOKAHEAD);
        let back = (lookback as u64).min(ln.off) as usize;

        // A few bytes past the line as well, so a character straddling the
        // line break still decodes. Those bytes are context only — the data
        // slice below stops exactly at the end of the line.
        let data = self
            .reader
            .read_at(ln.off - back as u64, back + ln.len + encoding::LOOKAHEAD)?;
        let body = back.min(data.len())..(back + ln.len).min(data.len());
        let bytes = &data[body];

        // Decode from the context start so UTF-8 resyncs on real character
        // boundaries, then drop the context: cell i belongs to byte i.
        let mut glyphs = std::mem::take(&mut self.glyphs);
        encoding::decode(self.enc, ln.off - back as u64, data, &mut glyphs);
        let cells = &glyphs[back.min(glyphs.len())..];

        let mut styles = vec![Style::Plain; ln.len];
        if let Some(pat) = self.needle.as_ref().filter(|_| self.highlight) {
            let plen = pat.len();
            for i in pat.find_all(data) {
                for k in i..i + plen {
                    if k >= back && k - back < ln.len {
                        styles[k - back] = Style::Match;
                    }
                }
            }
        }
        if self.cur >= ln.off && self.cur - ln.off < ln.len as u64 {
            let mut i = (self.cur - ln.off) as usize;
            // Text mode draws a character once, on its first byte. A cursor
            // sitting on a continuation byte has to light up that one cell, or
            // it would have nowhere to show at all.
            if mode == Mode::Text {
                while i > 0 && cells.get(i).copied() == Some(encoding::CONTINUATION) {
                    i -= 1;
                }
            }
            styles[i] = Style::Cursor;
        }

        let mut used = seg(out, FG_OFFSET, BG, &offset_s)?;
        used += if ln.skipped > 0 {
            seg(out, FG_TRUNC, BG, "‹")?
        } else {
            seg(out, FG_OFFSET, BG, " ")?
        };
        used += runs(out, bpl, &styles, FG, |s, i| match bytes.get(i) {
            Some(&b) => view::push_byte(s, mode, b, cells.get(i).copied().unwrap_or(' ')),
            None => view::push_gap(s, mode),
        })?;

        if mode.has_text_column() {
            // The separator doubles as the marker for a record that did not fit.
            let (mfg, mch) = if ln.hidden > 0 { (FG_TRUNC, "›") } else { (FG, " ") };
            used += seg(out, mfg, BG, mch)?;
            used += runs(out, bpl, &styles, FG_TEXTCOL, |s, i| {
                s.push(if i < bytes.len() {
                    cells.get(i).copied().unwrap_or(' ')
                } else {
                    ' '
                });
            })?;
        } else if ln.hidden > 0 {
            used += seg(out, FG_TRUNC, BG, "›")?;
        }

        self.glyphs = glyphs;
        blank(out, (w as usize).saturating_sub(used))
    }

    fn draw_title(&self, out: &mut impl Write, w: u16, readout: &str) -> io::Result<()> {
        let len = self.reader.len();
        let percent = if len <= 1 {
            100
        } else {
            ((self.cur.min(len - 1) as f64 / (len - 1) as f64) * 100.0).round() as u64
        };
        let rec = match &self.record {
            Some(p) => format!(
                " | fmt:{} {}",
                clip(p.src(), 12),
                if self.wrap { "wrap" } else { "cut" }
            ),
            None => String::new(),
        };
        // Mode and code page are padded for the same reason the readout is:
        // stepping through code pages should not shuffle the bar sideways.
        let right = format!(
            " {:<4} {:<enc$}{} | {} | {} bytes | {:>3}% ",
            self.mode.name(),
            self.enc.name(),
            rec,
            readout,
            len,
            percent,
            enc = Encoding::NAME_WIDTH
        );
        let room = (w as usize).saturating_sub(right.chars().count() + 1);
        let left = format!(" {}", trim_left(&self.name, room.saturating_sub(1)));

        queue!(out, MoveTo(0, 0))?;
        let mut used = seg(out, BAR_FG, BAR_BG, &left)?;
        let gap = (w as usize).saturating_sub(used + right.chars().count());
        used += seg_fill(out, BAR_FG, BAR_BG, gap)?;
        used += seg(out, BAR_FG, BAR_BG, &right)?;
        seg_fill(out, BAR_FG, BAR_BG, (w as usize).saturating_sub(used))?;
        Ok(())
    }

    /// The keys the bottom bar offers.
    ///
    /// A slot for something that would do nothing is left out rather than
    /// shown dead: there is nothing to step to before a search, and nothing to
    /// wrap before lines are records. A live search takes the bar over for as
    /// long as it lasts, since walking matches is what you are doing.
    fn bar_slots(&self) -> Vec<(&'static str, &'static str)> {
        if self.needle.is_some() {
            return vec![
                ("9", "Next"),
                ("8", "Prev"),
                ("7", "Find"),
                ("Esc", "Clear"),
                ("10", "Quit"),
            ];
        }
        // 3 doubles as the code page key, so it shows where it will take you.
        let mut slots = vec![
            ("1", "Bin"),
            ("2", "Hex"),
            ("3", if self.mode == Mode::Text { self.enc.name() } else { "Txt" }),
            ("5", "Goto"),
            ("6", "Format"),
            ("7", "Find"),
        ];
        if self.record.is_some() {
            slots.push(("9", if self.wrap { "Wrap" } else { "Cut" }));
        }
        slots.push(("10", "Quit"));
        slots
    }

    fn draw_bottom(&self, out: &mut impl Write, w: u16, h: u16) -> io::Result<()> {
        queue!(out, MoveTo(0, h - 1))?;
        let mut used = 0;

        let prompt = match &self.input {
            Input::Goto(buf) => {
                Some((" Go to offset (hex; d672 for decimal): ".to_string(), buf))
            }
            Input::Search(buf) => Some((
                format!(" Search in {} (x: for hex, ?? any byte): ", self.enc.name()),
                buf,
            )),
            Input::Record(buf) => Some((
                " Format — break lines at (text, or x:0A for bytes, empty to stop): "
                    .to_string(),
                buf,
            )),
            Input::Normal | Input::Pick(_) => None,
        };

        // The picker types nothing, so it gets a hint rather than a prompt.
        if let Input::Pick(_) = self.input {
            let hint = " Code page — ↑↓←→ to choose, Enter to take it, Esc to leave it be ";
            used += seg(out, BAR_FG, BAR_BG, hint)?;
            return seg_fill(out, BAR_FG, BAR_BG, (w as usize).saturating_sub(used)).map(|_| ());
        }

        match prompt {
            Some((label, buf)) => {
                used += seg(out, BAR_FG, BAR_BG, &label)?;
                used += seg(out, BAR_FG, BAR_BG, buf)?;
                used += seg(out, BAR_FG, BAR_BG, "_")?;
            }
            None => {
                if !self.status.is_empty() {
                    used += seg(out, BAR_FG, BAR_BG, &format!(" {} ", self.status))?;
                } else {
                    for (key, label) in self.bar_slots() {
                        used += seg(out, FG, BG, key)?;
                        used += seg(out, BAR_FG, BAR_BG, label)?;
                        used += seg(out, FG, BG, " ")?;
                    }
                }
            }
        }
        seg_fill(out, BAR_FG, BAR_BG, (w as usize).saturating_sub(used))?;
        Ok(())
    }

    /// The code page list, drawn over the top of everything else the way a DOS
    /// program put a dialog on the screen.
    fn draw_picker(&self, out: &mut impl Write, w: u16, h: u16, sel: usize) -> io::Result<()> {
        let all = Encoding::ALL;
        let down = picker_rows();
        let (bw, bh) = picker_size();
        if (w as usize) < bw || (h as usize) < bh {
            return Ok(());
        }
        let cell = Encoding::NAME_WIDTH + 4;
        let inner = bw - 2;
        let x = ((w as usize - bw) / 2) as u16;
        let y = ((h as usize - bh) / 2) as u16;

        queue!(out, MoveTo(x, y))?;
        seg(out, BAR_FG, BAR_BG, &format!("╔{}╗", "═".repeat(inner)))?;
        queue!(out, MoveTo(x, y + 1))?;
        seg(out, BAR_FG, BAR_BG, &format!("║{:^inner$}║", "Code page"))?;
        queue!(out, MoveTo(x, y + 2))?;
        seg(out, BAR_FG, BAR_BG, &format!("╟{}╢", "─".repeat(inner)))?;

        for r in 0..down {
            queue!(out, MoveTo(x, y + 3 + r as u16))?;
            seg(out, BAR_FG, BAR_BG, "║")?;
            for c in 0..2 {
                let i = c * down + r;
                match all.get(i) {
                    Some(&enc) => {
                        // ▸ is where you are in the list, • is what the screen
                        // behind the dialog is actually being drawn with.
                        let mark = if i == sel {
                            "▸"
                        } else if enc == self.enc {
                            "•"
                        } else {
                            " "
                        };
                        let text = format!(
                            "{:<cell$}",
                            format!("{} {:<name$} ", mark, enc.name(), name = Encoding::NAME_WIDTH)
                        );
                        if i == sel {
                            seg(out, CUR_FG, CUR_BG, &text)?;
                        } else {
                            seg(out, BAR_FG, BAR_BG, &text)?;
                        }
                    }
                    None => {
                        seg_fill(out, BAR_FG, BAR_BG, cell)?;
                    }
                }
            }
            seg(out, BAR_FG, BAR_BG, "║")?;
        }
        queue!(out, MoveTo(x, y + 3 + down as u16))?;
        seg(out, BAR_FG, BAR_BG, &format!("╚{}╝", "═".repeat(inner)))?;
        Ok(())
    }

    // ── Keyboard ─────────────────────────────────────────────────────────────

    /// Returns true when it is time to quit.
    fn on_key(&mut self, k: KeyEvent) -> io::Result<bool> {
        match self.input {
            Input::Normal => self.on_key_normal(k),
            Input::Pick(_) => {
                self.on_key_pick(k);
                Ok(false)
            }
            _ => {
                self.on_key_prompt(k)?;
                Ok(false)
            }
        }
    }

    fn on_key_pick(&mut self, k: KeyEvent) {
        let all = Encoding::ALL;
        let sel = match self.input {
            Input::Pick(i) => i.min(all.len() - 1),
            _ => return,
        };
        let down = picker_rows();
        let next = match k.code {
            KeyCode::Up => (sel + all.len() - 1) % all.len(),
            KeyCode::Down => (sel + 1) % all.len(),
            // The list is laid out in columns, so sideways is a column jump.
            KeyCode::Left => sel.saturating_sub(down),
            KeyCode::Right => (sel + down).min(all.len() - 1),
            KeyCode::Home => 0,
            KeyCode::End => all.len() - 1,
            KeyCode::Enter => {
                self.input = Input::Normal;
                self.set_encoding(all[sel]);
                return;
            }
            KeyCode::Esc => {
                self.input = Input::Normal;
                self.status = "Code page unchanged".into();
                return;
            }
            _ => sel,
        };
        self.input = Input::Pick(next);
    }

    fn on_key_normal(&mut self, k: KeyEvent) -> io::Result<bool> {
        self.status.clear();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let sup = k.modifiers.contains(KeyModifiers::SUPER);
        let page = self.rows.saturating_sub(1).max(1);

        match k.code {
            // Escape backs out of a search first. Only once there is nothing
            // to back out of does it mean "quit".
            KeyCode::Esc if self.needle.is_some() => {
                self.needle = None;
                self.status = "Search cleared".into();
            }
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc | KeyCode::F(10) => {
                return Ok(true)
            }
            KeyCode::Char('c') if ctrl => return Ok(true),

            // The ends of the line, on keys that actually arrive. macOS takes
            // Ctrl+← and Ctrl+→ for switching Spaces and Terminal keeps Cmd for
            // its own menus, so neither ever reaches us there — but Ctrl+A,
            // Ctrl+E, ^ and $ are plain control and printable characters that
            // no terminal or window manager wants for itself.
            KeyCode::Char('a') if ctrl => self.go_row_edge(false)?,
            KeyCode::Char('e') if ctrl => self.go_row_edge(true)?,
            KeyCode::Char('^') => self.go_row_edge(false)?,
            KeyCode::Char('$') => self.go_row_edge(true)?,

            KeyCode::Char('1') | KeyCode::F(1) => self.mode = Mode::Binary,
            KeyCode::Char('2') | KeyCode::F(2) => self.mode = Mode::Hex,
            // Text mode is also the code page key: the first press gets you
            // there, every press after that steps to the next page. There are
            // nineteen of them and nowhere near that many free keys.
            KeyCode::Char('3') => {
                if self.mode == Mode::Text {
                    self.set_encoding(self.enc.next());
                } else {
                    self.mode = Mode::Text;
                    self.status =
                        format!("Text in {} — 3 again for the next code page", self.enc.name());
                }
            }
            // Terminals disagree about whether a shifted letter arrives as
            // itself, as the modifier, or as both; take any of it.
            // Same key, bigger gesture: 3 steps to the next page, F3 opens the
            // list of all of them.
            KeyCode::F(3) | KeyCode::Char('#') => self.open_picker(),
            KeyCode::Char('e') if !shift && !ctrl => self.set_encoding(self.enc.next()),
            KeyCode::Char('e') | KeyCode::Char('E') => self.set_encoding(self.enc.prev()),
            // 9 answers to whichever slot the bar is currently showing for it
            // — Next while a search is live, Wrap/Cut once a record pattern
            // is set — and does nothing while neither is true, same as the
            // bar shows nothing for it then.
            KeyCode::Char('9') | KeyCode::F(9) if self.needle.is_some() => {
                self.search_step(true)?
            }
            KeyCode::Char('9') | KeyCode::F(9) if self.record.is_some() => self.toggle_wrap(),
            KeyCode::Char('[') => self.resize_line(false),
            KeyCode::Char(']') => self.resize_line(true),
            KeyCode::Char('\\') => {
                self.fixed_bpl = None;
                self.status = "Line width: as wide as the window".into();
            }

            // Sideways: the grid itself.
            KeyCode::Left if shift => self.shift_grid(false)?,
            KeyCode::Right if shift => self.shift_grid(true)?,
            // And the arrows too, for the terminals that can deliver them.
            KeyCode::Left if sup || ctrl || alt => self.go_row_edge(false)?,
            KeyCode::Right if sup || ctrl || alt => self.go_row_edge(true)?,
            KeyCode::Left => self.move_cursor(-1)?,
            KeyCode::Right => self.move_cursor(1)?,

            KeyCode::Down => self.step_rows(1, true)?,
            KeyCode::Up => self.step_rows(1, false)?,
            KeyCode::PageDown | KeyCode::Char(' ') => self.page_rows(page, true)?,
            KeyCode::PageUp => self.page_rows(page, false)?,
            // Home and End take the whole file; the ends of a line are on the
            // modified arrows.
            KeyCode::Home => self.go_to(0)?,
            KeyCode::End => self.go_end()?,

            // Every slot the bottom bar advertises answers to its own digit:
            // the bar said 5, 7 and 8 long before anything was listening.
            KeyCode::Char('g') | KeyCode::Char('5') | KeyCode::F(5) => {
                self.input = Input::Goto(String::new())
            }
            KeyCode::Char('6') | KeyCode::F(6) => self.input = Input::Record(String::new()),
            KeyCode::Char('/') | KeyCode::Char('7') | KeyCode::F(7) => {
                self.input = Input::Search(String::new())
            }
            KeyCode::Char('n') | KeyCode::F(8) => self.search_step(true)?,
            KeyCode::Char('8') if self.needle.is_some() => self.search_step(false)?,
            KeyCode::Char('p') | KeyCode::Char('N') => self.search_step(false)?,
            KeyCode::Char('r') => {
                self.reload(false)?;
            }
            KeyCode::Char('h') => {
                self.highlight = !self.highlight;
                self.status = if self.highlight {
                    "Highlight on".into()
                } else {
                    "Highlight off".into()
                };
            }

            _ => {}
        }
        Ok(false)
    }

    fn on_key_prompt(&mut self, k: KeyEvent) -> io::Result<()> {
        // Decide while the mutable borrow of the buffer is alive, act after —
        // otherwise the borrow checker rightly objects to assigning
        // self.input while it is still borrowed.
        enum Action {
            Keep,
            Cancel,
            Commit,
        }

        let action = match &mut self.input {
            Input::Goto(buf) | Input::Search(buf) | Input::Record(buf) => match k.code {
                KeyCode::Char(c) => {
                    buf.push(c);
                    Action::Keep
                }
                KeyCode::Backspace => {
                    buf.pop();
                    Action::Keep
                }
                KeyCode::Enter => Action::Commit,
                KeyCode::Esc => Action::Cancel,
                _ => Action::Keep,
            },
            // Neither of these types anything; they are handled elsewhere.
            Input::Normal | Input::Pick(_) => return Ok(()),
        };

        match action {
            Action::Keep => return Ok(()),
            Action::Cancel => {
                self.input = Input::Normal;
                return Ok(());
            }
            Action::Commit => {}
        }

        let input = std::mem::replace(&mut self.input, Input::Normal);
        match input {
            Input::Goto(buf) => match view::parse_offset(&buf) {
                Some(off) if off <= self.reader.len() => self.go_to(off)?,
                Some(_) => self.status = "Offset is past the end of the file".into(),
                None => self.status = "Not an offset: expected 1234 or 0x4D5A".into(),
            },
            // A search leaves the formatting alone — you go looking for
            // something inside the shape you set up, not instead of it.
            Input::Search(buf) => match Pattern::parse(&buf, self.enc) {
                Ok(pat) => {
                    self.needle = Some(pat);
                    let from = self.cur;
                    self.run_search(from, true)?;
                }
                Err(e) => self.status = format!("{} — pick a code page with 3", e),
            },
            Input::Record(buf) => {
                if buf.trim().is_empty() {
                    self.record = None;
                    self.hoff = 0;
                    self.status = "Formatting off — back to the byte grid".into();
                    self.ensure_visible()?;
                } else {
                    match Pattern::parse(&buf, self.enc) {
                        Ok(pat) => self.set_format(pat)?,
                        Err(e) => self.status = format!("{} — pick a code page with 3", e),
                    }
                }
            }
            Input::Normal | Input::Pick(_) => {}
        }
        Ok(())
    }

    /// Step to the next or previous match from where the cursor is.
    fn search_step(&mut self, forward: bool) -> io::Result<()> {
        if self.needle.is_none() {
            self.status = "Nothing to search for yet (7)".into();
            return Ok(());
        }
        let from = if forward {
            self.cur.saturating_add(1)
        } else {
            self.cur
        };
        self.run_search(from, forward)
    }

    fn run_search(&mut self, from: u64, forward: bool) -> io::Result<()> {
        let len = self.reader.len();
        // The pattern borrow is held only for the search itself: after that a
        // &mut self is needed as a whole.
        let found = {
            let pat = match self.needle.as_ref() {
                Some(p) => p,
                None => return Ok(()),
            };
            if forward {
                match self.reader.search(from, pat)? {
                    Some(o) => Some((o, false)),
                    // Wrap round to the start, like "continue from the top?".
                    None => self
                        .reader
                        .search(0, pat)?
                        .filter(|&o| o < from)
                        .map(|o| (o, true)),
                }
            } else {
                match self.reader.find_backward(from, pat, 1)?.first().copied() {
                    Some(o) => Some((o, false)),
                    // Nothing behind us: wrap round to the last match instead.
                    None => self
                        .reader
                        .find_backward(len, pat, 1)?
                        .first()
                        .copied()
                        .filter(|&o| o >= from)
                        .map(|o| (o, true)),
                }
            }
        };

        match found {
            Some((off, wrapped)) => {
                self.go_to(off)?;
                self.status = match (wrapped, forward) {
                    (false, _) => format!("Found at {:#X}", off),
                    (true, true) => format!("Wrapped to the start: {:#X}", off),
                    (true, false) => format!("Wrapped to the end: {:#X}", off),
                };
            }
            None => self.status = "Not found".into(),
        }
        Ok(())
    }

    /// Take a format pattern: from here on a line is one record of the file.
    ///
    /// A pattern that occurs nowhere is refused rather than accepted. Accepting
    /// it would leave the whole file as a single record on a single line, which
    /// looks exactly like the viewer having ignored what you typed — and that
    /// is what typing `0A` instead of `x:0A` gets you, since plain text is a
    /// perfectly good pattern that simply is not in the file. The two searches
    /// below cover the file between them, so "not found" means not anywhere.
    fn set_format(&mut self, pat: Pattern) -> io::Result<()> {
        let from = self.cur;
        let start = {
            let p = &pat;
            match self
                .reader
                .find_backward(from.saturating_add(1), p, 1)?
                .first()
                .copied()
            {
                Some(o) => Some(o),
                None => self.reader.find_forward(from, p, 1)?.first().copied(),
            }
        };

        let start = match start {
            Some(o) => o,
            None => {
                let src = pat.src();
                let hint = if src.len().is_multiple_of(2) && src.chars().all(|c| c.is_ascii_hexdigit()) {
                    format!(" — for the bytes {}, write x:{}", src, src)
                } else {
                    String::new()
                };
                self.status = format!("\"{}\" is nowhere in the file{}", src, hint);
                return Ok(());
            }
        };

        // A search from before would go on highlighting hits that have nothing
        // to do with the format just asked for.
        self.needle = None;
        self.hoff = 0;
        // Start on a record boundary, or the first line would begin in the
        // middle of the previous record.
        self.top = start;
        self.status = format!(
            "Lines break at \"{}\" ({})",
            pat.src(),
            if self.wrap { "wrapped" } else { "one line each" }
        );
        self.record = Some(pat);
        self.ensure_visible()
    }
}

/// How many rows the code page list needs, in two columns.
fn picker_rows() -> usize {
    Encoding::ALL.len().div_ceil(2)
}

/// Width and height of the code page box, in cells: two columns of
/// "▸ NAME  " inside a border, with a title and a rule above them.
fn picker_size() -> (usize, usize) {
    ((Encoding::NAME_WIDTH + 4) * 2 + 2, picker_rows() + 4)
}

// ── Small output helpers ─────────────────────────────────────────────────────

/// Print a piece and return how many cells it took.
fn seg(out: &mut impl Write, fg: Color, bg: Color, s: &str) -> io::Result<usize> {
    queue!(out, SetForegroundColor(fg), SetBackgroundColor(bg), Print(s))?;
    Ok(s.chars().count())
}

fn seg_fill(out: &mut impl Write, fg: Color, bg: Color, n: usize) -> io::Result<usize> {
    if n == 0 {
        return Ok(0);
    }
    let pad = " ".repeat(n);
    queue!(out, SetForegroundColor(fg), SetBackgroundColor(bg), Print(&pad))?;
    Ok(n)
}

/// Print a column of `n` bytes, split into runs of equal style. Neighbouring
/// bytes in the same style are merged into one write so that we do not spray
/// escape sequences per byte.
fn runs<F>(
    out: &mut impl Write,
    n: usize,
    styles: &[Style],
    fg: Color,
    mut emit: F,
) -> io::Result<usize>
where
    F: FnMut(&mut String, usize),
{
    let mut used = 0;
    let mut i = 0;
    while i < n {
        let style = styles.get(i).copied().unwrap_or(Style::Plain);
        let mut s = String::new();
        let mut j = i;
        while j < n && styles.get(j).copied().unwrap_or(Style::Plain) == style {
            emit(&mut s, j);
            j += 1;
        }
        used += match style {
            Style::Plain => seg(out, fg, BG, &s)?,
            Style::Match => seg(out, HL_FG, HL_BG, &s)?,
            Style::Cursor => seg(out, CUR_FG, CUR_BG, &s)?,
        };
        i = j;
    }
    Ok(used)
}

/// Pad the line out to the edge with blue — this leaves no leftovers from the
/// previous frame and makes the background solid without relying on how a
/// particular terminal implements Clear.
fn blank(out: &mut impl Write, n: usize) -> io::Result<()> {
    seg_fill(out, FG, BG, n).map(|_| ())
}

/// Trim a long path on the left, keeping the tail: the file name matters more
/// than the root.
fn trim_left(s: &str, room: usize) -> String {
    let count = s.chars().count();
    if count <= room {
        return s.to_string();
    }
    let skip = count.saturating_sub(room.saturating_sub(1));
    let tail: String = s.chars().skip(skip).collect();
    format!("…{}", tail)
}

/// Trim on the right, for the title bar.
fn clip(s: &str, room: usize) -> String {
    if s.chars().count() <= room {
        s.to_string()
    } else {
        s.chars().take(room.saturating_sub(1)).chain(['…']).collect()
    }
}

// ── Entry point ──────────────────────────────────────────────────────────────

fn main() {
    let path = match std::env::args_os().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("dosview — a DOS-style file viewer for the terminal\n");
            eprintln!("Usage: dosview <file>\n");
            eprintln!("  1/2          mode: binary, hex");
            eprintln!("  3            text mode; again for the next of 19 code pages");
            eprintln!("  F3 or #      pick a code page from the list");
            eprintln!("  [ ] \\        narrower / wider / window-wide data columns");
            eprintln!("  ←→↑↓         move the cursor");
            eprintln!("  Home/End     start / end of the file");
            eprintln!("  Ctrl+A / Ctrl+E  start / end of the line (also ^ and $)");
            eprintln!("  PgUp/PgDn    page up and down");
            eprintln!("  Shift+←→     slide the byte grid (for unaligned structures)");
            eprintln!("  5 or F5      go to offset — hex, so 2A0; d672 for decimal");
            eprintln!("  6 or F6      format: break lines on a pattern, e.g. x:0A");
            eprintln!("  9 or F9      wrap long formatted lines instead of cutting them");
            eprintln!("  7 or F7      search; x: for hex bytes, ?? for any byte");
            eprintln!("  n or F8      next match; p for the previous one (9/8 while searching)");
            eprintln!("  Esc          clear the search");
            eprintln!("  h            toggle match highlighting");
            eprintln!("  r            reread the file now (it is also watched while idle)");
            eprintln!("  q or F10     quit");
            std::process::exit(2);
        }
    };

    if let Err(e) = run(path) {
        // The terminal was already restored in run(), so stderr is safe.
        eprintln!("dosview: {}", e);
        std::process::exit(1);
    }
}

/// Whether the kitty keyboard protocol was turned on, so that it is turned
/// back off exactly once — including from the panic hook.
static ENHANCED: AtomicBool = AtomicBool::new(false);

fn run(path: PathBuf) -> io::Result<()> {
    let mut app = App::new(path)?;

    // If anything panics, the terminal must not be left in raw mode on the
    // alternate screen — that would hand the user a dead console.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        default_hook(info);
    }));

    terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide)?;

    // Ask for the kitty keyboard protocol. Plain xterm key reporting has no way
    // to say "Cmd" at all, and cannot tell Alt+Left from Escape followed by a
    // letter; with this, modified arrows arrive as themselves.
    //
    // Asked for blind, on purpose: crossterm can query whether the terminal
    // supports it, but that means writing a question and waiting up to two
    // seconds for an answer that never comes from the terminals which do not.
    // A viewer that opens instantly on a 50 GB file has no business stalling
    // two seconds on a keyboard question. A terminal that does not know the
    // sequence swallows it, which is exactly what we want.
    if execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        ENHANCED.store(true, Ordering::Relaxed);
    }

    let result = event_loop(&mut app, &mut out);

    restore()?;
    result
}

fn restore() -> io::Result<()> {
    let mut out = io::stdout();
    if ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    execute!(out, Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()
}

fn event_loop(app: &mut App, out: &mut impl Write) -> io::Result<()> {
    let mut dirty = true;
    loop {
        if dirty {
            app.draw(out)?;
            dirty = false;
        }
        // Waiting with a timeout rather than blocking is what lets the file be
        // watched: every time the keyboard goes quiet we look at it again, and
        // a frame is only redrawn when something actually moved.
        if event::poll(IDLE_POLL)? {
            match event::read()? {
                // Windows delivers events for both key press and key release —
                // without the filter every keystroke would fire twice.
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.on_key(k)? {
                        return Ok(());
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        } else if app.reload(true)? {
            dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Dir;

    /// An app over a scratch file, with the geometry a draw would have left
    /// behind: 4 rows of 8 bytes.
    fn app(bytes: &[u8]) -> (Dir, App) {
        let dir = Dir::new();
        let path = dir.file("data.bin", bytes);
        let mut a = App::new(path).unwrap();
        a.rows = 4;
        a.bpl = 8;
        (dir, a)
    }

    fn rec_app(bytes: &[u8], pattern: &str) -> (Dir, App) {
        let (dir, mut a) = app(bytes);
        a.record = Some(Pattern::parse(pattern, Encoding::Ascii).unwrap());
        (dir, a)
    }

    fn row(a: &mut App, off: u64) -> (u64, u64) {
        let r = a.row_at(off).unwrap();
        (r.start, r.len)
    }

    #[test]
    fn plain_rows_are_the_byte_grid() {
        let (_d, mut a) = app(&(0..20u8).collect::<Vec<_>>());
        assert_eq!(row(&mut a, 0), (0, 8));
        assert_eq!(row(&mut a, 7), (0, 8));
        assert_eq!(row(&mut a, 8), (8, 8));
        // The last row is short, because the file ends there.
        assert_eq!(row(&mut a, 19), (16, 4));
    }

    #[test]
    fn the_row_after_one_starts_where_it_ends() {
        let (_d, mut a) = app(&(0..20u8).collect::<Vec<_>>());
        let first = a.row_at(0).unwrap();
        let second = a.row_after(first).unwrap().unwrap();
        assert_eq!((second.start, second.len), (8, 8));
        assert_eq!(a.row_before(second).unwrap().unwrap(), first);
        assert!(a.row_before(first).unwrap().is_none());
        let last = a.row_at(19).unwrap();
        assert!(a.row_after(last).unwrap().is_none());
    }

    #[test]
    fn arrows_move_the_cursor_and_the_view_holds_still() {
        let (_d, mut a) = app(&(0..64u8).collect::<Vec<_>>());
        a.step_rows(1, true).unwrap();
        assert_eq!((a.cur, a.top), (8, 0), "one row down, screen unmoved");
        // The complaint this fixes: a left arrow after a step down used to
        // slide every line by a byte. Now it walks the cursor.
        a.move_cursor(-1).unwrap();
        assert_eq!((a.cur, a.top), (7, 0));
        a.move_cursor(1).unwrap();
        assert_eq!((a.cur, a.top), (8, 0));
    }

    #[test]
    fn the_view_scrolls_only_once_the_cursor_would_leave_it() {
        let (_d, mut a) = app(&(0..64u8).collect::<Vec<_>>());
        for _ in 0..3 {
            a.step_rows(1, true).unwrap();
        }
        assert_eq!((a.cur, a.top), (24, 0), "still on the last visible row");
        a.step_rows(1, true).unwrap();
        assert_eq!((a.cur, a.top), (32, 8), "now it scrolls, by one row");
    }

    #[test]
    fn a_page_keeps_the_cursor_on_the_same_screen_row() {
        // 4 rows of 8: a page is 3 rows, or 24 bytes.
        let (_d, mut a) = app(&(0..200u8).collect::<Vec<_>>());
        a.step_rows(2, true).unwrap();
        assert_eq!((a.cur, a.top), (16, 0), "third row from the top");

        a.page_rows(3, true).unwrap();
        assert_eq!(a.top, 24, "the view moved a page");
        assert_eq!(a.cur, 40, "and so did the cursor: still the third row");

        a.page_rows(3, false).unwrap();
        assert_eq!((a.cur, a.top), (16, 0));
    }

    #[test]
    fn the_last_row_of_a_file_that_is_not_a_whole_number_of_rows_is_reachable() {
        // 4 rows of 8 on a 21-byte file: the last row holds the single byte at
        // offset 16. Stopping the view a row short of it — which measuring the
        // limit from the file length does — leaves the cursor nowhere to be
        // drawn, so it simply vanishes at the end of the file.
        let (_d, mut a) = app(&(0..21u8).collect::<Vec<_>>());

        a.go_end().unwrap();
        assert_eq!(a.cur, 20);
        let rows = a.layout_rows(a.rows).unwrap();
        let last = rows.last().copied().unwrap();
        assert!(last.start <= 20 && 20 < last.start + last.len, "cursor on screen");

        // Walking there a row at a time has to reach it too.
        a.go_to(0).unwrap();
        a.step_rows(1, true).unwrap();
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 16, "the last row, not one short of it");
        let rows = a.layout_rows(a.rows).unwrap();
        assert!(rows.iter().any(|r| r.start == 16), "and it is on screen");
        // One more step has nowhere further to go, so it runs to the end.
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 20);

        // Same going right off the end of the second-to-last row.
        a.go_to(15).unwrap();
        a.move_cursor(1).unwrap();
        assert_eq!(a.cur, 16);
        let rows = a.layout_rows(a.rows).unwrap();
        assert!(rows.iter().any(|r| r.start == 16));
    }

    #[test]
    fn the_view_still_stops_at_the_last_screenful() {
        // A file that is a whole number of rows must behave as it always did:
        // the bottom row of the last screen is the last row of the file, with
        // no blank rows past it.
        let (_d, mut a) = app(&(0..64u8).collect::<Vec<_>>());
        a.go_end().unwrap();
        assert_eq!((a.cur, a.top), (63, 32), "four rows of eight, ending at 63");
        a.scroll_rows(5, true).unwrap();
        assert_eq!(a.top, 32, "and it will not scroll past that");
    }

    #[test]
    fn stepping_down_keeps_the_column() {
        let (_d, mut a) = app(&(0..64u8).collect::<Vec<_>>());
        a.go_to(3).unwrap();
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 11, "column 3 of the next row");
        a.step_rows(1, false).unwrap();
        assert_eq!(a.cur, 3);
    }

    #[test]
    fn home_and_end_take_the_file_the_modified_arrows_take_the_line() {
        let (_d, mut a) = app(&(0..20u8).collect::<Vec<_>>());
        a.go_to(11).unwrap();
        // Cmd, Ctrl or Alt with an arrow: the ends of the line.
        a.go_row_edge(false).unwrap();
        assert_eq!(a.cur, 8);
        a.go_row_edge(true).unwrap();
        assert_eq!(a.cur, 15);
        // Home and End: the ends of the file.
        a.go_end().unwrap();
        assert_eq!(a.cur, 19);
        a.go_to(0).unwrap();
        assert_eq!((a.cur, a.top), (0, 0));
    }

    #[test]
    fn the_line_width_keys_step_within_the_limits() {
        let (_d, mut a) = app(&(0..200u8).collect::<Vec<_>>());
        a.mode = Mode::Hex;
        a.bpl = 16;
        a.fit_bpl = 16;

        a.resize_line(false);
        assert_eq!(a.fixed_bpl, Some(12), "hex steps in fours");
        a.bpl = 12;
        a.resize_line(false);
        a.bpl = 8;
        a.resize_line(false);
        a.bpl = 4;
        // Four bytes is as narrow as a hex dump goes.
        a.resize_line(false);
        assert_eq!(a.fixed_bpl, Some(4));

        // And it never grows past what the window can hold.
        a.bpl = 16;
        a.resize_line(true);
        assert_eq!(a.fixed_bpl, Some(16));
    }

    #[test]
    fn the_line_width_keys_leave_text_mode_alone() {
        let (_d, mut a) = app(&(0..200u8).collect::<Vec<_>>());
        a.mode = Mode::Text;
        a.resize_line(false);
        assert_eq!(a.fixed_bpl, None);
        assert!(a.status.contains("byte modes"));
    }

    #[test]
    fn sliding_the_grid_moves_every_line_together() {
        let (_d, mut a) = app(&(0..64u8).collect::<Vec<_>>());
        a.shift_grid(true).unwrap();
        assert_eq!(a.top, 1, "the whole grid is re-anchored");
        assert_eq!(a.align, 1);
        // Every row boundary moved with it, not just the first line's.
        assert_eq!(row(&mut a, 1), (1, 8));
        assert_eq!(row(&mut a, 9), (9, 8));
        // The bytes below the new anchor are still a row of their own.
        assert_eq!(row(&mut a, 0), (0, 1));
        a.shift_grid(false).unwrap();
        assert_eq!((a.top, a.align), (0, 0));
    }

    #[test]
    fn the_grid_stays_slid_even_when_the_file_fits_on_one_screen() {
        // 4 rows of 8 hold 32 bytes, so this file needs no scrolling at all —
        // and the clamp that keeps the view on the last full screen must not
        // undo the slide because of it.
        let (_d, mut a) = app(&(0..12u8).collect::<Vec<_>>());
        a.shift_grid(true).unwrap();
        a.shift_grid(true).unwrap();
        assert_eq!(a.align, 2, "the anchor moved and stayed moved");
        // Every row boundary moved with it...
        assert_eq!(row(&mut a, 2), (2, 8));
        assert_eq!(row(&mut a, 10), (10, 2));
        // ...and the bytes below the anchor are a short row of their own
        // rather than bytes the view can no longer reach.
        assert_eq!(row(&mut a, 0), (0, 2));
        a.shift_grid(false).unwrap();
        assert_eq!(a.align, 1);
    }

    #[test]
    fn a_record_is_one_row_when_wrapping_is_off() {
        // Three records of 10, 6 and 4 bytes.
        let data = b"RECabcdefgRECabcRECa";
        let (_d, mut a) = rec_app(data, "REC");
        assert_eq!(row(&mut a, 0), (0, 10));
        assert_eq!(row(&mut a, 9), (0, 10));
        assert_eq!(row(&mut a, 10), (10, 6));
        assert_eq!(row(&mut a, 16), (16, 4));
    }

    #[test]
    fn a_long_record_is_cut_and_the_cursor_scrolls_it_sideways() {
        let data = b"RECabcdefgRECabc";
        let (_d, mut a) = rec_app(data, "REC");
        // bpl is 8, the first record is 10 bytes: two bytes hang off the edge.
        let r = a.row_at(0).unwrap();
        let line = a.clip(r);
        assert_eq!((line.off, line.len, line.hidden), (0, 8, 2));

        // Walking the cursor past the edge brings the tail into view.
        a.go_to(9).unwrap();
        assert_eq!(a.hoff, 2, "scrolled sideways by two bytes");
        let r = a.row_at(9).unwrap();
        let line = a.clip(r);
        assert_eq!((line.off, line.len, line.skipped, line.hidden), (2, 8, 2, 0));

        // And back: the left edge follows the cursor home again.
        a.go_row_edge(false).unwrap();
        assert_eq!((a.cur, a.hoff), (0, 0));
    }

    #[test]
    fn wrapping_spreads_a_record_over_as_many_rows_as_it_needs() {
        let data = b"RECabcdefgRECabc";
        let (_d, mut a) = rec_app(data, "REC");
        a.toggle_wrap();
        assert!(a.wrap);
        // The 10-byte record becomes an 8-byte row plus a 2-byte row.
        assert_eq!(row(&mut a, 0), (0, 8));
        assert_eq!(row(&mut a, 8), (8, 2));
        // The next record starts a fresh row rather than filling this one.
        assert_eq!(row(&mut a, 10), (10, 6));

        let rows = a.layout_rows(4).unwrap();
        let shape: Vec<(u64, u64)> = rows.iter().map(|r| (r.start, r.len)).collect();
        assert_eq!(shape, vec![(0, 8), (8, 2), (10, 6)]);
        // Nothing is cut off the right edge when records wrap.
        assert!(rows.iter().all(|&r| a.clip(r).hidden == 0));
    }

    #[test]
    fn wrapped_rows_step_through_a_record_and_on_to_the_next() {
        let data = b"RECabcdefgRECabc";
        let (_d, mut a) = rec_app(data, "REC");
        a.toggle_wrap();
        a.go_to(0).unwrap();
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 8, "second row of the same record");
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 10, "first row of the next record");
        a.step_rows(1, false).unwrap();
        assert_eq!(a.cur, 8);
    }

    #[test]
    fn record_rows_keep_the_column_and_clamp_to_short_records() {
        // Records of 10, 4 and 10 bytes.
        let data = b"RECabcdefgRECaRECabcdefg";
        let (_d, mut a) = rec_app(data, "REC");
        a.go_to(9).unwrap(); // column 9 of the first record
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 13, "the second record is only 4 bytes: last byte");
        // The column is read off the row the cursor is on at each step, so
        // passing through a short record carries its column onward rather than
        // restoring the one from two rows up.
        a.step_rows(1, true).unwrap();
        assert_eq!(a.cur, 14 + 3);
    }

    fn needle(a: &mut App, text: &str) {
        a.needle = Some(Pattern::parse(text, Encoding::Ascii).unwrap());
    }

    #[test]
    fn the_bar_only_offers_keys_that_would_do_something() {
        let (_d, mut a) = app(b"REC-one-REC-two");
        let labels = |a: &App| -> Vec<&str> { a.bar_slots().iter().map(|&(_, l)| l).collect() };

        // At startup there is nothing to step to and nothing to wrap.
        let start = labels(&a);
        assert!(!start.contains(&"Next"), "nothing has been searched for yet");
        assert!(!start.contains(&"Prev"));
        assert!(!start.contains(&"Wrap") && !start.contains(&"Cut"));
        assert!(start.contains(&"Find"));

        a.set_format(Pattern::parse("REC", Encoding::Ascii).unwrap()).unwrap();
        assert!(labels(&a).contains(&"Cut"), "now wrapping means something");

        needle(&mut a, "one");
        let searching = labels(&a);
        assert!(searching.contains(&"Next"));
        assert!(searching.contains(&"Prev"));
        assert!(searching.contains(&"Clear"));
        assert!(!searching.contains(&"Bin"), "a live search owns the bar");
    }

    #[test]
    fn search_walks_matches_both_ways_and_wraps_round() {
        let (_d, mut a) = app(b"--AB--AB--AB--");
        needle(&mut a, "AB");

        a.search_step(true).unwrap();
        assert_eq!(a.cur, 2);
        a.search_step(true).unwrap();
        assert_eq!(a.cur, 6);
        a.search_step(false).unwrap();
        assert_eq!(a.cur, 2, "back to the one before");

        // Off the front, round to the last.
        a.search_step(false).unwrap();
        assert_eq!(a.cur, 10);
        assert!(a.status.contains("Wrapped to the end"));

        // And off the back, round to the first.
        a.search_step(true).unwrap();
        assert_eq!(a.cur, 2);
        assert!(a.status.contains("Wrapped to the start"));
    }

    #[test]
    fn stepping_matches_needs_a_search_first() {
        let (_d, mut a) = app(b"--AB--");
        a.search_step(true).unwrap();
        assert_eq!(a.cur, 0);
        assert!(a.status.contains("Nothing to search for"));
    }

    #[test]
    fn escape_backs_out_of_a_search_before_it_quits() {
        let (_d, mut a) = app(b"--AB--");
        needle(&mut a, "AB");
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert!(!a.on_key(esc).unwrap(), "the first Escape clears the search");
        assert!(a.needle.is_none());
        assert!(a.on_key(esc).unwrap(), "with nothing to clear, it quits");
    }

    #[test]
    fn a_format_pattern_that_is_nowhere_is_refused() {
        // What typing 0A instead of x:0A does: a perfectly good text pattern
        // that simply is not in the file. Accepting it would put the whole
        // file on one line, which reads as the viewer ignoring you.
        let (_d, mut a) = app(b"REC-one\nREC-two\n");
        a.set_format(Pattern::parse("0A", Encoding::Ascii).unwrap()).unwrap();
        assert!(a.record.is_none(), "not accepted");
        assert!(a.status.contains("nowhere in the file"));
        assert!(a.status.contains("x:0A"), "and it says what to type instead");

        // The same bytes asked for as bytes do exist.
        a.set_format(Pattern::parse("x:0A", Encoding::Ascii).unwrap()).unwrap();
        assert!(a.record.is_some());
    }

    #[test]
    fn a_new_format_drops_a_leftover_search_but_not_the_other_way_round() {
        let (_d, mut a) = app(b"REC-one-REC-two");
        needle(&mut a, "one");

        a.set_format(Pattern::parse("REC", Encoding::Ascii).unwrap()).unwrap();
        assert!(a.needle.is_none(), "nothing left highlighted from before");
        assert!(a.record.is_some());

        // A search inside a formatted view keeps the formatting: you are
        // looking for something within the shape you set up.
        needle(&mut a, "two");
        assert!(a.record.is_some());
    }

    #[test]
    fn setting_a_format_lands_on_a_record_boundary() {
        let (_d, mut a) = app(b"..REC-one-REC-two");
        a.go_to(14).unwrap();
        a.set_format(Pattern::parse("REC", Encoding::Ascii).unwrap()).unwrap();
        assert_eq!(a.top, 10, "the record the cursor was in, not mid-record");
    }

    #[test]
    fn the_code_page_list_opens_on_the_current_page_and_takes_a_pick() {
        let (_d, mut a) = app(b"whatever");
        a.width = 90;
        a.rows = 22;
        a.set_encoding(Encoding::Cp1251);
        a.open_picker();
        assert!(matches!(a.input, Input::Pick(i) if i == Encoding::Cp1251.index()));

        // Down the column, then across to the other one, then take it.
        a.on_key_pick(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        a.on_key_pick(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let want = Encoding::ALL[Encoding::Cp1251.index() + 1 + picker_rows()];
        a.on_key_pick(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(a.input, Input::Normal));
        assert_eq!(a.enc, want);
    }

    #[test]
    fn escaping_the_code_page_list_changes_nothing() {
        let (_d, mut a) = app(b"whatever");
        a.width = 90;
        a.rows = 22;
        a.open_picker();
        a.on_key_pick(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        a.on_key_pick(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(a.input, Input::Normal));
        assert_eq!(a.enc, Encoding::Ascii);
    }

    #[test]
    fn a_window_too_short_for_the_list_says_so_instead_of_drawing_rubbish() {
        let (_d, mut a) = app(b"whatever");
        a.width = 90;
        a.rows = 4;
        a.open_picker();
        assert!(matches!(a.input, Input::Normal));
        assert!(a.status.contains("too small"));
    }

    #[test]
    fn the_cursor_survives_the_file_being_truncated() {
        let dir = Dir::new();
        let path = dir.file("shrink.bin", &(0..64u8).collect::<Vec<_>>());
        let mut a = App::new(path.clone()).unwrap();
        a.rows = 4;
        a.bpl = 8;
        a.go_to(60).unwrap();

        std::fs::write(&path, b"tiny").unwrap();
        assert!(a.reload(true).unwrap());
        assert_eq!(a.reader.len(), 4);
        assert_eq!(a.cur, 3, "cursor pulled back inside the file");
        assert_eq!(a.top, 0);
    }
}
