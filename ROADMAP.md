# Roadmap

What is done, what is next, and what each thing is waiting on. The design
reasoning behind the decisions here lives in [docs/decisions.md](docs/decisions.md).

## v0.1.0 — released

A read-only viewer that opens PDF, EPUB, CBZ, CBR (with a helper) and images,
plus plain text and Markdown, in one window, from one 9 MB binary, on three
platforms.

- Tiled rendering with a zoom ladder and a 32-megapixel raster ceiling
- Render thread, stale-work cancellation, byte-budgeted tile cache
- Tabs, pinned tabs, per-tab reading position, session restore
- Text selection and copy, in-document search, drag and drop
- Global shortcut (`Ctrl+Alt+F`), single-instance handoff, second windows as
  separate processes
- Desktop registration on Linux and Windows — as a candidate, never seizing the
  default
- CI on three platforms: 50+ tests, dependency budget, binary-size gate,
  headless screenshots of the real interface
- Checksummed release archives for Linux x86-64, Windows x86-64, macOS arm64

Since v0.1.0 was cut: CBR opens in-process through the UnRAR decoder, so no
helper needs installing; the format router no longer mistakes a RAR header for
text; and the numbers quoted on the landing page are now checked in CI.

## v0.2 — text that is not Latin

**Shaping.** Replace `fontdue` with `skrifa` + `harfrust` + `unicode-bidi`
inside `feather-text`. Same public API, so no caller changes.

Today every non-Latin script comes out wrong: Devanagari clusters do not form,
Arabic does not join, Thai marks land in the wrong place. For a viewer aimed at
Indian documents this is the largest correctness gap in the product.

*Waiting on:* a compiler in the loop. The change touches rasterisation, shaping,
font fallback and glyph caching, and a blind attempt would burn several CI cycles
and could end in a half-migrated state.

*Done when:* a Devanagari sample renders correctly, the screenshot in CI shows it,
and Latin text is unchanged.

## v0.3 — selectable PDFs

**Text extraction.** Walk the content stream (ISO 32000-2 §7.8, §8–9), map glyph
ids through the font encoding, and emit the `TextLayer` the reflowable pipelines
already produce. Selection and search then work in PDF without anything above
`feather-pdf` changing.

*Waiting on:* the same compiler, plus a spike to confirm the glyph-to-character
path on a real font.

*Done when:* `feather text paper.pdf` prints the page text, and dragging over a
PDF selects the words under the cursor.

## v0.4 — the tray, done properly

StatusNotifierItem over D-Bus (`ksni`) on Linux, `tray-icon` on Windows and
macOS. Menu: Open, Summon, Quit. No GTK, on any platform.

*Waiting on:* nothing but time.

## v0.5 — windows in one process

Extract `session`, `views`, `selection`, `cursor`, `viewport`, `window` and
`surface` into a `WindowState`; `App` holds a list of them plus the shared cache
and renderer. Mechanical, but a refactor of every method above it.

*Done when:* two windows share one tile cache, one render thread and one hotkey.

## v0.2.1 — the things a desktop app needs

- **An Open dialog.** `o` should raise the system file picker. Needs `rfd`, whose
  Linux backend is either GTK or a D-Bus portal — a dependency choice worth
  making deliberately rather than guessing, since GTK would break the
  "no system libraries" property the packaging depends on.
- **No console window on Windows.** Done for this repo in the same commit as the
  zoom fix: `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`.
  The same one line applies to every other Windows GUI binary in the estate.

## Then, if it earns its place

- **Office read mode** — Tier 1 from the plan: parse to blocks, reflow, label it
  approximate. Never page-accurate.
- **EPUB images and light CSS** — currently text only.
- **Phrase search** — the matcher is word-level.
- **Dropbox** — browse and open a file from a linked Dropbox account. An OAuth
  app, a token store, and a network path in a viewer that has none of those; it
  is the first feature that would make "no cloud" untrue.
- **Multi-page selection** — needs a document-wide text order.
- **`.cbr` in the desktop MIME list** — one line, now that no helper is needed.
- **Lazy RAR extraction** — a comic is fully unpacked on open, so a 25 MB CBR
  costs ~1.5 s before page one appears. The fix is unrar's single-file API:
  open once, pull the page asked for.

## Housekeeping, not features

- Generate and commit `Cargo.lock`, then restore `--locked` in the release build.
- Code signing: a Windows certificate and an Apple Developer ID. Money, not code.
  Until then both platforms warn on first run.
- A pixel-diff baseline for the CI screenshots, if the size check proves too
  coarse.
- Keep the documentation honest automatically — the dependency count and the
  quoted binary size are already asserted against the landing page.

## Not planned

Printing, annotation, editing, cloud sync, plugins, telemetry, an updater, a
settings window, Office formats at page fidelity.

## What v1.0 would mean

Every format in the router selectable and searchable; complex scripts correct;
no deferred feature that a reader would notice; releases signed.
