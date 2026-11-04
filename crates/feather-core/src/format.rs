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
            Format::Office => "office",
            Format::Unknown => "unknown",
        }
    }

    /// True when the file is a zip container, which is why the extension has
    /// to break the tie.
    pub fn is_zip_container(self) -> bool {
        matches!(
            self,
            Format::Epub | Format::Comic | Format::Xps | Format::Sheet | Format::Office
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
            "docx" | "pptx" | "odt" | "ods" | "odp" | "rtf" => Format::Office,
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
    fn a_workbook_is_a_sheet_not_a_generic_office_file() {
        let zip = b"PK\x03\x04rest of a workbook";
        assert_eq!(detect(zip, Some("xlsx")), Format::Sheet);
        assert_eq!(detect(zip, Some("xlsm")), Format::Sheet);
        assert_eq!(detect(zip, Some("docx")), Format::Office);
    }

    #[test]
    fn a_binary_extension_beats_a_text_looking_header() {
        // "Rar!" and its version bytes are valid UTF-8, which used to make a
        // CBR archive open as a text file.
        let rar = b"Rar!\x1a\x07\x01\x00\x00\x00\x00\x00\x00";
        assert_eq!(detect(rar, Some("cbr")), Format::Comic);
        assert_eq!(detect(rar, Some("cbz")), Format::Comic);
        // With no extension at all, text is still the reasonable guess.
        assert_eq!(detect(rar, None), Format::Text);
    }

    #[test]
    fn zip_needs_the_extension() {
        assert_eq!(detect(b"PK\x03\x04rest", Some("epub")), Format::Epub);
        assert_eq!(detect(b"PK\x03\x04rest", Some("cbz")), Format::Comic);
        assert_eq!(detect(b"PK\x03\x04rest", Some("docx")), Format::Office);
        assert_eq!(detect(b"PK\x03\x04rest", None), Format::Comic);
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
