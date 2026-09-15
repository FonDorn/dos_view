привет
# dosview

A DOS-style file viewer for the terminal: offset column on the left, data on
the right in one of three modes, decoded with one of six code pages. Blue
screen, cyan status bars, CP437 box drawing — the look of a DOS text mode
viewer, the behaviour of a modern one. Builds from the same source on Linux,
macOS and Windows; there is not a single platform `#[cfg]` in the code.

## Build

```
cargo build --release
./target/release/dosview file
```

Dependencies: `crossterm` (terminal) and `memchr` (search). The release binary
comes out around 500 KB.4

## Keys

| Key | Action |
|---|---|
| `1` | binary mode — every byte as eight bits |
| `2` | hex dump with a text column on the right |
| `3` | text mode; press it again to step to the next code page |
| `e` `E` | next / previous code page, from any mode |
| `[` `]` `\` | narrower / wider / window-wide data columns |
| `←` `→` `↑` `↓` | move the cursor |
| `Home` `End` | start / end of the file |
| `Cmd+←` `Cmd+→` | start / end of the line (`Ctrl` or `Alt` also work) |
| `PgUp` `PgDn` | page up and down |
| `Shift+←` `Shift+→` | slide the byte grid by one byte |
| `5` or `F5` | go to offset |
| `6` or `F6` | line = record: break lines on a pattern |
| `7` or `F7` | search |
| `8` or `F8` | next match |
| `9` or `F9` | wrap long records instead of cutting them |
| `h` | toggle match highlighting |
| `r` | reread the file now |
| `q`, `Esc` or `F10` | quit |

`g`, `/` and `n` also work for goto, find and next.

Cmd, Ctrl and Alt all do the same thing on the arrows because terminals cannot
agree on which of them an application is allowed to see. Cmd is the one a Mac
user reaches for and the hardest to actually get: Terminal.app and iTerm2 keep
it for their own menus, and it only arrives in terminals that speak the kitty
keyboard protocol and leave it alone — Ghostty, kitty, WezTerm. `Ctrl+←` and
`Ctrl+→` work everywhere.

## The cursor

A white block marks the byte you are on, in the byte column and in the text
column at once, and the title bar reads it out: the offset, the byte in hex and
in decimal, and the character it decodes to in the current code page.

Navigation moves the cursor and the view follows it. That is the whole point:
lines never slide out from under each other, the screen scrolls only once the
cursor would leave it, and then by one row. `PgUp` and `PgDn` move the view by
as many rows as the cursor, so the cursor keeps its place on the screen instead
of sliding to an edge.

Every field of the readout is a fixed width, code page name included. One that
changed length as the cursor moved would shove the rest of the bar back and
forth while you were trying to read it.

`Home` and `End` take the whole file. The ends of a line are on the modified
arrows, and in the record modes "the line" is the record — so `Cmd+→` on a
record that runs off the screen walks the cursor to its last byte and brings
the tail into view with it.

`Shift+←` and `Shift+→` are the one movement that is not the cursor's: they
re-anchor the byte grid, so a structure that does not start on a line boundary
can be lined up. Every line moves together, by the same byte:

```
00000000: 00 01 4D 5A 90 00 03 00                    00000002: 4D 5A 90 00 03 00 04 00
00000008: 04 00 00 00 FF FF 00 00   ── Shift+→ ──▶   0000000A: 00 00 FF FF 00 00 B8 00
00000010: B8 00 00 00 00 00 00 00                    00000012: 00 00 00 00 00 00 00 00
```

## Code pages

`3` opens text mode, and every press after that steps to the next code page;
`e` and `E` step forward and back from any mode. The choice drives both the
text column of the byte modes and the full-screen text mode, so a hex dump of
a Russian file reads as Cyrillic instead of dots:

```
00000000: CF F0 E8 E2 E5 F2 2C 20 EC E8 F0 21 20 48 65 6C  Привет, мир! Hel
```

Nineteen of them, in this order:

| Code page | For |
|---|---|
| `ASCII` | the classic hex dump: printable as-is, everything else a dot |
| `CP437` | the original IBM PC palette — ☺☻♥♦♣♠ and box drawing, all 256 bytes |
| `CP850` | DOS Western Europe |
| `CP852` | DOS Central Europe |
| `CP866` | DOS Cyrillic |
| `CP1250` | Windows Central Europe |
| `CP1251` | Windows Cyrillic |
| `CP1252` | Windows Western Europe |
| `ISO-8859-1` | Latin-1 |
| `ISO-8859-2` | Latin-2, Central Europe |
| `ISO-8859-5` | ISO Cyrillic |
| `ISO-8859-15` | Latin-9: Latin-1 with the euro sign |
| `KOI8-R` | Cyrillic in older Unix files and mail |
| `KOI8-U` | the Ukrainian variant |
| `MacRoman` | classic Mac OS Western |
| `MacCyrillic` | classic Mac OS Cyrillic |
| `UTF-8` | decoded per character |
| `UTF-16LE` | Windows text files, PE resources |
| `UTF-16BE` | the other byte order |

Worth knowing, because mislabelled text usually turns out to be one of these:
`CP1252` and `ISO-8859-1` differ only in 0x80..0x9F, where Windows put the
smart quotes and dashes that ISO left as control codes. A great deal of what
claims to be Latin-1 is really CP1252, and `3` twice tells you which.

The tables are generated from the reference mappings rather than typed, and
adding another code page is one more generated table.

The one thing the whole layout rests on is **one byte, one cell**: the text
column has to line up with the byte column, and every cell has to map back to
a file offset. Single-byte code pages give that for free. The Unicode ones do
not, so a multi-byte character is drawn on the cell of its leading byte and its
continuation bytes get `·`:

```
00000000: D0 9F D1 80 D0 B8 D0 B2 D0 B5 D1 82 2C 20 D0 BC  П·р·и·в·е·т·, м·
```

Cyrillic text ends up twice as wide as it reads, which is the price of keeping
cells addressable. Bytes that are not valid UTF-8 show as `.` and decoding
resyncs on the next byte rather than guessing. East Asian characters are two
columns wide and would shift the grid, so they are refused the same way — the
bytes are still there in the dump, the viewer just does not pretend to draw
them.

UTF-16 takes its alignment from the file offset, not from wherever a line
happened to start reading: the same four bytes one byte further along pair up
differently and mean something else, and a line that began mid-character says
so with a `·` in the first cell. A surrogate pair is one character over four
bytes, and is read as one rather than as four dots.

## The width of the data columns

By default a line holds as many bytes as the window can fit. `[` and `]` make
it fewer or more — 4 at a time in hex, 1 in binary — and `\` goes back to
filling the window. Both columns show the same bytes, so this is the one dial
there is: fewer bytes per line and the dump, text column and all, takes up less
of the window. The mode's own limits are the stops: never below 4 bytes in hex
or 1 in binary, never wider than the window can hold.

## Patterns

A pattern is written the same way in search (`7`) and in record splitting
(`6`): text as-is (`REC`) or `x:` followed by hex pairs (`x:89 50 4E 47` — the
PNG signature). In the hex form `??` means any byte: `x:89 ?? 4E 47`.

Text is encoded with the **current code page**, so searching for `Привет`
while viewing a CP866 file looks for CP866 bytes, not UTF-8 ones — the same
query hunts for different bytes depending on the view, which is what you
want. In a UTF-16 view the query becomes UTF-16 bytes, which is what it takes
to find a string in a Windows binary at all.

Switch the code page and the pattern is re-encoded; if the text does not exist
in the new one (`Привет` in CP437, say) the old bytes are kept and the status
line says so.

Offsets for `5` are accepted in decimal (`1024`) or hex (`0x4D5A`, `$4D5A`).

## Line = record

An ordinary dump cuts the file on a rigid grid, and variable-length records
fall apart in it. `6` takes a record-start pattern, and from then on a line
breaks at every occurrence: each record sits on its own line and line length
becomes variable.

```
00000000: 52 45 43 0C 41 42 43 44 45 46 47 48                          REC.ABCDEFGH
0000000C: 52 45 43 14 41 42 43 44 45 46 47 48 49 4A 4B 4C 4D 4E 4F 50  REC.ABCDEFGHIJKLMNOP
00000020: 52 45 43 09 41 42 43 44 45                                   REC.ABCDE
00000029: 52 45 43 1A 41 42 43 44 45 46 47 48 49 4A 4B 4C 4D 4E 4F 50 ›REC.ABCDEFGHIJKLMNOP
```

`↑`, `↓`, `PgUp` and `PgDn` now move by records rather than by the grid — a
step down is a search for the next occurrence, a step up a search backwards.
That is what `find_backward` in `reader.rs` is for.

Records longer than the window get one of two treatments, switched with `9`.
The key is only on the bottom bar while a record pattern is set, since that is
the only time it changes anything:

**Cut** (the default) keeps one line per record. A red `›` where the separator
goes means the record is only shown in part; walk the cursor right and the
whole screen scrolls sideways with it, a red `‹` in the offset column marking
what went off the left. `Cmd+→` takes the cursor — and the view — to the end
of the record in one step. `Shift+←` and `Shift+→` scroll sideways without
moving the cursor.

**Wrap** gives a long record as many lines as it needs, so all of it is on
screen at once and nothing is ever cut:

```
       cut                                        wrap
0000000A:  a very long line that will not f›  0000000A:  a very long line that will not f
000000BE:  another short one                 0000005A:  it into the window at all: ABCDE
000000D0:  tail                              000000BE:  another short one
                                             000000D0:  tail
```

Records and text lines are the same thing, which makes this a text viewer too:
set the record pattern to `x:0A` and every line of the file becomes a line on
screen, with `9` switching between wrapping long lines and scrolling them
sideways.

Every occurrence of the search pattern is highlighted in yellow, in the byte
column and in the text column alike. `h` turns it off.

## Following a file that changes

The file is re-examined whenever the keyboard goes quiet — length and
modification time, about twice a second — and reread when either moved, which
is how a log being written to stays current without being reopened. `r` checks
on demand.

A file that changed is picked up even when an editor saved it by writing a new
file and renaming it over the old one: the reread goes through the path again,
not through the descriptor we already hold, which would still be pointing at
the replaced inode. If the file shrank, the cursor is pulled back inside it.

## How it works

**`reader.rs`** — the reading window. The file is never read whole: one 1 MiB
buffer lives in memory and travels with the view position. Opening costs one
`open` plus one `metadata`, so open time does not depend on file size — a 50 GB
file appears as fast as a 4 KB one.

Search streams in megabyte blocks overlapping by `len(needle) - 1` bytes, so a
match on a block seam is not lost and memory does not grow.

**`pattern.rs`** — the pattern with wildcards. Wildcards break fast search:
memmem can only look for an exact sequence. So the longest solid run of known
bytes inside the pattern is picked, the file is sieved with it at full memmem
speed, and the full match is only checked at the points that turns up. For
`x:89 ?? 4E 47 AA` the anchor is `4E 47 AA`, not the lone `89`.

**`encoding.rs`** — the code page tables and the byte-to-cell decoding, plus
the reverse direction for encoding a typed query into file bytes.

**`main.rs`** — state, drawing, the event loop, and the layout geometry. All
of it comes off one rule: a row is a run of bytes, and the row after a row
starts where it ends. The three layouts — the byte grid, one record per line,
a record wrapped over several — differ only in what a row is, so the cursor,
the scrolling and the paging are written once and work in all of them.

**`view.rs`** — formatting and how wide a line can be. All three modes are the
same bytes formatted differently; only the number of cells per byte differs,
and from it follows how many bytes fit on a line, and how narrow the line
width keys are allowed to make it. The offset column widens from 8 to 12
digits by itself on files over 4 GiB.

Drawing is virtualised: a frame reads exactly as many bytes as are visible
(usually under a kilobyte) in one call. Scrolling a terabyte file therefore
costs exactly what scrolling a kilobyte one does.

It draws straight through `crossterm`, without ratatui: the screen here is a
rigid grid of monospace cells, and a layout engine buys nothing on it.

## Details these programs usually trip over

All offsets are `u64`, with no `usize` anywhere in position arithmetic. On a
32-bit build `usize` would break at two gigabytes.

One `read` is not obliged to return everything asked for. `read_full` loops for
the remainder and handles `Interrupted` — without that you get floating "bytes
go missing sometimes" bugs.

A UTF-8 character can straddle a line break, so each line is decoded with a few
bytes of context on either side. Without the lead-in the first letter of every
line would decode as garbage; without the lookahead the last one would.

The grid anchor is state of its own, not `top % bpl`. Deriving it looked
tempting and was wrong: `top` gets clamped, snapped and scrolled for all sorts
of reasons, and each of those quietly threw the slide away — visibly so on a
file small enough to fit on one screen, where `Shift+→` appeared to do nothing
at all.

The kitty keyboard protocol is asked for blind. crossterm can query whether
the terminal supports it, but querying means writing a question and waiting up
to two seconds for an answer that the terminals without it never send — and a
viewer that opens a 50 GB file instantly has no business stalling two seconds
on a keyboard question. Terminals that do not know the sequence swallow it.

The event loop waits with a timeout instead of blocking on a key. That is what
makes watching the file possible, and a frame is only redrawn when something
actually moved — an idle viewer sends nothing to the terminal.

On Windows `crossterm` delivers events for both key press and key release.
Without the `KeyEventKind::Press` filter every keystroke would fire twice — it
is invisible on Linux, and the bug only surfaces for users.

A `panic hook` is installed that puts the terminal back to normal. Otherwise a
panic would leave the user with a dead console in raw mode.

Matches do not overlap: after one is found the search continues past its end.
Otherwise the pattern `AA` over a run of identical bytes would match on every
byte, and record splitting would degenerate into one-byte lines.

A line is coloured in runs: neighbouring bytes with the same highlight state
are merged into one write, so we do not spray escape sequences per byte.

Every line is padded with spaces to the edge of the screen instead of relying
on `Clear`: that way the background is solid in every terminal and no leftovers
from the previous frame show through.

## Tests

```
cargo test
```

71 tests.

*The reader:* window buffer moves across boundaries, reads past the end of the
file, a match landing exactly on a block seam searched forwards and backwards,
a file appended to, truncated, and replaced by rename underneath a live window.

*Patterns:* anchor choice with wildcards, a wildcard at the start (a match
"from offset −1" has to be dropped, not underflow), non-overlapping matches,
text encoded through each code page.

*Encodings:* one cell per byte for all 256 values of every code page at both
offset parities, every Cyrillic page decoding its own bytes, the Western pages
differing where they should, UTF-8 resyncing after invalid bytes, UTF-16 byte
orders and surrogate pairs, wide characters refused, and a query round-tripping
through each page.

*Layout and navigation:* layout fitting the terminal width at any chosen line
width, formatting in every mode, rows in all three layouts, the view holding
still while the cursor moves inside it, scrolling by exactly one row when it
leaves, a page moving both so the cursor keeps its screen row, the grid slide
surviving a file that fits on one screen, records cut and scrolled sideways
versus wrapped, the line width keys stopping at their limits, and the cursor
surviving the file shrinking under it.

## What could come next

Backward search on `Shift+F7`, selecting and copying a byte range, UTF-16
decoding, an edit mode — same-length edits only need a write at an offset,
while insertion and deletion would want a piece table.
