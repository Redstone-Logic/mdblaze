# Icons

`md` in Archivo Black over a blaze, in Redstone Logic's crimson.

| file | used by |
|---|---|
| `icon-master.png` | **the source**, 1024px |
| `icon-master-small.png` | the same tile carrying only an `m`, for sizes under 32 |
| `icons/*.png` | Linux, one per `hicolor` size |
| `icon.icns` | the macOS bundle's `Contents/Resources` |
| `icon.ico` | Windows, named by the `DefaultIcon` registry value |
| `icon.png` | 512px, for the README |

## Why the source is raster

The mark is a rendered flame with type set over it. There is no vector original
to scale from, and tracing one would lose exactly the thing that makes it look
like anything — the soft edges and the glow. So the 1024px master IS the
artwork, and everything else is derived from it.

The blaze was generated locally (see `~/tools/README.md`), then hue-shifted
toward `#B63C35` so it belongs to the Redstone family rather than reading as a
generic fire app. The type is a real font composited on top, because a model
that can render fire cannot spell.

## Why there are two masters

Below 32 pixels `md` does not fit beside a flame: the letters close up and the
blaze becomes a smudge behind them. The small sizes use `icon-master-small.png`,
which carries a single `m` — the same reduction your eye makes squinting at the
full mark. Showing less on purpose beats showing mush.

This was decided by looking at the candidates **at 16 pixels**, not at a
magnified picture of 16 pixels. Those are different questions and only the first
one matters.

## Rebuilding

```sh
python3 assets/make-icons.py
```

Standard library only — no Pillow, no ImageMagick, no drawing program. It
resamples with a box filter over premultiplied pixels, which is what stops the
soft edges developing a dark halo. Takes about seven seconds.

The generated files are committed because the build must not need Python, and
because an icon that changes when you rebuild it is one nobody can review.
