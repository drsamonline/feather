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
| **wordcraft** | `.docx` | `wordcraft-docx` (reader/writer), `wordcraft-layout`, `wordcraft-render` (vello_cpu, "pages and thumbnails") | **Not taken — we wrote the read half.** Its value is a layout engine wrapped in an editor. |
| **deckcraft** | `.pptx` | `deckcraft-pptx` (ECMA-376 PresentationML), `deckcraft-render` | **Not taken — we wrote the text half.** Its renderer does charts, effects and embedded video; ours does words. |
| **gridcraft** | `.xlsx`, `.csv` | `gridcraft-xlsx`, `gridcraft-formula`, `gridcraft-numfmt` | **Superseded: we wrote our own.** See below. |
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

## Excel, and why GridCraft is not in it

`.xlsx` is now supported, by a reader written here in `feather-sheet`: a workbook
is a zip of XML, and we already read zips for comics. It handles shared strings,
inline strings, booleans, numbers and sheet names, and draws each worksheet as
one large page so the tile cache, the zoom ladder and the scroll model do the
rest.

GridCraft would have done it, and its `gridcraft-xlsx` crate is better than ours
on the hard parts — number formats, formulas, tables, charts. Three things tipped
it the other way: the crate is unpublished, so it is a git dependency on a 16 MB
workspace; it pulls `gridcraft-core`, `-model`, `-formula` and `-math` plus
`quick-xml`, `aes` and `cfb`; and it is shaped around an editing session. Two
hundred lines of reader is the better trade for a viewer that only displays —
and if we ever want formulas or number formats, `gridcraft-numfmt` is the crate
to take first.

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

## OpenDocument came free

`.odt`, `.ods` and `.odp` are the same idea in another dialect — a zip with a
`content.xml` — so none of them needed a pipeline. Each existing reader checks
which part it finds and parses the dialect it gets: `<text:p>` and `<text:h>` for
documents, `<table:table>` for sheets, with repeated rows and columns honoured
because that is how a spreadsheet stores a blank run.

Three formats became six in one commit, and the cost was two parsers rather than
three pipelines. That is the argument for one `Document` trait in miniature.

`.odp` routes to the slide pipeline, which still expects PPTX parts; it opens
with a clear message rather than a wrong page. A known gap, named.

## Slides

`.pptx` is the opposite of `.docx`, and that is why it took an afternoon. Word
hands you paragraphs and leaves the layout to you; PowerPoint hands you every
shape with its own position and extent in EMU, because the author already placed
it. There is no layout engine to write: read the boxes, read the text, draw it
where the file points.

What that means in practice: slides open at their true aspect ratio, titles are
larger than body copy, and the words clip to their boxes. What it does not mean:
images, shapes, themes, transitions, tables, speaker notes. Text only, and the
page says so.

## Word, and why WordCraft is not in it

`.docx` is now supported by a reader in `feather-epub`: a Word file is a zip of
XML, `word/document.xml` holds paragraphs of runs, and the blocks go to the same
`Reflow` that draws an EPUB. Heading styles survive, runs split mid-word rejoin,
numeric entities decode.

WordCraft's renderer would give real page layout — columns, floating images,
headers and footers, exact line breaks. That is a genuine step up from a reflow,
and it is not what a read-only viewer needs first: the product is a viewer, the
crates are an editor, and the dependency is a git workspace. The honest label is
"approximate", and the page says so.

If page-accurate Word ever becomes the requirement, `wordcraft-layout` and
`wordcraft-render` are where to start, and this note is the record that we
looked and chose otherwise.

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
