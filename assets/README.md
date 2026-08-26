# Icons

One mark: the letter M and a downward arrow — markdown's own — on Redstone
Logic's accent red.

| file | used by |
|---|---|
| `icon.svg` | the source, and what Linux installs (a desktop asks for arbitrary sizes, so scalable is the only answer right at all of them) |
| `icon.icns` | the macOS bundle's `Contents/Resources` |
| `icon.ico` | Windows, written beside the file association and named by `DefaultIcon` |
| `icon.png` | 512px, for the README |

## Why the small sizes are drawn by hand

`icon.svg` scaled down to 16px puts a 6-unit stroke on 1.5 pixels. The M and the
arrow anti-alias into each other, the arrowhead disappears, and what is left is a
smudge. An icon is read at 16px in a taskbar far more often than at 512px
anywhere, so those sizes get hand-placed pixels instead — and 16px drops the
arrow entirely, because two glyphs will not fit legibly in sixteen pixels and
showing less on purpose beats showing mush.

## Rebuilding them

```sh
python3 assets/make-icons.py
```

No dependencies and no drawing program: it rasterises the shapes directly. The
generated files are committed because the build must not need Python, and
because an icon that changes when you rebuild it is an icon nobody can review.
