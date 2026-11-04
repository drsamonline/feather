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

/// Identify a file from its first bytes, falling back to its extension.
pub fn detect(bytes: &[u8], extension: Option<&str>) -> Format {
    // Fixed-layout formats announce themselves.
    if starts_with(bytes, b"%PDF") {
        return Format::Pdf;
    }
    if starts_with(bytes, b"\x89PNG\r\n\x1a\n")
        || starts_with(bytes, b"\xff\xd8\xff")
        || starts_with(bytes, b"GIF87a")
        || starts_with(bytes, b"GIF89a")
        || starts_with(bytes, b"II*\x00")
        || starts_with(bytes, b"MM\x00*")
        || starts_with(bytes, b"BM")
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
        let head = &bytes[..bytes.len().min(512)];
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
    fn png_and_webp_are_images() {
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n....", None), Format::Image);
        assert_eq!(detect(b"RIFF\x00\x00\x00\x00WEBPVP8 ", None), Format::Image);
    }

    #[test]
    fn text_is_the_last_resort() {
        assert_eq!(detect(b"hello world\n", Some("txt")), Format::Text);
        assert_eq!(detect(b"", Some("rs")), Format::Text);
        assert_eq!(detect(b"", None), Format::Unknown);
    }
}
