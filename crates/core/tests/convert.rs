//! End-to-end conversion tests on generated sample PDFs
//! (see `tools/make_test_pdfs.py`). They need the PDFium library; when it
//! cannot be found (set `PDFIUM_DYNAMIC_LIB_PATH`) the tests are skipped.

use std::io::Read;
use std::path::{Path, PathBuf};

use pdf2word_core::{convert, ConvertOptions, LayoutMode, OcrMode};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn pdfium_available() -> bool {
    match pdf2word_core::load_pdfium(None) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping: {e}");
            false
        }
    }
}

struct Docx {
    document: String,
    media: usize,
}

fn run(name: &str, mode: LayoutMode) -> Docx {
    let out = std::env::temp_dir().join(format!("pdf2word-test-{}-{name}-{mode:?}.docx", std::process::id()));
    let opts = ConvertOptions { layout_mode: mode, ocr_mode: OcrMode::Never, ..Default::default() };
    convert(&fixture(name), &out, &opts, None, &mut |_| {}).expect("conversion failed");
    let file = std::fs::File::open(&out).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut document = String::new();
    zip.by_name("word/document.xml").unwrap().read_to_string(&mut document).unwrap();
    let media = (0..zip.len()).filter(|&i| zip.by_index(i).unwrap().name().starts_with("word/media/")).count();
    let _ = std::fs::remove_file(&out);
    Docx { document, media }
}

/// Concatenated text of the document (runs only).
fn text(doc: &str) -> String {
    let mut out = String::new();
    let mut rest = doc;
    while let Some(i) = rest.find("<w:t xml:space=\"preserve\">") {
        rest = &rest[i + 26..];
        let end = rest.find("</w:t>").unwrap();
        out.push_str(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

#[test]
fn report_structure_and_formatting() {
    if !pdfium_available() {
        return;
    }
    let d = run("report.pdf", LayoutMode::Flow);
    let t = text(&d.document);
    assert!(t.contains("Quarterly Business Report"));
    // Heading styles from font sizes.
    assert!(d.document.contains(r#"<w:pStyle w:val="Heading1"/>"#));
    // Ruled table with a merged cell.
    assert!(d.document.contains("<w:tbl>"));
    assert!(d.document.contains(r#"<w:gridSpan w:val="2"/>"#));
    assert!(t.contains("Not reported"));
    // Formatting.
    assert!(d.document.contains("<w:b/>"));
    assert!(d.document.contains("<w:i/>"));
    assert!(d.document.contains(r#"<w:u w:val="single"/>"#));
    assert!(d.document.contains(r#"<w:color w:val="C00000"/>"#) || d.document.contains(r#"<w:color w:val="BF0000"/>"#));
    assert!(d.document.contains("<w:position w:val="), "superscript/subscript shift missing");
    assert!(d.document.contains("<w:hyperlink"));
    // Justified paragraphs and first-line indents.
    assert!(d.document.contains(r#"<w:jc w:val="both"/>"#));
    assert!(d.document.contains(r#"w:firstLine="480""#));
    // List items with hanging indent.
    assert!(d.document.contains("w:hanging="));
    // Logo + rasterised chart.
    assert_eq!(d.media, 2);
    // One section per page.
    assert_eq!(d.document.matches("<w:sectPr>").count(), 2);
    // Footer with a right tab stop for the page number.
    assert!(d.document.contains(r#"<w:tab w:val="right""#));
    // Reading order.
    let a = t.find("Executive Summary").unwrap();
    let b = t.find("Financial Results").unwrap();
    let c = t.find("Outlook").unwrap();
    assert!(a < b && b < c);
}

#[test]
fn two_column_reading_order() {
    if !pdfium_available() {
        return;
    }
    let d = run("twocol.pdf", LayoutMode::Flow);
    let t = text(&d.document);
    let positions: Vec<usize> = (1..=14).map(|i| t.find(&format!("Section {i}.")).unwrap_or_else(|| panic!("Section {i} missing"))).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "columns read out of order");
    // Columns are laid out side by side.
    assert!(d.document.contains("<w:gridCol"));
}

#[test]
fn invoice_alignment_and_table() {
    if !pdfium_available() {
        return;
    }
    let d = run("invoice.pdf", LayoutMode::Flow);
    let t = text(&d.document);
    for s in ["INVOICE", "Invoice #: 2024-0042", "Consulting services", "$2,579.50", "Thank you for your business!"] {
        assert!(t.contains(s), "missing {s}");
    }
    assert!(d.document.contains(r#"<w:jc w:val="right"/>"#));
    assert!(d.document.contains(r#"<w:jc w:val="center"/>"#));
    // Address lines keep their line breaks.
    assert!(d.document.contains("<w:br/>"));
}

#[test]
fn exact_mode_uses_frames() {
    if !pdfium_available() {
        return;
    }
    let d = run("report.pdf", LayoutMode::Exact);
    assert!(d.document.contains("<w:framePr"));
    assert!(d.document.contains("<wp:anchor"));
    assert!(d.document.contains("<w:tblpPr"));
}

#[test]
fn page_selection() {
    if !pdfium_available() {
        return;
    }
    let out = std::env::temp_dir().join(format!("pdf2word-test-{}-pages.docx", std::process::id()));
    let opts = ConvertOptions { pages: Some("2".into()), ocr_mode: OcrMode::Never, ..Default::default() };
    let report = convert(&fixture("report.pdf"), &out, &opts, None, &mut |_| {}).unwrap();
    assert_eq!(report.pages, 1);
    let _ = std::fs::remove_file(&out);
}

/// OCR pipeline on a rendered page (no text layer involved): words, table
/// rulings recovered from pixels, and layout analysis.
#[cfg(feature = "ocr")]
#[test]
fn ocr_pipeline_on_rendered_page() {
    use pdf2word_core::model::{Block, RawPage};
    use pdf2word_core::ocr::{detect_rules, words_to_glyphs, OcrEngine, OcrSettings};

    if !pdfium_available() {
        return;
    }
    let mut engine = match OcrEngine::new(OcrSettings::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let pdfium = pdf2word_core::load_pdfium(None).unwrap();
    let doc = pdfium.load_pdf_from_file(&fixture("report.pdf"), None).unwrap();
    let page = doc.pages().get(0).unwrap();
    let dpi = 200.0;
    let img = pdf2word_core::extract::render_page(&page, dpi).unwrap();
    let words = engine.recognize(&img, dpi).unwrap();
    let mut raw = RawPage { width: page.width().value, height: page.height().value, ..Default::default() };
    words_to_glyphs(&mut raw, &words, &img, engine.settings());
    raw.rules = detect_rules(&img, dpi, &words);
    assert!(raw.rules.len() >= 8, "table rulings not detected: {}", raw.rules.len());

    let layout = pdf2word_core::layout::analyse_page(&raw, &Default::default());
    let mut all = String::new();
    let mut tables = 0;
    for b in &layout.blocks {
        match b {
            Block::Paragraph(p) => all.push_str(&p.plain_text()),
            Block::Table(t) if !t.layout_only => tables += 1,
            _ => {}
        }
        all.push('\n');
    }
    assert!(all.contains("Quarterly Business Report"), "{all}");
    assert!(all.contains("Executive Summary"));
    assert_eq!(tables, 1, "scanned table not reconstructed");
}
