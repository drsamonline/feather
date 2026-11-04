//! Spreadsheets.
//!
//! An `.xlsx` is a zip of XML: shared strings, one part per worksheet, and a
//! workbook part naming them. This reads those parts, builds a sparse grid, and
//! draws it — a sheet is not a page, so each worksheet becomes one very large
//! page that the existing tile cache, zoom and scroll handle exactly as they
//! handle a PDF.
//!
//! What this deliberately does not do: evaluate formulas. Excel stores the last
//! computed value beside every formula, and a viewer shows values. A spreadsheet
//! that recalculates on open is a spreadsheet editor.

use std::io::{Cursor, Read};

use feather_core::{Document, DocumentError, PageSize, RenderedPage, TextLayer, Word};
use feather_text::TextRenderer;

/// Column width and row height in points, matching Excel's defaults closely
/// enough that a sheet reads the way its author saw it.
const COL_W: f32 = 64.0;
const ROW_H: f32 = 18.0;
const MARGIN: f32 = 24.0;

const INK: u32 = 0x001A_1A1A;
const RULE: u32 = 0x00D8_D8D8;
const HEAD_FILL: u32 = 0x00F2_F2F2;
const PAPER: u32 = 0x00FF_FFFF;

/// A cell's display text, already resolved from shared strings and numbers.
type Grid = Vec<Vec<String>>;

pub struct SheetDocument {
    text: TextRenderer,
    title: Option<String>,
    sheets: Vec<Sheet>,
}

struct Sheet {
    name: String,
    grid: Grid,
    cols: usize,
    rows: usize,
}

impl SheetDocument {
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(data))
            .map_err(|e| DocumentError::Decode(format!("not a readable workbook: {e}")))?;

        // ODS is the same idea in another dialect: one content.xml with a table
        // per sheet instead of a workbook part and a file per worksheet.
        if archive.by_name("content.xml").is_ok() {
            let xml = read_part(&mut archive, "content.xml")
                .ok_or_else(|| DocumentError::Decode("the .ods has no content".to_string()))?;
            let sheets = parse_ods(&xml);
            if sheets.is_empty() {
                return Err(DocumentError::Decode(
                    "the workbook has no cells to show".to_string(),
                ));
            }
            let title = Some(sheets[0].name.clone());
            return Ok(Self {
                text: TextRenderer::load(),
                title,
                sheets,
            });
        }

        let shared = read_part(&mut archive, "xl/sharedStrings.xml")
            .map(|xml| parse_shared_strings(&xml))
            .unwrap_or_default();

        let names = read_part(&mut archive, "xl/workbook.xml")
            .map(|xml| parse_sheet_names(&xml))
            .unwrap_or_default();

        // Worksheet parts, in the order the workbook lists them where possible.
        let mut parts: Vec<String> = archive
            .file_names()
            .filter(|name| name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml"))
            .map(|name| name.to_string())
            .collect();
        parts.sort_by(|a, b| natural(a, b));

        if parts.is_empty() {
            return Err(DocumentError::Decode(
                "the workbook has no worksheets".to_string(),
            ));
        }

        let mut sheets = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            let Some(xml) = read_part(&mut archive, part) else {
                continue;
            };
            let grid = parse_sheet(&xml, &shared);
            let cols = grid.iter().map(|row| row.len()).max().unwrap_or(0);
            let rows = grid.len();
            if rows == 0 || cols == 0 {
                continue;
            }
            sheets.push(Sheet {
                name: names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("Sheet {}", index + 1)),
                grid,
                cols,
                rows,
            });
        }

        if sheets.is_empty() {
            return Err(DocumentError::Decode(
                "the workbook has no cells to show".to_string(),
            ));
        }

        let title = Some(sheets[0].name.clone());
        Ok(Self {
            text: TextRenderer::load(),
            title,
            sheets,
        })
    }

    fn sheet_size(sheet: &Sheet) -> PageSize {
        PageSize {
            width: sheet.cols as f32 * COL_W + MARGIN * 2.0,
            height: sheet.rows as f32 * ROW_H + MARGIN * 2.0,
        }
    }
}

impl Document for SheetDocument {
    fn page_count(&self) -> usize {
        self.sheets.len()
    }

    fn page_size(&self, page: usize) -> Option<PageSize> {
        self.sheets.get(page).map(Self::sheet_size)
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        let count = self.page_count();
        let Some(sheet) = self.sheets.get(page) else {
            return Err(DocumentError::OutOfRange { page, count });
        };
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let size = Self::sheet_size(sheet);

        let width = (size.width * scale).round().max(1.0) as u32;
        let height = (size.height * scale).round().max(1.0) as u32;

        // A big sheet at a high zoom is a big allocation. Refuse rather than
        // take the machine down with it; the viewer caps its own zoom anyway.
        const LIMIT: u64 = 96_000_000;
        if (width as u64) * (height as u64) > LIMIT {
            return Err(DocumentError::Decode(format!(
                "this sheet is {width}×{height} pixels at {scale:.2}× — zoom out to see it"
            )));
        }

        let mut canvas = vec![PAPER; (width as usize) * (height as usize)];
        let clip = (0, 0, width as i32, height as i32);

        let px = 11.0 * scale;
        let cell_w = COL_W * scale;
        let cell_h = ROW_H * scale;
        let margin = MARGIN * scale;

        // The frozen first row is drawn with a tint, which is what makes a
        // spreadsheet read as a table rather than a wall of words.
        let header_h = cell_h;
        let header_top = margin;
        if !sheet.grid.is_empty() {
            fill_rect(
                &mut canvas,
                width,
                height,
                margin,
                header_top,
                cell_w * sheet.cols as f32,
                header_h,
                HEAD_FILL,
            );
        }

        for (row_index, row) in sheet.grid.iter().enumerate() {
            let y = margin + row_index as f32 * cell_h;
            for (col_index, value) in row.iter().enumerate() {
                if value.is_empty() {
                    continue;
                }
                let x = margin + col_index as f32 * cell_w;
                let baseline = y + cell_h * 0.72;
                // Clip to the cell so a long value cannot spill into its
                // neighbour, which is what a spreadsheet does and what makes
                // wide text readable at all.
                let cell_clip = (
                    x as i32,
                    y as i32,
                    (x + cell_w) as i32,
                    (y + cell_h) as i32,
                );
                let clip = (
                    cell_clip.0.max(clip.0),
                    cell_clip.1.max(clip.1),
                    cell_clip.2.min(clip.2),
                    cell_clip.3.min(clip.3),
                );
                if row_index == 0 {
                    self.text
                        .draw_bold(&mut canvas, width, height, x + 4.0 * scale, baseline, value, px, INK, clip);
                } else {
                    self.text
                        .draw(&mut canvas, width, height, x + 4.0 * scale, baseline, value, px, INK, clip);
                }
            }
        }

        // Rules, drawn after the text so they sit on top the way a grid does.
        for col in 0..=sheet.cols {
            let x = margin + col as f32 * cell_w;
            fill_rect(&mut canvas, width, height, x, margin, scale.max(0.5), cell_h * sheet.rows as f32, RULE);
        }
        for row in 0..=sheet.rows {
            let y = margin + row as f32 * cell_h;
            fill_rect(&mut canvas, width, height, margin, y, cell_w * sheet.cols as f32, scale.max(0.5), RULE);
        }

        let png = encode_png(&canvas, width, height)?;
        Ok(RenderedPage {
            width,
            height,
            scale,
            png,
        })
    }

    fn title(&self) -> Option<String> {
        self.title.clone()
    }

    fn text_layer(&self, page: usize) -> Option<TextLayer> {
        let sheet = self.sheets.get(page)?;
        let mut words = Vec::new();
        for (row_index, row) in sheet.grid.iter().enumerate() {
            for (col_index, value) in row.iter().enumerate() {
                if value.is_empty() {
                    continue;
                }
                words.push(Word {
                    text: value.clone(),
                    x: MARGIN + col_index as f32 * COL_W,
                    y: MARGIN + row_index as f32 * ROW_H,
                    width: COL_W,
                    height: ROW_H,
                });
            }
        }
        Some(TextLayer { words })
    }
}

/// Sheets from an ODS `content.xml`.
fn parse_ods(xml: &str) -> Vec<Sheet> {
    let mut sheets = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<table:table ") {
        let after = &rest[start..];
        let Some(close) = after.find("</table:table>") else { break };
        let block = &after[..close];
        rest = &after[close + 14..];

        let name = attr(block, "table:name").unwrap_or_else(|| "Sheet".to_string());
        let mut grid: Grid = Vec::new();
        let mut rows = block;

        while let Some(row_start) = rows.find("<table:table-row") {
            let after_row = &rows[row_start..];
            let Some(row_end) = after_row.find("</table:table-row>") else { break };
            let row = &after_row[..row_end];
            rows = &after_row[row_end + 16..];

            // A row may repeat, which is how a spreadsheet stores a blank run.
            let repeat: usize = attr(row, "table:number-rows-repeated")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1)
                .min(1000);

            let mut cells: Vec<String> = Vec::new();
            let mut rest_cells = row;
            while let Some(cell_start) = rest_cells.find("<table:table-cell") {
                let after_cell = &rest_cells[cell_start..];
                let Some(gt) = after_cell.find('>') else { break };
                let self_closing = after_cell[..gt].ends_with('/');
                let (body, next) = if self_closing {
                    ("", &after_cell[gt + 1..])
                } else {
                    match after_cell.find("</table:table-cell>") {
                        Some(end) => (&after_cell[gt + 1..end], &after_cell[end + 18..]),
                        None => break,
                    }
                };
                let value = ods_cell_text(body);
                let span: usize = attr(&after_cell[..gt], "table:number-columns-repeated")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1)
                    .min(1000);
                for _ in 0..span {
                    cells.push(value.clone());
                }
                rest_cells = next;
            }

            while cells.last().map(|c| c.is_empty()).unwrap_or(false) {
                cells.pop();
            }
            if cells.is_empty() {
                continue;
            }
            for _ in 0..repeat {
                grid.push(cells.clone());
            }
        }

        while grid.last().map(|row| row.iter().all(|c| c.is_empty())).unwrap_or(false) {
            grid.pop();
        }

        let cols = grid.iter().map(|row| row.len()).max().unwrap_or(0);
        if cols == 0 || grid.is_empty() {
            continue;
        }
        sheets.push(Sheet {
            name,
            rows: grid.len(),
            cols,
            grid,
        });
    }

    sheets
}

/// The visible text of a cell: its paragraphs joined by a space.
fn ods_cell_text(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(start) = rest.find("<text:p") {
        let after = &rest[start..];
        let Some(gt) = after.find('>') else { break };
        let Some(end) = after.find("</text:p>") else { break };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&strip_tags(&after[gt + 1..end]));
        rest = &after[end + 8..];
    }
    out.trim().to_string()
}

fn strip_tags(inner: &str) -> String {
    let mut out = String::new();
    let mut rest = inner;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        match rest[lt..].find('>') {
            Some(gt) => rest = &rest[lt + gt + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    unescape(&out)
}

fn read_part(archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut out = String::new();
    file.read_to_string(&mut out).ok()?;
    Some(out)
}

fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    feather_core::natural_cmp(a, b)
}

/// `<si><t>text</t></si>` entries, in order.
fn parse_shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<si") {
        let after = &rest[start..];
        let Some(end) = after.find("</si>") else { break };
        let block = &after[..end];
        out.push(text_of(block));
        rest = &after[end + 5..];
    }
    out
}

/// Sheet names from the workbook part, in order.
fn parse_sheet_names(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<sheet ") {
        let after = &rest[start..];
        let Some(end) = after.find('>') else { break };
        let tag = &after[..end];
        if let Some(name) = attr(tag, "name") {
            out.push(unescape(&name));
        }
        rest = &after[end + 1..];
    }
    out
}

/// Cells of one worksheet, positioned by their `r` reference.
fn parse_sheet(xml: &str, shared: &[String]) -> Grid {
    let mut grid: Grid = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<c ") {
        let after = &rest[start..];
        let Some(tag_end) = after.find('>') else { break };
        let tag = &after[..tag_end];

        let Some(reference) = attr(tag, "r") else {
            rest = &after[tag_end + 1..];
            continue;
        };
        let (col, row) = match cell_ref(&reference) {
            Some(pair) => pair,
            None => {
                rest = &after[tag_end + 1..];
                continue;
            }
        };

        // A cell is either self-closing or has a body we must read.
        let self_closing = tag.ends_with('/');
        let (body, next) = if self_closing {
            ("", &after[tag_end + 1..])
        } else {
            let Some(close) = after.find("</c>") else { break };
            (&after[tag_end + 1..close], &after[close + 4..])
        };

        let kind = attr(tag, "t").unwrap_or_default();
        let value = match kind.as_str() {
            "s" => {
                let index: usize = first_tag_text(body, "v").and_then(|v| v.trim().parse().ok()).unwrap_or(usize::MAX);
                shared.get(index).cloned().unwrap_or_default()
            }
            "inlineStr" => text_of(body),
            "str" => first_tag_text(body, "v").map(|v| unescape(&v)).unwrap_or_default(),
            "b" => match first_tag_text(body, "v").as_deref().map(str::trim) {
                Some("1") => "TRUE".to_string(),
                Some("0") => "FALSE".to_string(),
                _ => String::new(),
            },
            _ => first_tag_text(body, "v").map(|v| trim_number(&v)).unwrap_or_default(),
        };

        if !value.is_empty() {
            if grid.len() <= row {
                grid.resize(row + 1, Vec::new());
            }
            if grid[row].len() <= col {
                grid[row].resize(col + 1, String::new());
            }
            grid[row][col] = value;
        }

        rest = next;
    }

    // Trailing empty rows and columns are noise, not data.
    while grid.last().map(|row| row.iter().all(|c| c.is_empty())).unwrap_or(false) {
        grid.pop();
    }
    for row in grid.iter_mut() {
        while row.last().map(|c| c.is_empty()).unwrap_or(false) {
            row.pop();
        }
    }
    grid
}

/// `AB12` → column 27, row 11.
fn cell_ref(reference: &str) -> Option<(usize, usize)> {
    let mut col = 0usize;
    let mut letters = 0;
    for ch in reference.chars() {
        if ch.is_ascii_alphabetic() {
            col = col * 26 + (ch.to_ascii_uppercase() as usize - 'A' as usize + 1);
            letters += 1;
        } else {
            break;
        }
    }
    if letters == 0 {
        return None;
    }
    let digits: String = reference.chars().filter(|c| c.is_ascii_digit()).collect();
    let row: usize = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((col - 1, row - 1))
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// The concatenated text of every `<t>` in a block.
fn text_of(block: &str) -> String {
    let mut out = String::new();
    let mut rest = block;
    while let Some(start) = rest.find("<t") {
        let after = &rest[start..];
        let Some(gt) = after.find('>') else { break };
        let inner_start = gt + 1;
        let Some(end) = after[inner_start..].find("</t>") else { break };
        out.push_str(&unescape(&after[inner_start..inner_start + end]));
        rest = &after[inner_start + end + 4..];
    }
    out
}

fn first_tag_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = body.find(&open)?;
    let after = &body[start..];
    let gt = after.find('>')?;
    let close = after.find(&format!("</{tag}>"))?;
    if close < gt {
        return None;
    }
    Some(after[gt + 1..close].to_string())
}

fn trim_number(value: &str) -> String {
    let trimmed = value.trim();
    // Excel stores 3 as 3, but 0.30000000000000004 happens; show it the way a
    // person would write it rather than the way a float did.
    if let Ok(number) = trimmed.parse::<f64>() {
        if number.fract() == 0.0 && number.abs() < 1e15 {
            return format!("{}", number as i64);
        }
        let rounded = format!("{number:.10}");
        return rounded.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    trimmed.to_string()
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn fill_rect(buf: &mut [u32], width: u32, height: u32, x: f32, y: f32, w: f32, h: f32, colour: u32) {
    let x0 = x.floor().max(0.0) as i64;
    let y0 = y.floor().max(0.0) as i64;
    let x1 = (x + w).ceil().min(width as f32) as i64;
    let y1 = (y + h).ceil().min(height as f32) as i64;
    for py in y0..y1 {
        for px in x0..x1 {
            let index = (py as usize) * (width as usize) + px as usize;
            if index < buf.len() {
                buf[index] = colour;
            }
        }
    }
}

fn encode_png(canvas: &[u32], width: u32, height: u32) -> Result<Vec<u8>, DocumentError> {
    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for pixel in canvas {
        rgba.push((pixel >> 16) as u8);
        rgba.push((pixel >> 8) as u8);
        rgba.push(*pixel as u8);
        rgba.push(255);
    }
    let image = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| DocumentError::Decode("the sheet has the wrong size".to_string()))?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| DocumentError::Decode(e.to_string()))?;
    Ok(png)
}
