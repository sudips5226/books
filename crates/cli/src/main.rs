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

    /// Tesseract language(s), e.g. `eng` or `eng+deu`; `auto` detects the
    /// language from the document among the installed ones.
    #[arg(short, long, default_value = "auto")]
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

    /// Do not turn scans upright or straighten skewed scans before OCR.
    #[arg(long)]
    no_straighten: bool,

    /// Do not recognise text in pictures (used as their alternative text).
    #[arg(long)]
    no_picture_text: bool,

    /// Worker threads for OCR and layout (default: number of CPUs, at most 8).
    #[arg(long)]
    threads: Option<usize>,

    /// Correct text in the PDF itself instead of converting it: replaces every
    /// occurrence of OLD with NEW (repeatable) and writes the corrected PDF to
    /// --output (default: `<input>-edited.pdf`).
    #[arg(long = "replace", value_name = "OLD=NEW")]
    replace: Vec<String>,

    /// With --replace: only occurrences with the same upper and lower case.
    #[arg(long)]
    match_case: bool,

    /// With --replace: only whole words.
    #[arg(long)]
    whole_word: bool,

    /// With --replace: keep the earlier revisions in the file (an incremental
    /// update, faster for big files) instead of rewriting it without them.
    #[arg(long)]
    keep_history: bool,

    /// Join the input PDFs into one (--output, a .pdf file) instead of converting.
    #[arg(long)]
    merge: bool,

    /// Write these pages (1-based, e.g. `1-3,7`) to a new PDF (--output) instead of converting.
    #[arg(long, value_name = "PAGES")]
    extract_pages: Option<String>,

    /// Split the PDF into files of this many pages (in --output, a folder; default: next to the PDF).
    #[arg(long, value_name = "N")]
    split: Option<usize>,

    /// Remove these pages (writes a PDF, like --replace).
    #[arg(long, value_name = "PAGES")]
    delete_pages: Option<String>,

    /// Turn pages clockwise, e.g. `2-4:90` or `1:180` (repeatable; writes a PDF).
    #[arg(long, value_name = "PAGES:DEGREES")]
    rotate: Vec<String>,

    /// Put the pages in this order, e.g. `3,1,2,4-` (pages not listed are left out; writes a PDF).
    #[arg(long, value_name = "PAGES")]
    page_order: Option<String>,

    /// Path to the PDFium shared library (file or directory).
    #[arg(long)]
    pdfium: Option<PathBuf>,

    /// Only print warnings and errors.
    #[arg(short, long)]
    quiet: bool,
}

/// Operations that write PDFs instead of converting.
fn pdf_tools(args: &Args) -> anyhow::Result<()> {
    use pdf2word_core::pages;
    let pw = args.password.as_deref();
    let is_pdf = |p: &std::path::Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    if args.merge {
        let output = args.output.clone().ok_or_else(|| anyhow::anyhow!("--merge needs --output (a .pdf file)"))?;
        if !is_pdf(&output) {
            bail!("--merge writes a PDF file: {}", output.display());
        }
        let inputs: Vec<(PathBuf, Option<String>)> = args.inputs.iter().map(|p| (p.clone(), args.password.clone())).collect();
        pages::merge(&inputs, &output)?;
        log::info!("wrote {} ({} files joined)", output.display(), inputs.len());
        return Ok(());
    }
    let [input] = args.inputs.as_slice() else { bail!("this works on one PDF at a time") };
    let total = pdf2word_core::inspect(input, pw, args.pdfium.as_deref())?.pages;
    if let Some(n) = args.split {
        let dir = args.output.clone().unwrap_or_else(|| input.parent().map(PathBuf::from).unwrap_or_default());
        std::fs::create_dir_all(&dir)?;
        let parts = pages::split(input, pw, n, &dir)?;
        log::info!("wrote {} file(s) to {}", parts.len(), dir.display());
        return Ok(());
    }
    if let Some(spec) = &args.extract_pages {
        let output = args.output.clone().ok_or_else(|| anyhow::anyhow!("--extract-pages needs --output (a .pdf file)"))?;
        let list = pdf2word_core::parse_page_range(spec, total)?;
        pages::extract(input, pw, &list, &output)?;
        log::info!("wrote {} ({} page(s))", output.display(), list.len());
        return Ok(());
    }
    let output = args.output.clone().unwrap_or_else(|| {
        let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
        input.with_file_name(format!("{stem}-edited.pdf"))
    });
    if !is_pdf(&output) {
        bail!("the output is a PDF file: {}", output.display());
    }
    let total_pages = total;
    let tmp = std::env::temp_dir().join(format!("pdf2word-edit-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let mut current = input.clone();
    let mut total = 0;
    for (i, pair) in args.replace.iter().enumerate() {
        let Some((old, new)) = pair.split_once('=') else { bail!("--replace expects OLD=NEW, got '{pair}'") };
        let next = tmp.join(format!("step{i}.pdf"));
        let opts = pdf2word_core::edit::FindOptions { match_case: args.match_case, whole_word: args.whole_word };
        let (n, report) = pdf2word_core::edit::find_replace(&current, pw, old, new, opts, &next, args.pdfium.as_deref())?;
        for note in report.notes {
            log::warn!("{note}");
        }
        if !args.quiet {
            log::info!("'{old}' → '{new}': {n} occurrence(s) replaced");
        }
        total += n;
        if n > 0 {
            current = next;
        }
    }
    // Page operations, after the text changes.
    let mut ops = Vec::new();
    for r in &args.rotate {
        let Some((pages_spec, deg)) = r.rsplit_once(':') else { bail!("--rotate expects PAGES:DEGREES, got '{r}'") };
        let degrees: i32 = deg.trim().parse().with_context(|| format!("invalid angle in '{r}'"))?;
        ops.push(pages::PageOp::Rotate { pages: pdf2word_core::parse_page_range(pages_spec, total_pages)?, degrees });
    }
    if let Some(spec) = &args.page_order {
        ops.push(pages::PageOp::Reorder { order: pdf2word_core::parse_page_range(spec, total_pages)? });
    }
    if let Some(spec) = &args.delete_pages {
        let mut del = pdf2word_core::parse_page_range(spec, total_pages)?;
        // Page numbers refer to the order after --page-order.
        if let Some(pages::PageOp::Reorder { order }) = ops.last() {
            del.retain(|p| *p < order.len());
        }
        ops.push(pages::PageOp::Delete { pages: del });
    }
    if !ops.is_empty() {
        let next = tmp.join("pages.pdf");
        pages::apply_page_ops(&current, pw, &ops, &next)?;
        current = next;
        total += ops.len();
    }
    if args.keep_history || total == 0 {
        std::fs::copy(&current, &output).with_context(|| format!("cannot write {}", output.display()))?;
    } else {
        pdf2word_core::edit::compact(&current, pw, &output, args.pdfium.as_deref())?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if !args.quiet {
        log::info!("wrote {} ({total} change(s))", output.display());
    }
    Ok(())
}

fn main() {
    pdf2word_core::ocr::limit_openmp_threads();
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
    let page_tools = args.merge || args.split.is_some() || args.extract_pages.is_some() || args.delete_pages.is_some() || !args.rotate.is_empty() || args.page_order.is_some();
    if !args.replace.is_empty() || page_tools {
        return pdf_tools(&args);
    }
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
        straighten_scans: !args.no_straighten,
        picture_text: !args.no_picture_text,
        threads: args.threads,
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
