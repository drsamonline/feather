# Decisions, and the evidence for them

Short notes on the choices that were not obvious, with the thing that settled
them. If a decision here is wrong, this is the file that should say why.

## Shaping: skrifa + harfrust + unicode-bidi

**Status:** decided, not implemented. Feather is Latin-only today.

`fontdue` rasterises glyphs but does not shape: it maps one character to one
glyph. Devanagari, Arabic and Thai need cluster shaping, which is a different
job. The answer is the stack PdfCraft uses in `pdfcraft-fonts`:

- **harfrust** — the Rust port of HarfBuzz, which does the actual shaping
- **skrifa** — font parsing, metrics and outlines
- **unicode-bidi** — bidirectional text ordering

All three are permissively licensed and in the same family as the rest of our
stack. This replaces the inside of `feather-text`; the public API — `measure`,
`wrap`, `elide`, `draw` — does not change, so no caller needs to know.

**Why this and not cosmic-text:** cosmic-text wraps the same three crates and
adds a layout cache we do not need, since our layout is already written. Take
the smaller dependency.

## PDF text extraction: walk the content stream

**Status:** not started.

hayro 0.9 renders pages but does not expose a glyph-to-character mapping, which
is why PDF is unselectable. PdfCraft does not use hayro for text either — its
`pdfcraft-content` crate parses content streams itself (ISO 32000-2 §7.8, §8–9)
on top of `hayro-syntax`, and `pdfcraft-render` mixes `hayro` for rasterising
with `lopdf` for structure.

That is the shape of the work: parse the text operators out of the content
stream, map glyph ids through the font's encoding with skrifa, and emit the same
`TextLayer` the reflowable pipelines already produce. Then selection and search
work in PDF without touching anything above `feather-pdf`.

## CBR: an external tool, not a dependency

**Status:** implemented — `feather-image::extract_rar`.

CBR is a RAR archive (CBZ is the zip one). The pure-Rust reader `unrar-rs` is
GPL-3.0-or-later and carries RARLAB's restriction that the source may not be
used to build a RAR archiver. Linking it would make Feather GPL-3.0.

The licence-clean route is the one the plan already uses for LibreOffice: shell
out to `unrar`, `7z` or `bsdtar` when one is installed, extract to a temporary
directory, and hand the images to the existing comic pipeline. No dependency, no
licence entanglement, and it degrades to a clear message when no helper exists.

## Tray icon: not yet, and not with GTK

**Status:** deferred.

`tray-icon` requires GTK on Linux, which is hundreds of megabytes of
development libraries for a status icon. The right Linux implementation is a
StatusNotifierItem over D-Bus (`ksni`); Windows and macOS would use `tray-icon`.
Two backends, one behaviour, and no GTK.

## In-process windows: a refactor, not a flag

**Status:** deferred. Multiple windows work today as separate processes.

The window state — session, views, selection, cursor, viewport, surface — is
owned by the application rather than by a window. Extracting it into a
`WindowState` and letting `App` hold a list of them is mechanical, but it is a
refactor of every method above it, not a feature flag.

## Preview handlers: not a substitute

**Status:** rejected as an approach.

QuickLook and PowerToys Peek host the shell's `IPreviewHandler` — a Windows-only
mechanism that renders whatever the installed Office (or other) handler draws.
Perfect fidelity, zero portability, and no control over zoom, selection or the
chrome. Useful as an escape hatch on Windows; not as a foundation.
