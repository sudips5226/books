//! `pdf2word` – command-line PDF to DOCX converter.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context};
use clap::Parser;
use pdf2word_core::{convert, CancelToken, ConvertOptions, LayoutMode, OcrMode, Progress};

#[derive(Parser, Debug)]
#[command(name = "pdf2word", version, about = "Convert PDF to Word (DOCX) preserving layout and formatting, with OCR for scanned pages")]
struct Args {
    /// Input PDF file(s).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output DOCX file (single input) or directory (multiple inputs).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Layout strategy: `flow` (editable paragraphs, page by page), `continuous`
    /// (one flowing document: Word columns, paragraphs and tables joined across
    /// pages) or `exact` (positioned frames).
    #[arg(short, long, default_value = "flow")]
    mode: LayoutMode,

    /// OCR: `auto` (scanned pages only), `always` or `never`.
    #[arg(long, default_value = "auto")]
    ocr: OcrMode,

    /// Tesseract language(s), e.g. `eng` or `eng+deu`.
    #[arg(short, long, default_value = "eng")]
    lang: String,

    /// Directory containing Tesseract `*.traineddata` files.
    #[arg(long)]
    tessdata: Option<PathBuf>,

    /// Rendering resolution for OCR.
    #[arg(long, default_value_t = 300.0)]
    ocr_dpi: f32,

    /// Font family for OCR text; `auto` picks Times New Roman, Arial or
    /// Courier New from the letter shapes.
    #[arg(long, default_value = "auto")]
    ocr_font: String,

    /// Pages to convert, 1-based (e.g. `1-3,7,10-`).
    #[arg(short, long)]
    pages: Option<String>,

    /// Password for encrypted PDFs.
    #[arg(long)]
    password: Option<String>,

    /// Do not extract images.
    #[arg(long)]
    no_images: bool,

    /// Do not rasterise vector figures (charts, diagrams).
    #[arg(long)]
    no_figures: bool,

    /// Do not detect tables.
    #[arg(long)]
    no_tables: bool,

    /// Do not assign heading styles.
    #[arg(long)]
    no_headings: bool,

    /// Do not adjust character spacing to keep the PDF's text widths.
    #[arg(long)]
    no_width_compensation: bool,

    /// Do not embed the PDF's fonts into the DOCX.
    #[arg(long)]
    no_embed_fonts: bool,

    /// Keep repeated page tops/bottoms in the body instead of Word headers/footers.
    #[arg(long)]
    no_headers_footers: bool,

    /// Keep list markers as typed text instead of Word numbering.
    #[arg(long)]
    no_lists: bool,

    /// Keep notes at the bottom of the page instead of Word footnotes.
    #[arg(long)]
    no_footnotes: bool,

    /// Do not build paragraph styles (every run carries its full formatting).
    #[arg(long)]
    no_styles: bool,

    /// Ignore the PDF outline, internal links and tables of contents.
    #[arg(long)]
    no_navigation: bool,

    /// Do not turn annotations into Word comments.
    #[arg(long)]
    no_comments: bool,

    /// Keep vector figures as pictures only (no editable shapes).
    #[arg(long)]
    no_shapes: bool,

    /// Path to the PDFium shared library (file or directory).
    #[arg(long)]
    pdfium: Option<PathBuf>,

    /// Only print warnings and errors.
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    let args = Args::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(if args.quiet { "warn" } else { "info" }))
        .format_timestamp(None)
        .format_target(false)
        .init();
    if let Err(e) = run(args) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    let mut opts = ConvertOptions {
        layout_mode: args.mode,
        ocr_mode: args.ocr,
        ocr_dpi: args.ocr_dpi.clamp(72.0, 600.0),
        pages: args.pages.clone(),
        password: args.password.clone(),
        detect_headings: !args.no_headings,
        compensate_widths: !args.no_width_compensation,
        embed_fonts: !args.no_embed_fonts,
        headers_footers: !args.no_headers_footers,
        word_lists: !args.no_lists,
        footnotes: !args.no_footnotes,
        paragraph_styles: !args.no_styles,
        navigation: !args.no_navigation,
        annotations: !args.no_comments,
        pdfium_path: args.pdfium.clone(),
        ..Default::default()
    };
    opts.ocr.language = args.lang.clone();
    opts.ocr.tessdata = args.tessdata.clone();
    opts.ocr.font = args.ocr_font.clone();
    opts.extract.extract_images = !args.no_images;
    opts.extract.rasterize_vector_graphics = !args.no_figures && !args.no_images;
    opts.layout.detect_tables = !args.no_tables;
    opts.extract.vector_shapes = !args.no_shapes;

    let multi = args.inputs.len() > 1;
    if multi {
        if let Some(o) = &args.output {
            std::fs::create_dir_all(o).with_context(|| format!("cannot create {}", o.display()))?;
        }
    }
    let cancel = CancelToken::new();
    install_ctrlc(cancel.clone());

    let mut failures = 0;
    for input in &args.inputs {
        if !input.exists() {
            bail!("{} does not exist", input.display());
        }
        let output = match (&args.output, multi) {
            (Some(o), false) => o.clone(),
            (Some(dir), true) => dir.join(input.with_extension("docx").file_name().unwrap()),
            (None, _) => input.with_extension("docx"),
        };
        let start = Instant::now();
        let quiet = args.quiet;
        let result = convert(input, &output, &opts, Some(&cancel), &mut |p| match p {
            Progress::Opened { pages } if !quiet => log::info!("{}: {pages} page(s)", input.display()),
            Progress::PageOcr { page } if !quiet => log::info!("  page {page}: running OCR"),
            Progress::PageDone { page, done, total, ocr } if !quiet => {
                log::info!("  page {page} done ({done}/{total}){}", if ocr { " [OCR]" } else { "" })
            }
            Progress::Warning(w) => log::warn!("{w}"),
            _ => {}
        });
        match result {
            Ok(r) => {
                if !quiet {
                    log::info!(
                        "wrote {} ({} pages, {} paragraphs, {} tables, {} images{}{}) in {:.1}s",
                        output.display(),
                        r.pages,
                        r.paragraphs,
                        r.tables,
                        r.images,
                        if r.ocr_pages.is_empty() { String::new() } else { format!(", OCR on {} page(s)", r.ocr_pages.len()) },
                        if r.embedded_fonts.is_empty() { String::new() } else { format!(", embedded fonts: {}", r.embedded_fonts.join(", ")) },
                        start.elapsed().as_secs_f32()
                    );
                }
            }
            Err(e) => {
                failures += 1;
                log::error!("{}: {e:#}", input.display());
                if !multi {
                    return Err(e);
                }
            }
        }
    }
    if failures > 0 {
        bail!("{failures} file(s) failed");
    }
    Ok(())
}

/// Installs a Ctrl-C handler that cancels the conversion (Unix only; no-op elsewhere).
/// The handler only performs atomic operations, which keeps it async-signal-safe.
fn install_ctrlc(token: CancelToken) {
    #[cfg(unix)]
    {
        use std::sync::OnceLock;
        static TOKEN: OnceLock<CancelToken> = OnceLock::new();
        let _ = TOKEN.set(token);
        extern "C" fn on_sigint(_: i32) {
            if let Some(t) = TOKEN.get() {
                t.cancel();
            }
        }
        extern "C" {
            fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
        }
        unsafe {
            signal(2, on_sigint);
        }
    }
    #[cfg(not(unix))]
    let _ = token;
}
