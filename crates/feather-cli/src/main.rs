//! `feather` — the M0 spike.
//!
//! Before a single pixel reaches a window, the plan asks one question: how long
//! does it take to open a document and draw page one, and how big is the
//! binary? This answers both without a GUI, which is why it can be built and
//! measured on three operating systems in CI from day one.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use feather_core::{detect, Document, Format};

const USAGE: &str = "\
feather — read-only document viewer (M0 spike)

USAGE:
    feather info <file>
    feather render <file> [page] [-o out.png] [-s scale]

ARGS:
    <file>      document to open
    [page]      1-based page number (default 1)
    -o          output PNG path (default: page-<n>.png)
    -s          scale in pixels per point (default 1.5)

The render command prints timings to stderr, so `feather render x.pdf -o -`
style piping stays clean.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    let result = match args[0].as_str() {
        "info" => cmd_info(&args[1..]),
        "render" => cmd_render(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command: {other}\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("feather: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// The router's other half — the same match the window uses, deliberately kept
/// identical so the headless spike and the GUI can never disagree about what a
/// format means.
fn open_document(bytes: Vec<u8>, format: Format) -> Result<Box<dyn Document>, String> {
    match format {
        Format::Pdf => feather_pdf::PdfDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Image => feather_image::ImageDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        Format::Comic => feather_image::ComicDocument::from_bytes(bytes)
            .map(|d| Box::new(d) as Box<dyn Document>)
            .map_err(|e| e.to_string()),
        other => Err(format!("the {} pipeline is not implemented yet", other.as_str())),
    }
}

fn cmd_info(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("info needs a file")?;
    let path = PathBuf::from(path);
    let started = Instant::now();

    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str());
    let format = detect(&bytes, ext);
    let read = started.elapsed();

    println!("file      {}", path.display());
    println!("bytes     {}", bytes.len());
    println!("format    {}", format.as_str());
    println!("sniffed   {:.2} ms", read.as_secs_f64() * 1000.0);

    let doc = open_document(bytes, format).map_err(|e| format!("open failed: {e}"))?;
    println!("pages     {}", doc.page_count());
    if let Some(size) = doc.page_size(0) {
        println!("page 1    {:.0} × {:.0} pt", size.width, size.height);
    }
    println!(
        "total     {:.2} ms (read + open)",
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

fn cmd_render(args: &[String]) -> Result<(), String> {
    let mut path: Option<PathBuf> = None;
    let mut page: usize = 1;
    let mut out: Option<PathBuf> = None;
    let mut scale: f32 = 1.5;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                out = Some(PathBuf::from(args.get(i).ok_or("-o needs a path")?));
            }
            "-s" | "--scale" => {
                i += 1;
                scale = args
                    .get(i)
                    .ok_or("-s needs a number")?
                    .parse()
                    .map_err(|_| "-s must be a number".to_string())?;
            }
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => match other.parse::<usize>() {
                Ok(n) => page = n,
                Err(_) => return Err(format!("unexpected argument: {other}")),
            },
        }
        i += 1;
        }

    let path = path.ok_or("render needs a file")?;
    let out = out.unwrap_or_else(|| PathBuf::from(format!("page-{page}.png")));
    if page == 0 {
        return Err("page numbers start at 1".into());
    }

    let started = Instant::now();
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let read_done = started.elapsed();

    let format = detect(&bytes, path.extension().and_then(|e| e.to_str()));
    let doc = open_document(bytes, format).map_err(|e| format!("open failed: {e}"))?;
    let opened = started.elapsed();

    let index = page - 1;
    let rendered = doc
        .render(index, scale)
        .map_err(|e| format!("render failed: {e}"))?;
    let drawn = started.elapsed();

    std::fs::write(&out, &rendered.png)
        .map_err(|e| format!("writing {}: {e}", out.display()))?;

    eprintln!(
        "read {:.1} ms · open {:.1} ms · render {:.1} ms · total {:.1} ms",
        read_done.as_secs_f64() * 1000.0,
        (opened - read_done).as_secs_f64() * 1000.0,
        (drawn - opened).as_secs_f64() * 1000.0,
        drawn.as_secs_f64() * 1000.0
    );
    eprintln!(
        "page {} of {} · {}×{} px at {:.2}x · {} bytes png → {}",
        page,
        doc.page_count(),
        rendered.width,
        rendered.height,
        scale,
        rendered.byte_len(),
        out.display()
    );
    Ok(())
}
