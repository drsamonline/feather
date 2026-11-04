//! Still images, and comics — which are an archive of still images.
//!
//! Both exist to test one claim from the plan: adding a format should mean one
//! new file and one router arm, and nothing in the window. These two
//! implementations of [`Document`] are that claim, cashed.

use std::io::{Cursor, Read};

use feather_core::{natural_cmp, Document, DocumentError, PageSize, RenderedPage};

use image::{imageops::FilterType, DynamicImage, ImageFormat, RgbaImage};

/// Extensions a comic archive may contain.
fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Unpack a RAR archive in-process and return its images in reading order.
///
/// RAR is the one format here whose decoder is not pure Rust: `unrar-ng` binds
/// RARLAB's UnRAR C++, which is freeware — usable in any software and
/// distributable inside it, on the single condition that it is not used to build
/// a RAR archiver. The bindings are MIT/Apache; the C++ carries its own licence,
/// shipped as `LICENSE-UNRAR`.
///
/// This replaced shelling out to `unrar`/`7z`/`bsdtar`, which worked but made a
/// portable single binary depend on something the user had to install.
fn extract_rar(data: &[u8]) -> Result<Vec<Vec<u8>>, DocumentError> {
    let root = std::env::temp_dir().join(format!("feather-cbr-{}", std::process::id()));
    let unpacked = root.join("pages");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&unpacked)
        .map_err(|e| DocumentError::Decode(format!("could not create a temp directory: {e}")))?;

    let archive = root.join("comic.cbr");
    std::fs::write(&archive, data)
        .map_err(|e| DocumentError::Decode(format!("could not stage the archive: {e}")))?;

    let opened = unrar_ng::Archive::new(&archive)
        .open_for_processing()
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&root);
            DocumentError::Decode(format!("could not read the RAR archive: {e}"))
        })?;

    if let Err(e) = opened.extract_all(&unpacked) {
        let _ = std::fs::remove_dir_all(&root);
        return Err(DocumentError::Decode(format!(
            "could not extract the RAR archive: {e}"
        )));
    }

    let mut files = Vec::new();
    collect_images(&unpacked, &mut files);
    // Sorted by file name, not by full path. RARs written on Windows store
    // `folder\\page01.jpg`, and an extractor on Linux may keep those
    // backslashes in the name — comparing whole paths then orders the book by
    // whatever the separator happens to be.
    files.sort_by(|a, b| {
        let name = |path: &std::path::Path| {
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string())
        };
        feather_core::natural_cmp(&name(a), &name(b))
    });

    let mut pages = Vec::with_capacity(files.len());
    for file in files {
        if let Ok(bytes) = std::fs::read(&file) {
            pages.push(bytes);
        }
    }

    let _ = std::fs::remove_dir_all(&root);
    Ok(pages)
}

/// Every image under a directory, recursively.
fn collect_images(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_images(&path, out);
        } else if path
            .file_name()
            .map(|name| is_image_name(&name.to_string_lossy()))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

fn encode_png(img: RgbaImage) -> Result<Vec<u8>, DocumentError> {
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| DocumentError::Decode(e.to_string()))?;
    Ok(out)
}

/// Scale an image so that one source pixel becomes `scale` device pixels.
fn rasterise(img: &RgbaImage, scale: f32) -> RgbaImage {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let w = ((img.width() as f32) * scale).round().max(1.0) as u32;
    let h = ((img.height() as f32) * scale).round().max(1.0) as u32;
    if w == img.width() && h == img.height() {
        return img.clone();
    }
    image::imageops::resize(img, w, h, FilterType::Triangle)
}

/// One still image, as a one-page document.
///
/// An image has no intrinsic size in points, so one pixel is treated as one
/// point: at 100% zoom the picture is shown at its native resolution, which is
/// what someone opening a photograph expects.
pub struct ImageDocument {
    image: RgbaImage,
}

impl ImageDocument {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, DocumentError> {
        let decoded = image::load_from_memory(&bytes)
            .map_err(|e| DocumentError::Decode(e.to_string()))?;
        Ok(ImageDocument {
            image: decoded.to_rgba8(),
        })
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DocumentError> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::from_bytes(bytes)
    }
}

impl Document for ImageDocument {
    fn page_count(&self) -> usize {
        1
    }

    fn page_size(&self, page: usize) -> Option<PageSize> {
        if page != 0 {
            return None;
        }
        Some(PageSize {
            width: self.image.width() as f32,
            height: self.image.height() as f32,
        })
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        if page != 0 {
            return Err(DocumentError::OutOfRange { page, count: 1 });
        }
        let scaled = rasterise(&self.image, scale);
        let width = scaled.width();
        let height = scaled.height();
        Ok(RenderedPage {
            width,
            height,
            scale: if scale > 0.0 { scale } else { 1.0 },
            png: encode_png(scaled)?,
        })
    }
}

/// A comic archive: a zip full of images, one page each.
///
/// The archive is kept as bytes and opened per page rather than held open,
/// because a `ZipArchive` wants exclusive access and the render thread is the
/// only thing that should pay for that.
pub struct ComicDocument {
    data: Vec<u8>,
    names: Vec<String>,
    /// Pages already extracted into memory — used for CBR, where the archive is
    /// unpacked by an external tool and there is nothing left to seek into.
    pages: Vec<Vec<u8>>,
}

impl ComicDocument {
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, DocumentError> {
        // CBR is a RAR archive, not a zip. There is no permissive RAR decoder in
        // Rust — the pure-Rust one is GPL-3.0 — so the archive is handed to a
        // helper the system already has, the same way the plan treats
        // LibreOffice for Office documents. No dependency, no licence
        // entanglement, and a clear message when no helper is installed.
        if data.starts_with(b"Rar!") {
            let pages = extract_rar(&data)?;
            if pages.is_empty() {
                return Err(DocumentError::Decode(
                    "the RAR archive contains no images".to_string(),
                ));
            }
            return Ok(ComicDocument {
                data: Vec::new(),
                names: Vec::new(),
                pages,
            });
        }

        let mut archive = zip::ZipArchive::new(Cursor::new(data.as_slice()))
            .map_err(|e| DocumentError::Decode(format!("not a readable archive: {e}")))?;

        let mut names = Vec::new();
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| DocumentError::Decode(e.to_string()))?;
            if !file.is_dir() && is_image_name(file.name()) {
                names.push(file.name().to_string());
            }
        }
        names.sort_by(|a, b| natural_cmp(a, b));

        if names.is_empty() {
            return Err(DocumentError::Decode(
                "archive contains no images".to_string(),
            ));
        }

        Ok(ComicDocument {
            data,
            names,
            pages: Vec::new(),
        })
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DocumentError> {
        let data = std::fs::read(path.as_ref())?;
        Self::from_bytes(data)
    }

    fn page_bytes(&self, page: usize) -> Result<Vec<u8>, DocumentError> {
        if !self.pages.is_empty() {
            return self
                .pages
                .get(page)
                .cloned()
                .ok_or(DocumentError::OutOfRange {
                    page,
                    count: self.pages.len(),
                });
        }
        let name = self
            .names
            .get(page)
            .ok_or(DocumentError::OutOfRange {
                page,
                count: self.names.len(),
            })?;
        let mut archive = zip::ZipArchive::new(Cursor::new(self.data.as_slice()))
            .map_err(|e| DocumentError::Decode(e.to_string()))?;
        let mut file = archive
            .by_name(name)
            .map_err(|e| DocumentError::Decode(e.to_string()))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

impl Document for ComicDocument {
    fn page_count(&self) -> usize {
        if self.pages.is_empty() {
            self.names.len()
        } else {
            self.pages.len()
        }
    }

    fn page_size(&self, _page: usize) -> Option<PageSize> {
        // Learned on first render, like hayro's pages.
        None
    }

    fn render(&self, page: usize, scale: f32) -> Result<RenderedPage, DocumentError> {
        let bytes = self.page_bytes(page)?;
        let decoded = image::load_from_memory(&bytes)
            .map_err(|e| DocumentError::Decode(e.to_string()))?;
        let scaled = rasterise(&decoded.to_rgba8(), scale);
        let width = scaled.width();
        let height = scaled.height();
        Ok(RenderedPage {
            width,
            height,
            scale: if scale > 0.0 { scale } else { 1.0 },
            png: encode_png(scaled)?,
        })
    }

    fn title(&self) -> Option<String> {
        self.names.first().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comic_pages_come_out_in_reading_order() {
        let mut names = vec!["page10.png", "page2.png", "page1.png"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["page1.png", "page2.png", "page10.png"]);
    }

    #[test]
    fn only_image_extensions_count() {
        assert!(is_image_name("a/b/001.PNG"));
        assert!(is_image_name("page.jpeg"));
        assert!(!is_image_name("Thumbs.db"));
        assert!(!is_image_name("readme.txt"));
    }

    #[test]
    fn scaling_never_produces_a_zero_sized_bitmap() {
        let img = RgbaImage::new(4, 4);
        let tiny = rasterise(&img, 0.0);
        assert!(tiny.width() >= 1 && tiny.height() >= 1);
        let big = rasterise(&img, 3.0);
        assert_eq!(big.width(), 12);
    }
}
