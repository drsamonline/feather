# The ArtCraft suite, read properly

Thirteen repositories from `storytold` were suggested as the answer to this
viewer's file-support gaps. They are real, they are Rust, they are Apache/MIT,
and several of them are the engine we would otherwise have to write. This note
records what each one actually is, what it would cost to use, and what I would
and would not do with it.

## What the suite is

Every repository follows one shape: a parser crate named after the format, a
shared model, a layout crate, and a rasteriser built on **vello_cpu** — the same
renderer hayro uses, and one this project already has in the tree through hayro.

That matters more than the feature list. A `*-render` crate turns a display list
into pixels, which is exactly the `Document::render` contract here: give me a
page and a scale, get a PNG back. Nothing above `feather-pdf` would need to know
a Word file from a PDF.

## The ones that matter to a document viewer

| Repository | Formats | Crates that do the work | Verdict |
|---|---|---|---|
| **wordcraft** | `.docx` | `wordcraft-docx` (reader/writer), `wordcraft-layout`, `wordcraft-render` (vello_cpu, "pages and thumbnails") | **The Office answer.** A real layout engine: drop caps, text wrapping, hyphenation, TOC, footnotes, track changes. 17k stars. |
| **deckcraft** | `.pptx` | `deckcraft-pptx` (ECMA-376 PresentationML), `deckcraft-render` (slides, masters, charts, effects) | **Slides.** Same shape, same renderer. Also carries its own h264/hevc/av1/vp9/opus codecs for embedded media. |
| **gridcraft** | `.xlsx`, `.csv` | `gridcraft-xlsx` (SpreadsheetML import/export), `gridcraft-formula`, `gridcraft-numfmt` | **Spreadsheets.** Import is solid; the pixel story is less clear — no `render` crate, only a `pdf` one. |
| **photocraft** | `.psd`, RAW, `.heif` | `photocraft-psd`, `photocraft-raw`, `photocraft-heif`, `photocraft-codecs` | **New image formats** the `image` crate will not open. Cheapest win in the suite: images already fit our pipeline exactly. |
| **vectorcraft** | `.svg`, `.eps` | `vectorcraft-svg`, `vectorcraft-eps`, `vectorcraft-render` | **Vector art**, if SVG ever needs to be more than text. |
| **designcraft** | `.idml`, `.epub` | `designcraft-idml`, `designcraft-epub` | InDesign markup. Niche, but nothing else reads it. |
| **cadcraft** | `.dwg`, `.dxf` | `cadcraft-dwg`, `cadcraft-dxf`, `cadcraft-render` | Drawings. Its DWG bridge wraps `acadrust`, which is **MPL-2.0** — a different licence question from the rest. |
| **figcraft** | `.fig` | `figcraft-format`, `figcraft-render` | Figma files. Interesting, not urgent. |

## The five that are not for us

`soundcraft` (Pro Tools), `filmcraft` (video editing), `lightcraft` (photo
editing), `effectcraft` (motion graphics) and `materialcraft` (node-based
materials) are excellent and irrelevant: a document viewer has nothing to say
about an audio session or a 3D material graph.

## What it would actually cost

1. **Almost nothing is on crates.io.** `wordcraft-docx`, `wordcraft-render`,
   `gridcraft-xlsx`, `photocraft-psd`, `vectorcraft-svg` and `designcraft-idml`
   are all unpublished. Using one means a **git dependency** on a 40 MB
   workspace, pinned to a commit, resolved fresh on every build.
   Beware name collisions: `cadcraft-dwg` on crates.io is a *different* project
   (a DWG↔DXF bridge built on `acadrust`), not the ArtCraft crate.
2. **Each format drags in siblings.** `wordcraft-docx` pulls `doc`, `geom` and
   external `quick-xml`, `zip`, `cfb`, `aes`, `sha1`, `sha2`, `hmac`, `base64`,
   `zeroize` — the last six because it also handles encrypted documents. Our
   dependency ceiling is 25 and we sit at 10; two formats would put a serious
   dent in the remaining 15.
3. **They are editors.** The model carries edits, undo and revisions. We would
   use the parse and render third of it and leave the rest, which is fine — but
   it means the crates are not shaped for a read-only consumer, and their APIs
   may assume an editing session exists.
4. **Binary size.** `vello_cpu` is already linked through hayro, so the renderer
   is nearly free. The parsers are not: expect a few hundred KB each.

## What I would do

**Start with wordcraft, as a spike, alone.** It is the format people actually
send each other, its renderer shares our graphics stack, and one git dependency
answers the question that matters: does an external consumer build cleanly
against their workspace? If it does, a `.docx` becomes a `Document` impl of about
a hundred lines and the plan's "Office in five honest tiers" collapses into
"Office renders, approximately, at real fidelity".

If it does not build, we learn that in one CI run rather than after three more
integrations.

**Then photocraft**, because PSD and HEIF slot into the image pipeline with no
layout work at all — parse to RGBA, hand it to the same tile cache.

**Not yet:** gridcraft, cadcraft, figcraft, designcraft. Each is a real format
with a small audience in a document viewer, and each spends dependency budget
that the two above need more.

## One correction to the plan

The plan says Office documents are "read-mode only, never page-accurate". That
was written assuming we would parse OOXML ourselves into reflowed blocks. A
layout engine changes the claim: WordCraft's renderer draws pages from a display
list, so page-accurate is on the table. The plan should say "page-accurate where
a layout engine exists, approximate otherwise" — and this note is the reason.
