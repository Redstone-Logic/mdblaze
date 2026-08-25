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
- **No font system.** The faces are compiled in. Asking the OS what fonts exist
  is the most reliable way to lose the whole budget.
- **A lazy font reader.** The first version used an eager one and paid 36ms *per
  face* -- 100ms, 95% of startup, to build outlines for glyphs no document uses.
  Avoiding fontconfig and then parsing the fonts anyway is not a saving.
- **No GPU.** A graphics context costs more than everything else here put
  together, to draw static text.
- **No vault, no index, no workspace.** One file, opened.

## Use

```sh
mdedit README.md             # read it
mdedit --edit README.md      # open straight into editing
mdedit --timing file.md      # and say where the time went
mdedit --shot out.ppm f.md   # render one frame with no window at all
```

The document is always rendered. The block your caret is in shows its markdown
so you can change it, and the moment the caret leaves, that block renders again.
No mode to switch, no second pane: what you edit is what you are looking at.

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

Saving is a temp sibling plus a rename. `fs::write` truncates first, so a crash
or a full disk between the truncate and the write leaves the truncated file --
at the exact moment someone asked for their work to be kept.

## Limits, stated

- **DejaVu, whatever you have installed**, and no coverage for scripts the
  embedded faces lack -- CJK, Arabic, Devanagari render as missing glyphs. The
  fix is to embed more coverage, not to start asking the system.
- **Synthetic italics.** Sheared from the regular face rather than a true italic,
  because `fonts-dejavu-core` ships no oblique sans.
- **Tables are parsed but not laid out.** They currently render as their cell
  text run together, which looks broken because it is. The next thing to fix.
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
