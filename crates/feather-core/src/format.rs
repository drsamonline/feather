//! The router: one decision, made once, from bytes rather than trust.
//!
//! Extensions lie. A `.pdf` that is really a zip, a `.jpg` that is really a
//! PNG — both arrive in real inboxes. Magic bytes are the first word, the
//! extension is only a tie-breaker for container formats (EPUB, CBZ, XPS and
//! the Office formats are all zip files, so bytes alone cannot separate them).

/// What a file claims to be, after routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pdf,
    Epub,
    /// CBZ, CBR, CB7, CBT — an archive of images.
    Comic,
    Xps,
    Svg,
    /// Any still image the image pipeline can decode.
    Image,
    /// Plain text, Markdown, source code, CSV, JSON.
    Text,
    /// XLSX and XLSM — a workbook, drawn as a grid rather than as pages.
    Sheet,
    /// DOCX — read as blocks and reflowed, not laid out page-for-page.
    Doc,
    /// PPTX — slides, drawn at the coordinates the file already carries.
    Slide,
    /// DOCX, PPTX, ODT, ODS, ODP — still unimplemented, and still not claimed.
    Office,
    Unknown,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Epub => "epub",
            Format::Comic => "comic",
            Format::Xps => "xps",
            Format::Svg => "svg",
            Format::Image => "image",
            Format::Text => "text",
            Format::Sheet => "sheet",
            Format::Doc => "doc",
            Format::Slide => "slide",
            Format::Office => "office",
            Format::Unknown => "unknown",
        }
    }

    /// True when the file is a zip container, which is why the extension has
    /// to break the tie.
    pub fn is_zip_container(self) -> bool {
        matches!(
            self,
            Format::Epub
                | Format::Comic
                | Format::Xps
                | Format::Sheet
                | Format::Doc
                | Format::Slide
                | Format::Office
        )
    }

    /// The extension is only consulted when the bytes cannot decide.
    pub fn from_extension(ext: &str) -> Format {
        match ext.to_ascii_lowercase().as_str() {
            "pdf" | "ai" => Format::Pdf,
            "epub" => Format::Epub,
            "cbz" | "cbr" | "cb7" | "cbt" => Format::Comic,
            "xps" | "oxps" | "xod" | "dwfx" => Format::Xps,
            "svg" => Format::Svg,
            "png" | "jpg" | "jpeg" | "jfif" | "gif" | "bmp" | "webp" | "tif" | "tiff"
            | "avif" | "jxl" | "heic" | "heif" | "ico" | "tga" => Format::Image,
            "txt" | "log" | "nfo" | "md" | "markdown" | "json" | "csv" | "tsv" | "rs" | "toml"
            | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "js" | "ts" | "py" | "c" | "h"
            | "cpp" | "hpp" | "java" | "sh" | "sql" => Format::Text,
            "xlsx" | "xlsm" => Format::Sheet,
            "docx" => Format::Doc,
            "pptx" => Format::Slide,
            // The OpenDocument family is the same idea in another dialect, so it
            // rides the pipelines that already exist.
            "odt" => Format::Doc,
            "ods" => Format::Sheet,
            "odp" => Format::Slide,
            "rtf" => Format::Office,
            _ => Format::Unknown,
        }
    }
}

fn starts_with(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.len() >= needle.len() && &bytes[..needle.len()] == needle
}

/// Is this really a BMP?
///
/// `BM` is two bytes, which is far too weak to route a file on: a note that
/// begins "BMW service history" is not a bitmap, and sending it to the image
/// pipeline is how a text file fails to open. Corroborate the signature with
/// the rest of the header — both reserved words zero, and a DIB header size
/// from the set every BMP writer uses.
fn is_bmp(bytes: &[u8]) -> bool {
    if bytes.len() < 18 || !starts_with(bytes, b"BM") {
        return false;
    }
    if bytes[6..10] != [0u8, 0, 0, 0] {
        return false;
    }
    let dib = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124)
}

/// Identify a file from its first bytes, falling back to its extension.
pub fn detect(bytes: &[u8], extension: Option<&str>) -> Format {
    // Fixed-layout formats announce themselves.
    if starts_with(bytes, b"%PDF") {
        return Format::Pdf;
    }
    // Photoshop, which the image crate cannot open and this pipeline can.
    if starts_with(bytes, b"8BPS") {
        return Format::Image;
    }
    if starts_with(bytes, b"\x89PNG\r\n\x1a\n")
        || starts_with(bytes, b"\xff\xd8\xff")
        || starts_with(bytes, b"GIF87a")
        || starts_with(bytes, b"GIF89a")
        || starts_with(bytes, b"II*\x00")
        || starts_with(bytes, b"MM\x00*")
        || is_bmp(bytes)
    {
        return Format::Image;
    }
    // RIFF....WEBP
    if bytes.len() >= 12 && starts_with(bytes, b"RIFF") && bytes[8..12] == *b"WEBP" {
        return Format::Image;
    }

    // Everything below is either text or a zip container, and both need the
    // extension to finish the job.
    let by_ext = extension.map(Format::from_extension).unwrap_or(Format::Unknown);

    if starts_with(bytes, b"PK\x03\x04") {
        return if by_ext.is_zip_container() {
            by_ext
        } else {
            // A zip we were not told about is almost always a comic.
            Format::Comic
        };
    }

    if bytes.len() >= 5 {
        // A 512-byte cut can land inside a multi-byte character, which would
        // make good text look like binary. Step back to the last boundary.
        let head = &bytes[..bytes.len().min(512)];
        let head = match std::str::from_utf8(head) {
            Ok(_) => head,
            Err(e) if e.valid_up_to() > 0 => &head[..e.valid_up_to()],
            Err(_) => head,
        };
        if let Ok(text) = std::str::from_utf8(head) {
            let trimmed = text.trim_start();
            if trimmed.starts_with("<?xml") || trimmed.starts_with("<svg") {
                if trimmed.contains("<svg") || by_ext == Format::Svg {
                    return Format::Svg;
                }
            }
            if by_ext == Format::Text {
                return Format::Text;
            }
            // Valid UTF-8 with no other signature: treat as text — but only
            // when the extension has not already claimed a binary container.
            // A RAR archive whose header happens to decode as UTF-8 is not a
            // text file, and opening it as one is how a corrupt download looks
            // like a novel.
            if !trimmed.is_empty()
                && (by_ext == Format::Text || by_ext == Format::Unknown)
            {
                return Format::Text;
            }
        }
    }

    by_ext
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_wins_over_a_lying_extension() {
        assert_eq!(detect(b"%PDF-1.7\n", Some("jpg")), Format::Pdf);
    }

    #[test]
    fn a_zip_needs_its_extension_to_say_which_pipeline() {
        let zip = b"PK\x03\x04rest of a document";
        assert_eq!(detect(zip, Some("epub")), Format::Epub);
        assert_eq!(detect(zip, Some("cbz")), Format::Comic);
        assert_eq!(detect(zip, Some("docx")), Format::Doc);
        assert_eq!(detect(zip, Some("xlsx")), Format::Sheet);
        assert_eq!(detect(zip, Some("pptx")), Format::Slide);
        // OpenDocument rides the same three pipelines, in another dialect.
        assert_eq!(detect(zip, Some("odt")), Format::Doc);
        assert_eq!(detect(zip, Some("ods")), Format::Sheet);
        assert_eq!(detect(zip, Some("odp")), Format::Slide);
        // With no extension, a zip is a comic — the common case by far.
        assert_eq!(detect(zip, None), Format::Comic);
    }

    #[test]
    fn photoshop_files_are_images() {
        assert_eq!(detect(b"8BPS\x00\x01rest", Some("psd")), Format::Image);
        // A layered PSD is still just an image to the router.
        assert_eq!(detect(b"8BPS\x00\x01rest", None), Format::Image);
    }

    #[test]
    fn png_and_webp_are_images() {
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n....", None), Format::Image);
        assert_eq!(detect(b"RIFF\x00\x00\x00\x00WEBPVP8 ", None), Format::Image);
    }

    #[test]
    fn a_text_file_beginning_with_bm_is_not_a_bitmap() {
        // "BM" is two bytes. A note about a car is not an image.
        assert_eq!(detect(b"BMW service history\n", Some("txt")), Format::Text);
        assert_eq!(detect(b"BMW service history\n", None), Format::Text);
    }

    #[test]
    fn a_real_bmp_header_is_still_an_image() {
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&70u32.to_le_bytes()); // declared file size
        bmp.extend_from_slice(&[0u8, 0, 0, 0]); // reserved
        bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
        bmp.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        bmp.extend_from_slice(&[0u8; 26]);
        assert_eq!(detect(&bmp, None), Format::Image);
        assert_eq!(detect(&bmp, Some("bmp")), Format::Image);
    }

    #[test]
    fn a_character_split_at_the_cut_does_not_make_text_binary() {
        // 511 ASCII bytes, then a four-byte character straddling offset 512:
        // the sniff used to stop mid-character and call the file binary.
        let mut bytes = vec![b'a'; 511];
        bytes.extend_from_slice("\u{1F600}".as_bytes());
        bytes.extend_from_slice(b"more text\n");
        assert_eq!(detect(&bytes, None), Format::Text);
    }

    #[test]
    fn text_is_the_last_resort() {
        assert_eq!(detect(b"hello world\n", Some("txt")), Format::Text);
        assert_eq!(detect(b"", Some("rs")), Format::Text);
        assert_eq!(detect(b"", None), Format::Unknown);
    }
}
