<div align="center">

<img src="docs/assets/feather.svg" alt="Feather" width="84"/>

# Feather

**A read-only document viewer that opens before you blink.**

Small, portable, cross-platform, summoned by a keystroke. No editing, no
annotation, no cloud, no plugins. One window, one document, nothing else.

[![CI](https://github.com/drsamonline/feather/actions/workflows/ci.yml/badge.svg)](https://github.com/drsamonline/feather/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust)](https://www.rust-lang.org/)

[The plan](docs/plan.html) · [Landing page](https://drsamonline.github.io/feather/) · [All projects](https://drsamonline.github.io/)

</div>

---

## Status

This repository is at **M0 — the spike**. The engine path is being proven before
any window code is written, because the plan's first question is not "does it
look good" but "how long does it take to open a document and draw page one".

| Piece | State |
|---|---|
| Format router (extension + magic bytes) | done, tested |
| `Document` trait — the contract every format implements | done |
| Byte-budgeted LRU tile cache | done, tested |
| PDF pipeline (hayro, pure Rust, Apache/MIT) | done — renders real pages |
| Image pipeline — one picture, one page | done |
| Comic pipeline — CBZ, natural page order | done |
| EPUB pipeline — reflowed text, its own layout engine | done, first cut |
| `feather` CLI — `info` and `render` | done — measures the budget |
| Scroll model — page geometry, visible range, zoom anchoring | done, tested |
| Window — open, fit to width, scroll, zoom, page | done — first light |
| Tile cache in the draw path, render thread, stale-work cancellation | done |
| Pages as a grid of tiles, with a raster ceiling for huge pages | done |
| Session model — tabs, pinning, dedupe, per-tab reading position | done, tested |
| Single instance — a second launch becomes a tab, not a window | done |
| Tab strip — labels, active chip, click to switch | done |
| Multiple windows | next |
| Desktop integration — .desktop, registry, MIME candidates | done (Linux, Windows) |
| Global shortcut — Ctrl+Alt+F summons the window | done |
| Tray icon | deferred — see below |
| EPUB, images, comics, Office read mode | later |
| Desktop integration — .desktop, registry, MIME candidates | done (Linux, Windows) |
| Global shortcut — Ctrl+Alt+F summons the window | done |
| Tray icon | deferred — see below |
| EPUB, images, comics, Office read mode | later |

## Try it

```sh
cargo build --release
./target/release/feather info  some.pdf
./target/release/feather render some.pdf 1 -s 2.0 -o page1.png
```

And the window itself:

```sh
cargo run --release -p feather-app -- some.pdf
```

| Key | Does |
|---|---|
| `↓` `↑` / `PgDn` `PgUp` / wheel | scroll |
| `←` `→` | previous / next page |
| `+` `-` | zoom around the centre of the screen |
| `1` | fit to width |
| `Home` `End` | first / last page |
| `]` `[` | next / previous tab |
| click a tab | switch to it |
| `Ctrl+Alt+F` (anywhere) | summon the window |
| drag over text | select it (EPUB today) |
| `c` | copy the selection |
| `p` | pin or unpin the current tab |
| `w` | close the current tab (refused if pinned) |
| `Esc` `q` | quit |

There is no toolbar. There is nothing on a toolbar.

The window never rasterises. A worker thread owns the document and draws pages
into a byte-budgeted cache; the window only blits what is already there, so a
400-page PDF scrolls without freezing and a fast flick drops the pages it has
already left behind instead of queueing them.

Text is selectable where a pipeline can say where the words are — drag to select,
`c` to copy. EPUB can; PDF cannot yet, because hayro hands back a page image and
extracting positioned words from it is the next piece of work. A viewer that
pretends to select in a PDF it cannot read would be worse than one that does not.

The tab strip is the only permanent chrome: one chip per document, the active
one lit, a pin marked with `*`. Labels are drawn with the system's own sans
face — fontdb finds it, fontdue rasterises it — so no font is embedded and the
binary does not grow. Latin only for now; a real shaper is its own block.

Open a second file while the viewer is running and it becomes a tab in the
running window rather than a second window — the first launch owns a loopback
port and later launches hand their path to it. Pinned tabs sort left and refuse
to close. Tiles are keyed by document as well as page, so two tabs of the same
PDF can never share a bitmap.

Pages are cut into a grid of 512-pixel tiles, so the cache can evict a corner
rather than a page. And a page whose raster would exceed 32 megapixels — a
20,000-pixel scan, a poster-sized vector drawing — is rendered at a lower rung
of the zoom ladder and stretched on screen: you lose sharpness, not the process.

`render` prints its own timings to stderr:

```
read 1.2 ms · open 0.4 ms · render 38.1 ms · total 39.7 ms
page 1 of 412 · 1190×1684 px at 2.00x · 214633 bytes png → page1.png
```

A one-page fixture lives at `tests/fixtures/minimal.pdf`, and CI renders it on
every push — so the first green build is proof that the engine works on Windows,
Linux and macOS, not a claim in a README.

Those four numbers are the project. §8 of the plan sets the ceiling: first page
of a 50 MB PDF in under 150 ms, binary under 20 MB, cold start under 300 ms. CI
prints the binary size on every push so the number cannot drift quietly.

## The shape of the code

```
crates/
  feather-core/   Document trait · format router · tile cache · scroll model
                  · session                                  (zero dependencies)
  feather-pdf/    PDF pipeline on hayro                          (pure Rust)
  feather-image/  still images, and CBZ as an archive of them
  feather-text/   glyph rasterisation, shared by the window and the pipelines
  feather-epub/   reflowable text: strip, wrap, paginate
  feather-cli/    the spike: info + render, with timings
  feather-app/    the window: winit + softbuffer + two font crates
```

`feather-core` has **zero dependencies** and does not know what a pixel looks
like on screen. The UI will hold a `Box<dyn Document>` and never match on a file
extension again — that single rule is what stops a fourteen-format viewer from
becoming fourteen special cases in the window code.

Adding a format means one enum variant, one router arm, one `Document` impl, and
one line in the `open_document` factory. Nothing else in the tree changes —
`feather-image` was added to prove it, and the window did not learn what an image
is.

## Why hayro and not MuPDF

MuPDF would give more formats in one binary, but it is AGPL — the only such
dependency in this estate, and awkward beside a licence whose point is a
retained attribution. hayro is Apache/MIT, pure Rust, read-only by design, and
cross-compiles to Windows ARM64 without a second machine. The cost is that EPUB
reflow is ours to write. §12 of the plan argues this properly.

## Summoning

`Ctrl+Alt+F` from anywhere brings the window up — un-minimised, focused, on top.
That is the honest version of "open the document selected in the file manager":
no desktop exposes the shell's selection to a third party, so the shortcut
raises the viewer and the path can be pasted or dropped into it. On Wayland the
registration fails and Feather says so once, rather than silently doing nothing.

**The tray icon is deferred, on purpose.** `tray-icon` needs GTK on Linux, which
would pull a few hundred megabytes of development libraries into a build that
currently needs none — for a status icon. The right Linux answer is a
StatusNotifierItem over D-Bus (`ksni`), which is a self-contained piece of work
rather than a dependency swap, and it is not in this milestone.

## Registering with the desktop

```sh
packaging/linux/install.sh          # ~/.local/bin + a .desktop entry
```

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\install.ps1
```

Both register Feather as a *candidate* for the formats it can open — PDF, EPUB,
CBZ/CBR and images — and neither seizes the default. Choosing the default is the
user's job, in their own settings, where they can also undo it. macOS is not
automated yet; [packaging/macos/README.md](packaging/macos/README.md) says what a
bundle would need and why notarisation is the real blocker.

## Read the plan first

[docs/plan.html](docs/plan.html) — sixteen sections: the engine decision, the
requirement ledger, the format matrix, Office and ODF in five honest tiers, the
ecosystem sweep, the crate picklist, the performance budget, the audit log of
what earlier drafts got wrong, and a triage of twenty reference projects.

## Licence

MIT. Copyright © 2026 Dr. Sohil Momin. The attribution notice above must be
retained in copies and substantial portions of the software.
