# mdedit

A markdown editor that renders, and opens instantly.

Nothing occupies the space between the two. A browser or a text editor shows you
the source. Obsidian shows you the document, but pays an Electron start and a
vault index before it can show you anything at all. Opening one file to read it
should cost neither.

## Measured

On the machine it was written on, a 900x760 window:

| | small document | 36 KB document |
|---|---|---|
| read the file | 0.05 ms | 0.08 ms |
| reference the fonts | 0.08 ms | 0.09 ms |
| parse | 0.05 ms | 0.8 ms |
| lay out | 0.03 ms | 4.5 ms |
| **our work** | **0.21 ms** | **5.5 ms** |
| first frame on screen | ~30 ms | ~38 ms |

Everything after "our work" is the windowing toolkit. `winit` costs about 35ms
before a frame is possible, most of it compiling a keyboard layout, loading
cursor themes and enumerating monitors -- none of which a document being read
needs. Raw X11 puts a window up in 0.2ms, so that 35ms is claimable, at the price
of a backend per platform.

## The shape, and why

Speed decides the architecture rather than being tuned for afterwards.

- **No document tree.** Parsing produces a flat list of blocks with a depth
  number, so laying out is one ordered walk.
- **No HTML.** The console renders markdown to HTML because a browser consumes it
  there. Here the consumer is a rasteriser.
- **No font system.** The text faces are compiled in. Asking the OS what fonts
  exist is the most reliable way to lose the whole budget. The one exception is
  colour emoji: 10.8MB is too much to put in every copy of the binary for a
  feature most documents do not use, so the file is opened from a list of four
  absolute paths -- a list, not a scan -- and only once a document turns out to
  contain an emoji. It is mapped rather than read, so an emoji costs the two
  pages its own picture is on.
- **A lazy font reader.** The first version used an eager one and paid 36ms *per
  face* -- 100ms, 95% of startup, to build outlines for glyphs no document uses.
  Avoiding fontconfig and then parsing the fonts anyway is not a saving.
- **No GPU.** A graphics context costs more than everything else here put
  together, to draw static text.
- **No vault, no index, no workspace.** One file, opened.

## Use

```sh
mdedit --install-handler     # open .md by double-click
mdedit --uninstall-handler   # and give the association back

mdedit README.md             # read it
mdedit --edit README.md      # open straight into editing
mdedit --timing file.md      # and say where the time went
mdedit --shot out.ppm f.md   # render one frame with no window at all
```

The document is always rendered. The block your caret is in shows its markdown
so you can change it, and the moment the caret leaves, that block renders again.
No mode to switch, no second pane: what you edit is what you are looking at.

`--install-handler` writes a `.desktop` entry and makes it the default for
`text/markdown` and `text/x-markdown`. It records whatever it displaced and
`--uninstall-handler` puts that back: a tool that seizes a file association and
cannot give it back is one people are right to be wary of.

Click to put the caret where you clicked -- including in a block that is still
rendered, which reveals it. Arrow keys move the caret, the wheel and
PageUp/PageDown scroll without moving it, Home/End go to the ends of the line. `Ctrl+S` saves, `Ctrl+Z` undoes,
`Ctrl+Shift+Z` or `Ctrl+Y` redoes, Escape closes -- and asks once first if there
are unsaved changes. Tab inserts two spaces, because markdown's nesting is
defined in spaces and a literal tab renders differently in every tool that reads
the file next.

### Why live rendering is affordable here

Every keystroke re-parses and re-lays out the **whole document**: about 4ms for a
36KB file, a quarter of a frame at 60Hz. Incremental re-layout is where editors
of this kind get complicated and subtly wrong, and at this speed it buys nothing.

The map that makes it possible is that every block, span and run records the
source bytes it came from. A cursor is a position in the source and rendering
happens in blocks, so without that map the two coordinate systems never meet:
the caret can only live in a separate source pane, and a click can only answer
in screen terms.

Both directions are needed. Laying out turns a byte offset into a position on
screen; clicking turns a position on screen back into a byte offset. The second
is why runs carry provenance rather than just text -- the rendered text is not
the source, since `**bold**` renders as `bold`, so the mapping cannot be
arithmetic on what is displayed.

Closing with unsaved changes is refused once, on every route out -- Escape, the
title bar's close button, the window manager. A guard that only covers the way
you thought of is not a guard. The whole status bar turns red while the second
press would discard, and the bypass expires after a second and a half: a swift
second press is someone confirming, a slow one is someone who has moved on.

Saving is a temp sibling plus a rename. `fs::write` truncates first, so a crash
or a full disk between the truncate and the write leaves the truncated file --
at the exact moment someone asked for their work to be kept.

## Limits, stated

- **Noto Sans, whatever you have installed.** No coverage for scripts the
  embedded faces lack -- CJK, Arabic, Devanagari render as missing glyphs. The
  fix is to embed more coverage, not to start asking the system.
- **Pictures are local, and PNG or JPEG.** A markdown file arrives from
  anywhere, and one that says `![](https://...)` is asking this program to tell
  a stranger which files you open and when. Remote images are not fetched and
  there is no setting to fetch them; the alt text and the reason are shown
  instead. Everything else -- GIF, WebP, AVIF, SVG -- is a decoder's worth of
  code and attack surface for a format that does not turn up in a document about
  software.
- **Emoji sequences render as their base character.** A skin tone, a variation
  selector or a zero-width joiner needs the font's ligature substitutions
  applied, which means a shaping engine. A family comes out as its first member
  and a waving hand in the font's default yellow. In prose an emoji takes a
  fixed multiple of the type size; in a code fence, exactly two columns, which
  is what a terminal gives it.
- **A picture in a table cell shows its alt text.** A picture in a cell would
  need that row to be as tall as the picture, and the table is measured as a
  grid of type -- which is what makes the columns line up. Badge tables read as
  a list of what the badges say.
- **Mermaid diagrams render as box drawing**, not as pictures. The SVG renderers
  need a rasteriser, and the obvious one pulls 84 crates including `fontdb` --
  the font scanning this program exists without. A diagram that will not parse
  falls back to its source rather than vanishing.
- **Highlighting knows a short list of languages** -- Rust, JS/TS, Python, shell,
  Go, C-family, SQL, JSON, YAML/TOML. An unknown fence is shown plain rather than
  guessed at. It is a lexer, not a parser: it cannot tell a type from a variable,
  and it never alters the text, only its colour.
- **No settings, by choice.** The size, the measure and the leading are decided
  rather than exposed: a document reader that asks people to configure their
  typography has failed at the one job it has. 19px, about 66 characters a line,
  1.55 leading -- the middle of the band typographic practice settled on.
- **Prose and code get different measures**, because they are read differently:
  66 characters of prose, 79 columns of code (PEP 8's limit, and what most code
  is written to). Everything shares one left edge regardless -- blocks of
  different widths are fine, a left edge that moves between them is not.
- **The handler is Linux only.** `.desktop` files are a freedesktop convention;
  macOS declares document types in an app bundle and Windows in the registry.

- **Tables do not scroll sideways.** A table with many columns is scaled down
  until a minimum width, then overflows the measure rather than shrinking a
  column to nothing -- a column of no width shows nothing, which is worse than
  crowding.
- **No selection, no clipboard, no find.** The editing is a caret, characters and
  undo, and the mouse places the caret but does not drag a selection. Enough to
  fix a line; not yet enough to restructure a document.
- **Clicks inside emphasis are approximate.** A span's rendered text and its
  source are the same length for plain text and differ where markers were
  stripped, so a click inside `**bold**` can land a character or two out. Exact
  everywhere else, and exact in the revealed block, which is literal source.
- **The product now renders markdown twice** -- HTML in the console, this here --
  and the two can drift. Accepted because this opens arbitrary files rather than
  organisation content, so they never render the same document.
