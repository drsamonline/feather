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

## CBR: in-process, through the UnRAR decoder

**Status:** implemented as `feather-image::extract_rar` using `unrar-ng`.

**Correction, recorded because it was wrong the first time.** I wrote earlier
that adding a RAR decoder would "relicense the whole application". That is true
of exactly one crate and false of the others, and the difference matters:

| Route | Licence | Effect on Feather |
|---|---|---|
| `unrar-rs` (pure Rust) | GPL-3.0 | Would force the whole app to GPL-3.0 |
| `unrar-ng-sys` / `unrar-sys` | MIT/Apache bindings around RARLAB's C++ | **No copyleft.** The C++ is freeware: usable in any software, distributable inside it, with one restriction — it may not be used to build a RAR archiver |
| External `unrar` / `7z` / `bsdtar` | none | Nothing enters our tree at all |

HatchZip (MIT OR Apache-2.0, ~2.5k stars) ships the second route: it depends on
`unrar-ng-sys`, pins it because a new version brings new vendored C++, and
carries `LICENSE-UNRAR` alongside its own. So an MIT/Apache application can
include the RARLAB decoder without changing its own licence.

**Decision: taken, in-process.** The external-tool version worked but made a
portable single binary depend on something the reader had to install, which is
the opposite of the product. The cost is paid openly: `feather-image` now
compiles C++ through `cc`, so the workspace is no longer pure Rust and the
"no C toolchain" claim is retired; the binary carries a non-OSI licence, shipped
as `LICENSE-UNRAR` in every archive.

**The trade, stated plainly.** In-process extraction means CBR opens with no
helper installed — one fewer thing for a reader to get wrong. The cost is a C++
build dependency, a slightly larger binary, and a licence notice we did not have
before. That is the owner's call, not a bug fix.

Verified against a real 25 MB RAR4 comic (68 JPEGs, method 0x33 — normal
compression, so it genuinely needs a decompressor and cannot be read by parsing
headers alone). That archive also showed why the extracted pages are sorted by
file name rather than by path: RARs written on Windows store
`folder\\page01.jpg`, and an extractor on Linux may keep those backslashes.

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
