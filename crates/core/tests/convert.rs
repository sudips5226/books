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
    /// Other XML parts by name (word/footer1.xml, word/numbering.xml …).
    parts: std::collections::HashMap<String, String>,
}

impl Docx {
    fn part(&self, name: &str) -> &str {
        self.parts.get(name).map(|s| s.as_str()).unwrap_or("")
    }
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
    let mut parts = std::collections::HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().to_string();
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let mut s = String::new();
            f.read_to_string(&mut s).unwrap();
            parts.insert(name, s);
        }
    }
    let _ = std::fs::remove_file(&out);
    Docx { document, media, parts }
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
    // The repeated footer is a Word footer with a page-number field after a right tab.
    let footer = d.part("word/footer1.xml");
    assert!(footer.contains("Quarterly Report - Confidential"), "footer missing");
    assert!(footer.contains(r#"<w:tab w:val="right""#));
    assert!(footer.contains(r#"<w:fldSimple w:instr=" PAGE ">"#));
    assert!(!t.contains("Confidential"), "footer text left in the body");
    assert!(d.document.contains("<w:footerReference"));
    // Bullets are a real Word list (marker removed from the text).
    assert!(d.document.contains("<w:numPr>"));
    assert!(d.part("word/numbering.xml").contains(r#"<w:numFmt w:val="bullet"/>"#));
    assert!(!t.contains('•'));
    // Paragraph styles: Normal carries the body font, the caption is styled.
    let styles = d.part("word/styles.xml");
    assert!(styles.contains(r#"w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/><w:rPr>"#));
    assert!(d.document.contains(r#"<w:pStyle w:val="Caption"/>"#));
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

fn run_zip(name: &str) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
    let out = std::env::temp_dir().join(format!("pdf2word-test-{}-{name}-zip.docx", std::process::id()));
    let opts = ConvertOptions { ocr_mode: OcrMode::Never, ..Default::default() };
    convert(&fixture(name), &out, &opts, None, &mut |_| {}).expect("conversion failed");
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap()
}

#[test]
fn width_compensation_for_condensed_and_tracked_text() {
    if !pdfium_available() {
        return;
    }
    let d = run("fonts.pdf", LayoutMode::Flow);
    // Helvetica drawn at 82% width → character scale; letter-spaced Times → spacing.
    assert!(d.document.contains(r#"<w:w w:val="82"/>"#), "no horizontal scaling");
    assert!(d.document.contains("<w:spacing w:val=\""), "no character spacing");
}

#[test]
fn embeds_pdf_fonts() {
    if !pdfium_available() {
        return;
    }
    let mut z = run_zip("embedded.pdf");
    let mut table = String::new();
    z.by_name("word/fontTable.xml").unwrap().read_to_string(&mut table).unwrap();
    assert!(table.contains(r#"<w:font w:name="Fixture Sans">"#), "{table}");
    let key = table.split("w:fontKey=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let mut obf = Vec::new();
    z.by_name("word/fonts/font1.odttf").unwrap().read_to_end(&mut obf).unwrap();
    let font = pdf2word_core::embed::obfuscate(&obf, &key);
    let face = ttf_parser::Face::parse(&font, 0).expect("embedded font must parse");
    assert_eq!(pdf2word_core::metrics::face_families(&face), vec!["Fixture Sans".to_string()]);
    for c in "Layout preservation".chars().filter(|c| !c.is_whitespace()) {
        assert!(face.glyph_index(c).is_some(), "missing glyph for {c:?}");
    }
    let mut settings = String::new();
    z.by_name("word/settings.xml").unwrap().read_to_string(&mut settings).unwrap();
    assert!(settings.contains("<w:embedTrueTypeFonts/>"));
}

#[test]
fn right_to_left_text() {
    if !pdfium_available() {
        return;
    }
    for name in ["rtl.pdf", "rtl-office.pdf"] {
        let d = run(name, LayoutMode::Flow);
        let t = text(&d.document);
        assert!(d.document.contains("<w:bidi/>"), "{name}: no bidi paragraphs");
        assert!(d.document.contains("<w:rtl/>"), "{name}: no rtl runs");
        // Logical (reading) order, not the visual order stored in the PDF.
        assert!(t.contains("מסמך לדוגמה בעברית"), "{name}: {t}");
        assert!(t.contains("English"), "{name}: Latin run lost");
        assert!(t.contains("2024"), "{name}: number reversed");
    }
}

/// A CJK font is available to PDFium (system font directories).
fn fonts_with_cjk() -> bool {
    let dirs = ["/usr/share/fonts", "/usr/local/share/fonts", "/System/Library/Fonts", "/Library/Fonts", "C:\\Windows\\Fonts"];
    fn walk(p: &Path, depth: usize) -> bool {
        let Ok(rd) = std::fs::read_dir(p) else { return false };
        rd.flatten().any(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            if path.is_dir() {
                depth < 4 && walk(&path, depth + 1)
            } else {
                ["cjk", "msgothic", "msmincho", "yugoth", "hiragino", "ipag", "ipam", "takao", "wqy", "droidsansfallback", "notosansjp", "notoserifjp"].iter().any(|k| name.contains(k))
            }
        })
    }
    dirs.iter().any(|d| walk(Path::new(d), 0))
}

#[test]
fn rotated_and_vertical_text_boxes() {
    if !pdfium_available() {
        return;
    }
    let d = run("rotated.pdf", LayoutMode::Flow);
    let t = text(&d.document);
    // The Japanese column uses a non-embedded CID font: PDFium can only place
    // its glyphs when a CJK font is installed (e.g. fonts-noto-cjk).
    let cjk = !t.is_empty() && fonts_with_cjk();
    if !cjk {
        eprintln!("no CJK font installed: the vertical Japanese column is not checked");
    }
    assert_eq!(d.document.matches("<wps:wsp>").count(), if cjk { 3 } else { 2 }, "expected sidebar, watermark (and vertical column)");
    assert!(d.document.contains(r#"vert="vert270""#), "90° sidebar should use vert270");
    if cjk {
        assert!(d.document.contains(r#"vert="eaVert""#), "Japanese column should be eaVert");
        assert!(t.contains("縦書きの日本語"), "missing the Japanese column");
    }
    assert!(d.document.contains(r#"<a:xfrm rot="19800000">"#), "30° watermark rotation");
    for s in ["Confidential sidebar label", "DRAFT"] {
        assert!(t.contains(s), "missing {s}");
    }
    // The body text stays ordinary horizontal text.
    assert!(t.contains("Body text stays horizontal."));
}

#[test]
fn equations_become_office_math() {
    if !pdfium_available() {
        return;
    }
    let d = run("math.pdf", LayoutMode::Flow);
    let doc = &d.document;
    assert!(doc.matches("<m:oMath>").count() >= 6, "inline and display equations");
    for tag in ["<m:f>", "<m:rad>", "<m:nary>", "<m:sSup>", "<m:sSub>"] {
        assert!(doc.contains(tag), "missing {tag}");
    }
    // Fraction of the quadratic formula: numerator with a radical, denominator 2a.
    assert!(doc.contains(r#"<m:den><m:r><w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/>"#));
    // Matrix and case distinction are kept as pictures.
    assert_eq!(d.media, 2);
    // Surrounding text is untouched.
    assert!(text(doc).contains("Normal text continues after the equations."));
}

#[cfg(feature = "ocr")]
#[test]
fn ocr_detects_bold_italic_and_font_class() {
    use pdf2word_core::model::{Generic, RawPage};
    use pdf2word_core::ocr::{words_to_glyphs, OcrEngine, OcrSettings};

    if !pdfium_available() {
        return;
    }
    let Ok(mut engine) = OcrEngine::new(OcrSettings::default()) else { return };
    let pdfium = pdf2word_core::load_pdfium(None).unwrap();
    let doc = pdfium.load_pdf_from_file(&fixture("typography.pdf"), None).unwrap();
    let page = doc.pages().get(0).unwrap();
    let img = pdf2word_core::extract::render_page(&page, 200.0).unwrap();
    let words = engine.recognize(&img, 200.0).unwrap();
    let mut raw = RawPage { width: page.width().value, height: page.height().value, ..Default::default() };
    words_to_glyphs(&mut raw, &words, &img, engine.settings());
    // Style of the first glyph of each line, keyed by the line's first word.
    let mut lines: Vec<(f32, String, (Generic, bool, bool))> = Vec::new();
    for g in &raw.glyphs {
        if !lines.iter().any(|(b, _, _)| (b - g.baseline).abs() < 3.0) {
            let st = &raw.styles[g.style];
            let word: String = raw
                .glyphs
                .iter()
                .filter(|o| (o.baseline - g.baseline).abs() < 3.0 && o.bbox.x0 < g.bbox.x0 + 60.0)
                .map(|o| o.ch)
                .collect();
            lines.push((g.baseline, word, (st.generic, st.bold, st.italic)));
        }
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    let expected = [
        (Generic::Serif, false, false),
        (Generic::Serif, true, false),
        (Generic::Serif, false, true),
        (Generic::Sans, false, false),
        (Generic::Sans, true, false),
        (Generic::Sans, false, true),
        (Generic::Mono, false, false),
        (Generic::Mono, true, false),
    ];
    assert_eq!(lines.len(), expected.len(), "{lines:?}");
    for ((_, word, got), want) in lines.iter().zip(expected) {
        assert_eq!(*got, want, "line starting {word:?}");
    }
}

#[test]
fn headers_footers_lists_and_footnotes() {
    if !pdfium_available() {
        return;
    }
    let d = run("structure.pdf", LayoutMode::Flow);
    let t = text(&d.document);
    // Header on pages 2-3 only: a distinct, empty first-page header; the footer is on every page.
    assert!(d.part("word/header1.xml").contains("Annual Review 2025"));
    assert!(!d.part("word/header2.xml").contains("Annual Review"));
    assert!(d.part("word/footer2.xml").contains(r#"w:instr=" PAGE ""#), "first-page footer must keep the page number");
    assert!(d.document.contains("<w:titlePg/>"));
    assert_eq!(d.document.matches("<w:titlePg/>").count(), 1, "only the first section has a title page");
    let footer = d.part("word/footer1.xml");
    assert!(footer.contains(r#"w:instr=" PAGE ""#) && footer.contains(r#"w:instr=" NUMPAGES ""#));
    assert!(!t.contains("Annual Review") && !t.contains("of 3"), "header/footer text left in the body");
    // Nested numbered list: decimal level 0, lower-letter level 1, numbering continues after the nested items.
    let numbering = d.part("word/numbering.xml");
    assert!(numbering.contains(r#"<w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>"#));
    assert!(numbering.contains(r#"<w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/>"#));
    assert!(d.document.contains(r#"<w:ilvl w:val="1"/>"#));
    assert!(!t.contains("a)") && !t.contains("3. Third"), "typed list markers left in the text");
    // Footnotes: six notes, referenced from the body, removed from the page bottom.
    let notes = d.part("word/footnotes.xml");
    assert!(notes.contains("Source for the first fact on page 1."));
    assert!(notes.contains("Explanation of the second claim on page 3."));
    assert!(d.document.contains(r#"<w:footnoteReference w:id="6"/>"#));
    assert!(!t.contains("Source for the first fact"));
    assert!(d.part("word/settings.xml").contains("<w:footnotePr>"));
}

#[test]
fn booktabs_and_header_rows() {
    if !pdfium_available() {
        return;
    }
    let d = run("tables.pdf", LayoutMode::Flow);
    let t = text(&d.document);
    // The booktabs table (horizontal rules only) is one table with its header row inside.
    let first_table = &d.document[d.document.find("<w:tbl>").unwrap()..d.document.find("</w:tbl>").unwrap()];
    assert!(text(first_table).contains("Accuracy") && text(first_table).contains("0.903"));
    assert_eq!(first_table.matches("<w:tr>").count(), 5);
    assert!(!first_table.contains(r#"<w:left w:val="single""#), "booktabs tables have no vertical borders");
    // Numeric columns are right-aligned.
    assert!(first_table.contains(r#"<w:jc w:val="right"/>"#));
    // The shaded, bold first row of the inventory repeats on every page.
    assert!(d.document.contains("<w:tblHeader/>"));
    assert!(d.document.contains(r#"<w:shd w:val="clear" w:color="auto" w:fill="D9E2F3"/></w:tcPr>"#), "header shading belongs to the cells");
    assert!(t.contains("A-045"));
}

#[test]
fn continuous_mode_joins_pages_and_columns() {
    if !pdfium_available() {
        return;
    }
    // A table broken over two pages becomes one table; the repeated header row is dropped.
    let d = run("tables.pdf", LayoutMode::Continuous);
    assert_eq!(d.document.matches("<w:tbl>").count(), 2);
    assert_eq!(text(&d.document).matches("Description").count(), 1);
    assert!(d.document.contains("<w:tblHeader/>"));
    // No page-per-section structure.
    assert_eq!(d.document.matches("<w:sectPr>").count(), 1);

    // Two text columns become a Word column section; paragraphs split by a column or page break are joined.
    let d = run("twocol.pdf", LayoutMode::Continuous);
    assert!(d.document.contains(r#"<w:cols w:num="2""#));
    assert!(d.document.contains(r#"<w:type w:val="continuous"/>"#));
    let paragraphs: Vec<String> = d.document.split("</w:p>").map(text).collect();
    for i in 1..=14 {
        let start = format!("Section {i}.");
        let p = paragraphs.iter().find(|p| p.contains(&start)).unwrap_or_else(|| panic!("{start} missing"));
        assert!(p.trim_end().ends_with("separate lines."), "{start} was not joined: {p}");
    }
}

#[test]
fn outline_links_toc_comments_forms_and_shapes() {
    if !pdfium_available() {
        return;
    }
    let d = run("annotated.pdf", LayoutMode::Flow);
    let doc = &d.document;
    // Outline entries are headings with bookmarks; "2.1" is one level down.
    assert!(doc.contains(r#"<w:pStyle w:val="Heading2"/>"#));
    assert_eq!(doc.matches(r#"<w:bookmarkStart w:id="#).count(), 4);
    // The table of contents: TOC field, dot leaders, entries linked to the headings.
    assert!(doc.contains(r#"<w:instrText xml:space="preserve"> TOC \o &quot;1-3&quot; \h \z \u </w:instrText>"#));
    assert_eq!(doc.matches(r#"w:leader="dot""#).count(), 4);
    assert_eq!(doc.matches(r#"<w:hyperlink w:anchor="_Toc"#).count(), 4);
    assert!(doc.contains(r#"<w:pStyle w:val="TOC2"/>"#));
    // Review: comments with authors, the highlight as shading.
    let comments = d.part("word/comments.xml");
    assert!(comments.contains(r#"w:author="Reviewer""#) && comments.contains("Is this supported by the data?"));
    assert!(comments.contains("Please add the instrument model.") && comments.contains("Check the week 3 value."));
    assert_eq!(doc.matches("<w:commentReference").count(), 3);
    assert!(doc.contains(r#"w:fill="FFFF00""#));
    // Form fields are content controls with their values.
    assert!(doc.contains(r#"<w:alias w:val="name"/>"#) && text(doc).contains("Ada Lovelace"));
    assert!(doc.contains(r#"<w14:checked w14:val="1"/>"#));
    assert!(doc.contains(r#"<w:listItem w:displayText="Sweden" w:value="Sweden"/>"#));
    // The bar chart is a group of editable shapes (with the picture as fallback);
    // the reviewer's rectangle is a picture over the page.
    assert!(doc.contains("<wpg:wgp>") && doc.contains("<mc:Fallback>"));
    assert!(doc.matches("<a:custGeom>").count() >= 6);
    assert!(doc.contains("Week 3"));
    assert!(doc.contains(r#"relativeHeight="2517"#));
}

#[test]
fn ocr_straightens_turned_and_skewed_scans() {
    use pdf2word_core::model::RawPage;
    use pdf2word_core::ocr::{process_scan, OcrEngine, OcrSettings};
    use pdf2word_core::scan;

    if !pdfium_available() {
        return;
    }
    let Ok(mut engine) = OcrEngine::new(OcrSettings::default()) else { return };
    let pdfium = pdf2word_core::load_pdfium(None).unwrap();
    let doc = pdfium.load_pdf_from_file(&fixture("report.pdf"), None).unwrap();
    let page = doc.pages().get(0).unwrap();
    let dpi = 150.0;
    let upright = pdf2word_core::extract::render_page(&page, dpi).unwrap();
    // Scanned sideways and slightly crooked.
    let scan_img = scan::rotate(&scan::quarter_turn(&upright, 1), 2.0);
    let (w, h) = (page.width().value, page.height().value);
    let mut raw = RawPage { width: h, height: w, ..Default::default() };
    let rep = process_scan(&mut engine, &mut raw, scan_img, dpi, &Default::default(), true).unwrap();
    assert_eq!(rep.quarter_turns, 3, "{rep:?}");
    assert!((rep.deskew + 2.0).abs() < 0.5, "{rep:?}");
    assert_eq!((raw.width, raw.height), (w, h), "page not turned upright");
    let text: String = raw.glyphs.iter().map(|g| g.ch).collect();
    assert!(text.contains("ExecutiveSummary") || text.contains("Executive"), "{text}");
}

#[test]
fn parallel_ocr_with_automatic_language() {
    if !pdfium_available() || pdf2word_core::ocr::OcrEngine::new(Default::default()).is_err() {
        return;
    }
    let out = std::env::temp_dir().join(format!("pdf2word-test-{}-parallel-ocr.docx", std::process::id()));
    let mut opts = ConvertOptions { ocr_mode: OcrMode::Always, threads: Some(2), ..Default::default() };
    opts.ocr.language = "auto".into();
    let report = convert(&fixture("report.pdf"), &out, &opts, None, &mut |_| {}).expect("conversion failed");
    assert_eq!(report.ocr_pages, vec![1, 2]);
    assert_eq!(report.ocr_language.as_deref(), Some("eng"));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut doc = String::new();
    zip.by_name("word/document.xml").unwrap().read_to_string(&mut doc).unwrap();
    let t = text(&doc);
    assert!(t.contains("Executive Summary") && t.contains("Outlook"), "{t}");
    // Pages keep their order.
    assert!(t.find("Executive Summary").unwrap() < t.find("Outlook").unwrap());
    let _ = std::fs::remove_file(&out);
}
