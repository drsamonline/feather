<div align="center">

<img src="docs/assets/feather.svg" alt="Feather" width="84"/>

# Feather

**A read-only document viewer that opens before you blink.**

Small, portable, cross-platform, summoned by a keystroke. No editing, no
annotation, no cloud, no plugins. One window, one document, nothing else.

[![CI](https://github.com/drsamonline/feather/actions/workflows/ci.yml/badge.svg)](https://github.com/drsamonline/feather/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/built%20with-Rust-orange?logo=rust)](https://www.rust-lang.org/)

[The plan](docs/plan.html) · [All projects](https://drsamonline.github.io/)

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
| `feather` CLI — `info` and `render` | done — measures the budget |
| Window, scrolling, zoom, tabs | next |
| Shell integration, tray, hotkey | later (lifted from CLDR) |
| EPUB, images, comics, Office read mode | later |

## Try it

```sh
cargo build --release
./target/release/feather info  some.pdf
./target/release/feather render some.pdf 1 -s 2.0 -o page1.png
```

`render` prints its own timings to stderr:

```
read 1.2 ms · open 0.4 ms · render 38.1 ms · total 39.7 ms
page 1 of 412 · 1190×1684 px at 2.00x · 214633 bytes png → page1.png
```

Those four numbers are the project. §8 of the plan sets the ceiling: first page
of a 50 MB PDF in under 150 ms, binary under 20 MB, cold start under 300 ms. CI
prints the binary size on every push so the number cannot drift quietly.

## The shape of the code

```
crates/
  feather-core/   Document trait · format router · tile cache   (no dependencies)
  feather-pdf/    PDF pipeline on hayro                          (pure Rust)
  feather-cli/    the spike: info + render, with timings
```

`feather-core` has **zero dependencies** and does not know what a pixel looks
like on screen. The UI will hold a `Box<dyn Document>` and never match on a file
extension again — that single rule is what stops a fourteen-format viewer from
becoming fourteen special cases in the window code.

Adding a format means one enum variant, one router arm, one `Document` impl.
Nothing else in the tree changes.

## Why hayro and not MuPDF

MuPDF would give more formats in one binary, but it is AGPL — the only such
dependency in this estate, and awkward beside a licence whose point is a
retained attribution. hayro is Apache/MIT, pure Rust, read-only by design, and
cross-compiles to Windows ARM64 without a second machine. The cost is that EPUB
reflow is ours to write. §12 of the plan argues this properly.

## Read the plan first

[docs/plan.html](docs/plan.html) — sixteen sections: the engine decision, the
requirement ledger, the format matrix, Office and ODF in five honest tiers, the
ecosystem sweep, the crate picklist, the performance budget, the audit log of
what earlier drafts got wrong, and a triage of twenty reference projects.

## Licence

MIT. Copyright © 2026 Dr. Sohil Momin. The attribution notice above must be
retained in copies and substantial portions of the software.
