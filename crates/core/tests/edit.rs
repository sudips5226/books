//! Editing text in PDFs: changed runs, untouched content, fonts.
//! Skipped when the PDFium library cannot be found (set `PDFIUM_DYNAMIC_LIB_PATH`).

use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, Object, Stream};
use pdf2word_core::edit::{
    apply_edits, compact, find_replace, find_text, outlined_text, page_text, replace_matches, text_items, FindOptions, ImageChange, NewTextStyle, StyleChange,
    TextEdit, TextItem,
};

fn pdfium_available() -> bool {
    match pdf2word_core::load_pdfium(None) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("skipping: {e}");
            false
        }
    }
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("pdf2word-edit-{}-{name}", std::process::id()))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// A one-page PDF drawing `content` with Helvetica as /F1.
fn make_pdf(name: &str, content: &str, rotate: i64) -> PathBuf {
    make_pdf_with(name, content, rotate, &[])
}

/// Like [`make_pdf`], with form XObjects (name, content) that use /F1 too.
fn make_pdf_with(name: &str, content: &str, rotate: i64, forms: &[(&str, &str)]) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
    });
    let mut xobjects = lopdf::Dictionary::new();
    for (n, c) in forms {
        let id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
            },
            c.as_bytes().to_vec(),
        ));
        xobjects.set(n.as_bytes().to_vec(), id);
    }
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id }, "XObject" => xobjects },
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Rotate" => rotate,
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 }));
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let path = tmp(name);
    doc.save(&path).unwrap();
    path
}

fn page_content(path: &Path) -> String {
    let doc = Document::load(path).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    String::from_utf8_lossy(&doc.get_page_content(id)).into_owned()
}

fn item<'a>(items: &'a [TextItem], text: &str) -> &'a TextItem {
    items.iter().find(|i| i.text.trim() == text).unwrap_or_else(|| panic!("no run {text:?} in {:?}", items.iter().map(|i| &i.text).collect::<Vec<_>>()))
}

fn texts(items: &[TextItem]) -> Vec<String> {
    items.iter().map(|i| i.text.trim().to_string()).collect()
}

const LINES: &str = "BT /F1 12 Tf 72 700 Td [(Hello)-600(world)] TJ /F1 10 Tf (, again) Tj 0 -20 Td /F1 12 Tf [(Second)-600(line)] TJ ET";

#[test]
fn replacing_a_word_keeps_the_rest_of_the_page() {
    if !pdfium_available() {
        return;
    }
    let input = make_pdf("lines.pdf", LINES, 0);
    let items = text_items(&input, None, 0, None).unwrap();
    assert_eq!(texts(&items), vec!["Hello world", ", again", "Second line"]);
    let hello = item(&items, "Hello world");
    let out = tmp("lines-replaced.pdf");
    let report = apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: hello.objects.clone(), text: "Hello there".into(), fit: false }], &out, None).unwrap();
    assert!(report.notes.is_empty(), "{:?}", report.notes);

    let after = text_items(&out, None, 0, None).unwrap();
    assert_eq!(texts(&after), vec!["Hello there", ", again", "Second line"]);
    // The unchanged start keeps its exact kerning; the other line is untouched.
    let content = page_content(&out);
    assert!(content.contains("[(Second)-600(line)] TJ"), "{content}");
    assert!(content.contains("<48656C6C6F> -600 <7468657265>"), "{content}");
    // The text after the run follows its new end: "there" is narrower than "world".
    let (before_again, after_again) = (item(&items, ", again"), item(&after, ", again"));
    let shift = (after_again.x - before_again.x) * 612.0;
    assert!(shift < -0.5 && shift > -2.5, "shift {shift}");
}

#[test]
fn deleting_text_leaves_the_rest_in_place() {
    if !pdfium_available() {
        return;
    }
    let input = make_pdf("lines-del.pdf", LINES, 0);
    let items = text_items(&input, None, 0, None).unwrap();
    let out = tmp("lines-deleted.pdf");
    apply_edits(&input, None, &[TextEdit::Delete { page: 0, objects: item(&items, "Hello world").objects.clone() }], &out, None).unwrap();
    let after = text_items(&out, None, 0, None).unwrap();
    assert_eq!(texts(&after), vec![", again", "Second line"]);
    let moved = (item(&after, ", again").x - item(&items, ", again").x).abs() * 612.0;
    assert!(moved < 0.2, "the following text moved by {moved}pt");
}

#[test]
fn runs_of_single_glyphs_are_edited_as_one() {
    if !pdfium_available() {
        return;
    }
    // One object per glyph, as some producers write them.
    let input = make_pdf("glyphs.pdf", "BT /F1 12 Tf 1 0 0 1 72 600 Tm (A) Tj 1 0 0 1 80.004 600 Tm (B) Tj 1 0 0 1 88.008 600 Tm (C) Tj ET", 0);
    let items = text_items(&input, None, 0, None).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].objects.len(), 3);
    assert_eq!(items[0].text, "ABC");
    let out = tmp("glyphs-replaced.pdf");
    apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: items[0].objects.clone(), text: "ABCD".into(), fit: false }], &out, None).unwrap();
    let after = text_items(&out, None, 0, None).unwrap();
    assert_eq!(texts(&after), vec!["ABCD"]);
    assert!((after[0].x - items[0].x).abs() < 0.002);
}

#[test]
fn fit_condenses_longer_text() {
    if !pdfium_available() {
        return;
    }
    let input = make_pdf("fit.pdf", "BT /F1 12 Tf 72 700 Td (Short) Tj ET", 0);
    let items = text_items(&input, None, 0, None).unwrap();
    let text = "Shorter";
    let width = |fit: bool| {
        let out = tmp(&format!("fit-{fit}.pdf"));
        apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: items[0].objects.clone(), text: text.into(), fit }], &out, None).unwrap();
        let after = text_items(&out, None, 0, None).unwrap();
        assert_eq!(texts(&after), vec![text]);
        after[0].w * 612.0
    };
    let (natural, fitted, original) = (width(false), width(true), items[0].w * 612.0);
    assert!(natural > original * 1.25, "{natural} {original}");
    assert!((fitted - original).abs() < 1.5, "condensed to {fitted}, original {original}");
    // At most to half the width.
    let long = "A considerably longer replacement";
    let out = tmp("fit-long.pdf");
    apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: items[0].objects.clone(), text: long.into(), fit: true }], &out, None).unwrap();
    let w = text_items(&out, None, 0, None).unwrap()[0].w * 612.0;
    assert!(w > original * 1.5, "{w}");
}

#[test]
fn new_text_and_missing_glyphs_use_installed_fonts() {
    if !pdfium_available() {
        return;
    }
    if pdf2word_core::metrics::installed_families().is_empty() {
        eprintln!("skipping: no installed fonts");
        return;
    }
    let input = make_pdf("insert.pdf", LINES, 0);
    let out = tmp("insert-out.pdf");
    let insert = TextEdit::Insert {
        page: 0,
        x: 0.1,
        y: 0.1,
        text: "Inserted note".into(),
        size: 14.0,
        color: [200, 0, 0],
        style: NewTextStyle::default(),
    };
    apply_edits(&input, None, &[insert], &out, None).unwrap();
    let after = text_items(&out, None, 0, None).unwrap();
    let note = item(&after, "Inserted note");
    assert!((note.x - 0.1).abs() < 0.01 && (note.y - 0.1).abs() < 0.02, "{note:?}");
    assert_eq!(note.color, [200, 0, 0]);
    assert!(texts(&after).contains(&"Hello world".to_string()));

    // The inserted font is embedded; it has only been used for these letters,
    // so a word made of them keeps it and other letters need another font.
    let out2 = tmp("insert-out2.pdf");
    let r = apply_edits(&out, None, &[TextEdit::Replace { page: 0, objects: note.objects.clone(), text: "Inserted tone".into(), fit: false }], &out2, None).unwrap();
    assert!(r.notes.is_empty(), "{:?}", r.notes);
    let after2 = text_items(&out2, None, 0, None).unwrap();
    assert!(texts(&after2).contains(&"Inserted tone".to_string()), "{:?}", texts(&after2));

    // Helvetica has no Ω in its encoding: the letter comes from an installed font.
    let hello = item(&after2, "Hello world");
    let out3 = tmp("insert-out3.pdf");
    match apply_edits(&out2, None, &[TextEdit::Replace { page: 0, objects: hello.objects.clone(), text: "Hello Ωmega".into(), fit: false }], &out3, None) {
        Ok(r) => {
            assert!(!r.notes.is_empty());
            let t = texts(&text_items(&out3, None, 0, None).unwrap()).join(" ");
            assert!(t.contains("Hello") && t.contains("Ωmega"), "{t}");
        }
        Err(e) => assert!(e.to_string().contains("no installed font"), "{e}"),
    }
}

#[test]
fn text_on_rotated_pages_is_added_upright() {
    if !pdfium_available() || pdf2word_core::metrics::installed_families().is_empty() {
        return;
    }
    let input = make_pdf("rotated-page.pdf", LINES, 90);
    let out = tmp("rotated-page-out.pdf");
    let insert = TextEdit::Insert { page: 0, x: 0.1, y: 0.1, text: "Upright".into(), size: 20.0, color: [0, 0, 0], style: NewTextStyle::default() };
    apply_edits(&input, None, &[insert], &out, None).unwrap();
    let after = text_items(&out, None, 0, None).unwrap();
    let it = item(&after, "Upright");
    // Wider than tall on the displayed (landscape) page, where it was put.
    assert!(it.w * 792.0 > it.h * 612.0 * 2.0, "{it:?}");
    assert!((it.x - 0.1).abs() < 0.01 && (it.y - 0.1).abs() < 0.03, "{it:?}");
}

#[test]
fn find_and_replace_then_convert() {
    if !pdfium_available() {
        return;
    }
    let out = tmp("report-edited.pdf");
    let (n, _) = find_replace(&fixture("report.pdf"), None, "Quarterly", "Annual", FindOptions::default(), &out, None).unwrap();
    assert!(n >= 2, "{n}");
    let items = text_items(&out, None, 0, None).unwrap();
    assert!(texts(&items).contains(&"Annual Business Report".to_string()), "{:?}", texts(&items));
    assert!(!texts(&items).iter().any(|t| t.contains("Quarterly")));
    // The corrected PDF converts like any other.
    let docx = tmp("report-edited.docx");
    let opts = pdf2word_core::ConvertOptions { ocr_mode: pdf2word_core::OcrMode::Never, ..Default::default() };
    pdf2word_core::convert(&out, &docx, &opts, None, &mut |_| {}).unwrap();
    let mut xml = String::new();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&docx).unwrap()).unwrap();
    std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml).unwrap();
    assert!(xml.contains("Annual"));
}

#[test]
fn right_to_left_text_is_edited() {
    if !pdfium_available() {
        return;
    }
    let input = fixture("rtl-office.pdf");
    let items = text_items(&input, None, 0, None).unwrap();
    let hebrew = items.iter().find(|i| i.rtl && i.text.chars().any(|c| ('\u{5d0}'..='\u{5ea}').contains(&c))).unwrap();
    let out = tmp("rtl.pdf");
    apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: hebrew.objects.clone(), text: "מסמך חדש".into(), fit: false }], &out, None).unwrap();
    let after = texts(&text_items(&out, None, 0, None).unwrap());
    assert!(after.contains(&"מסמך חדש".to_string()), "{after:?}");
    // Arabic needs shaping: it comes from an installed font when there is one.
    if pdf2word_core::metrics::installed_families().is_empty() {
        return;
    }
    let out2 = tmp("rtl-ar.pdf");
    let insert = TextEdit::Insert { page: 0, x: 0.1, y: 0.9, text: "مرحبا بالعالم".into(), size: 14.0, color: [0, 0, 0], style: NewTextStyle::default() };
    if apply_edits(&out, None, &[insert], &out2, None).is_ok() {
        let all = texts(&text_items(&out2, None, 0, None).unwrap()).join(" ");
        assert!(all.contains("مرحبا") && all.contains("بالعالم"), "{all}");
    }
}

/// Three lines of a paragraph (Helvetica 10, 14pt apart) and a line below it.
const PARA: &str = "BT /F1 10 Tf 72 700 Td (The quick brown fox jumps over the lazy dog and keeps) Tj 0 -14 Td (running through the long grass of the meadow until the) Tj 0 -14 Td (evening comes.) Tj 0 -40 Td (A following paragraph.) Tj ET";

#[test]
fn paragraphs_rewrap_and_push_text_down() {
    if !pdfium_available() {
        return;
    }
    let input = make_pdf("para.pdf", PARA, 0);
    let pt = page_text(&input, None, 0, None).unwrap();
    let para = pt.paragraphs.iter().find(|p| p.text.starts_with("The quick")).unwrap();
    assert_eq!(para.lines, 3);
    assert!(para.text.ends_with("evening comes."), "{}", para.text);
    let objects = para.items.iter().flat_map(|i| pt.items[*i].objects.clone()).collect();
    let text = format!("{} It was a very long day for everybody involved, and the fox was tired.", para.text);
    let out = tmp("para-out.pdf");
    apply_edits(&input, None, &[TextEdit::Paragraph { page: 0, objects, text: text.clone(), fit: false, push_down: true }], &out, None).unwrap();
    let after = page_text(&out, None, 0, None).unwrap();
    let p2 = after.paragraphs.iter().find(|p| p.text.starts_with("The quick")).unwrap();
    assert!(p2.lines >= 4, "{} lines: {}", p2.lines, p2.text);
    assert_eq!(p2.text.split_whitespace().collect::<Vec<_>>(), text.split_whitespace().collect::<Vec<_>>());
    // The paragraph below moved down by the added lines.
    let below = |pt: &pdf2word_core::edit::PageText| pt.items.iter().find(|i| i.text == "A following paragraph.").unwrap().y * 792.0;
    let moved = below(&after) - below(&pt);
    assert!(moved > 10.0, "moved {moved}");
}

#[test]
fn runs_are_restyled_and_moved() {
    if !pdfium_available() {
        return;
    }
    let input = make_pdf("style.pdf", LINES, 0);
    let items = text_items(&input, None, 0, None).unwrap();
    let hello = item(&items, "Hello world");
    let second = item(&items, "Second line");
    let out = tmp("style-out.pdf");
    let change = StyleChange { bold: Some(true), size: Some(18.0), color: Some([200, 0, 0]), ..Default::default() };
    let edits = [
        TextEdit::Restyle { page: 0, objects: hello.objects.clone(), text: None, change, fit: false },
        TextEdit::Move { page: 0, objects: second.objects.clone(), dx: 0.1, dy: 0.05 },
    ];
    apply_edits(&input, None, &edits, &out, None).unwrap();
    let after = text_items(&out, None, 0, None).unwrap();
    let h = item(&after, "Hello world");
    assert!(h.bold, "{h:?}");
    assert!((h.size - 18.0).abs() < 0.6, "{h:?}");
    assert_eq!(h.color, [200, 0, 0]);
    let s2 = item(&after, "Second line");
    assert!((s2.x - second.x - 0.1).abs() < 0.003 && (s2.y - second.y - 0.05).abs() < 0.003, "{s2:?} {second:?}");
    // Moving keeps the exact glyphs and spacing.
    assert!(page_content(&out).contains("<5365636F6E64> -600 <6C696E65>"), "{}", page_content(&out));
}

#[test]
fn text_inside_forms_is_edited_where_it_is_used() {
    if !pdfium_available() {
        return;
    }
    // The same form is drawn twice; only the first copy is edited.
    let content = "q 1 0 0 1 0 0 cm /X1 Do Q q 1 0 0 1 0 -100 cm /X1 Do Q";
    let input = make_pdf_with("form.pdf", content, 0, &[("X1", "BT /F1 12 Tf 72 700 Td (Inside a form) Tj ET")]);
    let items = text_items(&input, None, 0, None).unwrap();
    assert_eq!(texts(&items), vec!["Inside a form", "Inside a form"]);
    assert_eq!(items[0].objects[0].len(), 2, "{:?}", items[0].objects);
    let out = tmp("form-out.pdf");
    apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: items[0].objects.clone(), text: "Inside an edited form".into(), fit: false }], &out, None).unwrap();
    let after = text_items(&out, None, 0, None).unwrap();
    assert_eq!(texts(&after), vec!["Inside an edited form", "Inside a form"]);
    assert!((after[0].y - items[0].y).abs() < 0.002);
}

#[test]
fn find_and_replace_across_lines() {
    if !pdfium_available() {
        return;
    }
    let content = "BT /F1 10 Tf 72 700 Td (One morning the quick brown) Tj 0 -14 Td (fox found a new conver-) Tj 0 -14 Td (sation partner in the old barn.) Tj ET";
    let input = make_pdf("find.pdf", content, 0);
    let opts = FindOptions::default();
    let found = find_text(&input, None, "brown fox", opts, None).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].found, "brown fox");
    assert_eq!(find_text(&input, None, "conversation", opts, None).unwrap().len(), 1);
    assert_eq!(find_text(&input, None, "Quick", FindOptions { match_case: true, ..opts }, None).unwrap().len(), 0);
    assert_eq!(find_text(&input, None, "the", FindOptions { whole_word: true, ..opts }, None).unwrap().len(), 2);
    let out = tmp("find-out.pdf");
    replace_matches(&input, None, &found, "red cat", &out, None).unwrap();
    let all = texts(&text_items(&out, None, 0, None).unwrap()).join(" ");
    assert!(all.contains("quick red cat") && !all.contains("brown") && !all.contains("fox"), "{all}");
    let out2 = tmp("find-out2.pdf");
    let (n, _) = find_replace(&out, None, "conversation", "chat", opts, &out2, None).unwrap();
    assert_eq!(n, 1);
    let all = texts(&text_items(&out2, None, 0, None).unwrap()).join(" ");
    assert!(all.contains("new chat") && !all.contains("conver"), "{all}");
}

#[test]
fn edits_are_incremental_and_compact_removes_history() {
    if !pdfium_available() || pdf2word_core::metrics::installed_families().is_empty() {
        return;
    }
    let input = make_pdf("inc.pdf", LINES, 0);
    let out = tmp("inc-out.pdf");
    let insert = TextEdit::Insert { page: 0, x: 0.2, y: 0.3, text: "Subset me".into(), size: 12.0, color: [0, 0, 0], style: NewTextStyle::default() };
    apply_edits(&input, None, &[insert], &out, None).unwrap();
    let (a, b) = (std::fs::read(&input).unwrap(), std::fs::read(&out).unwrap());
    assert!(b.len() > a.len() && b.starts_with(&a), "not an incremental update");
    // Only the used glyphs of the installed font are embedded.
    let doc = Document::load(&out).unwrap();
    let biggest = doc
        .objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .filter(|s| s.dict.has(b"Length1") || s.dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()) == Some(b"CIDFontType0C"))
        .map(|s| s.decompressed_content().map(|c| c.len()).unwrap_or(s.content.len()))
        .max()
        .unwrap_or(0);
    assert!(biggest > 0 && biggest < 60_000, "embedded font program of {biggest} bytes");
    let out2 = tmp("inc-compact.pdf");
    compact(&out, None, &out2, None).unwrap();
    let after = texts(&text_items(&out2, None, 0, None).unwrap());
    assert!(after.contains(&"Subset me".to_string()), "{after:?}");
    assert!(std::fs::metadata(&out2).unwrap().len() < b.len() as u64);
}

/// A page whose text is drawn as outlines of an installed font's glyphs.
fn outlined_pdf(name: &str) -> Option<PathBuf> {
    let font = ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf", "C:\\Windows\\Fonts\\arial.ttf", "/Library/Fonts/Arial.ttf"]
        .iter()
        .find_map(|p| std::fs::read(p).ok())?;
    let face = ttf_parser::Face::parse(&font, 0).ok()?;
    struct Pen<'a>(&'a mut String, f32, f32, f32);
    impl ttf_parser::OutlineBuilder for Pen<'_> {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.push_str(&format!("{:.2} {:.2} m ", self.2 + x * self.1, self.3 + y * self.1));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.push_str(&format!("{:.2} {:.2} l ", self.2 + x * self.1, self.3 + y * self.1));
        }
        fn quad_to(&mut self, _: f32, _: f32, x: f32, y: f32) {
            self.line_to(x, y);
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, x: f32, y: f32) {
            self.line_to(x, y);
        }
        fn close(&mut self) {
            self.0.push_str("h ");
        }
    }
    let mut c = String::from("0 0 0 rg\n");
    let s = 24.0 / face.units_per_em() as f32;
    let mut x = 72.0;
    for ch in "Outlined words".chars() {
        let g = face.glyph_index(ch)?;
        face.outline_glyph(g, &mut Pen(&mut c, s, x, 700.0));
        c.push_str("f\n");
        x += face.glyph_hor_advance(g)? as f32 * s;
    }
    c.push_str("BT /F1 12 Tf 72 600 Td (Real text) Tj ET");
    Some(make_pdf(name, &c, 0))
}

#[test]
fn outlined_text_is_recognised_and_replaced() {
    if !pdfium_available() {
        return;
    }
    if pdf2word_core::scan::available_languages(None).iter().all(|l| l != "eng") || pdf2word_core::metrics::installed_families().is_empty() {
        eprintln!("skipping: needs Tesseract English data and installed fonts");
        return;
    }
    let Some(input) = outlined_pdf("outlined.pdf") else { return };
    let pt = page_text(&input, None, 0, None).unwrap();
    assert!(pt.outlined_shapes >= 10, "{}", pt.outlined_shapes);
    assert_eq!(texts(&pt.items), vec!["Real text"]);
    let found = outlined_text(&input, None, 0, "auto", None).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let o = &found[0];
    assert_eq!(o.text, "Outlined words");
    assert!(o.outlined && (o.size - 24.0).abs() < 4.0, "{o:?}");
    let out = tmp("outlined-out.pdf");
    let edit = TextEdit::Outlined {
        page: 0,
        x: o.x,
        y: o.y,
        w: o.w,
        h: o.h,
        baseline: o.baseline,
        text: "Real words now".into(),
        size: o.size,
        color: o.color,
        style: NewTextStyle::default(),
        fit: true,
    };
    apply_edits(&input, None, &[edit], &out, None).unwrap();
    let after = page_text(&out, None, 0, None).unwrap();
    assert!(texts(&after.items).contains(&"Real words now".to_string()), "{:?}", texts(&after.items));
    assert_eq!(after.outlined_shapes, 0, "the outlines are removed");
}

/// A page using a fully embedded TrueType font (WinAnsi) that draws only "Hello".
fn embedded_font_pdf(name: &str) -> Option<PathBuf> {
    let data = ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"]
        .iter()
        .find_map(|p| std::fs::read(p).ok())?;
    let face = ttf_parser::Face::parse(&data, 0).ok()?;
    let upem = face.units_per_em() as f32;
    let widths: Vec<Object> = (32u8..=126)
        .map(|c| {
            let g = face.glyph_index(c as char);
            Object::Integer(g.and_then(|g| face.glyph_hor_advance(g)).map(|a| (a as f32 * 1000.0 / upem).round() as i64).unwrap_or(0))
        })
        .collect();
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let file_id = doc.add_object(Stream::new(dictionary! { "Length1" => data.len() as i64 }, data.clone()));
    let fd_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "Embedded", "Flags" => 32, "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 900.into()],
        "ItalicAngle" => 0, "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80, "FontFile2" => file_id,
    });
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "Embedded", "Encoding" => "WinAnsiEncoding",
        "FirstChar" => 32, "LastChar" => 126, "Widths" => widths, "FontDescriptor" => fd_id,
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, b"BT /F1 14 Tf 72 700 Td (Hello) Tj ET".to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 }));
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let path = tmp(name);
    doc.save(&path).ok()?;
    Some(path)
}

#[test]
fn letters_of_an_embedded_font_program_are_used() {
    if !pdfium_available() {
        return;
    }
    let Some(input) = embedded_font_pdf("embedded-font.pdf") else { return };
    let items = text_items(&input, None, 0, None).unwrap();
    let out = tmp("embedded-font-out.pdf");
    // None of these letters is drawn anywhere, but the font file has them.
    let r = apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: items[0].objects.clone(), text: "Jump quickly!".into(), fit: false }], &out, None).unwrap();
    assert!(r.notes.is_empty(), "{:?}", r.notes);
    assert_eq!(texts(&text_items(&out, None, 0, None).unwrap()), vec!["Jump quickly!"]);
    // No other font was added.
    let doc = Document::load(&out).unwrap();
    assert!(!doc.objects.values().any(|o| o.as_dict().is_ok_and(|d| d.has(b"P2WFont"))));
}

/// A page with `content` and an image /Im1 (`w`×`h` pixels, white) next to Helvetica /F1.
fn make_pdf_with_image(name: &str, content: &str, w: u32, h: u32) -> PathBuf {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
    let img_id = doc.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 },
        vec![255u8; (w * h * 3) as usize],
    ));
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id }, "XObject" => dictionary! { "Im1" => img_id } },
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 }));
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let path = tmp(name);
    doc.save(&path).unwrap();
    path
}

/// The pixel of the page's first picture at (fx, fy), fractions from its top-left.
fn picture_pixel(path: &Path, fx: f32, fy: f32) -> [u8; 3] {
    use pdfium_render::prelude::PdfPageObjectsCommon;
    let pdfium = pdf2word_core::load_pdfium(None).unwrap();
    let doc = pdfium.load_pdf_from_file(path, None).unwrap();
    let page = doc.pages().get(0).unwrap();
    for o in page.objects().iter() {
        if let Some(img) = o.as_image_object() {
            let raw = img.get_raw_image().unwrap().to_rgb8();
            let x = ((raw.width() as f32 * fx) as u32).min(raw.width() - 1);
            let y = ((raw.height() as f32 * fy) as u32).min(raw.height() - 1);
            return raw.get_pixel(x, y).0;
        }
    }
    panic!("no picture");
}

#[test]
fn redaction_removes_text_and_pixels_for_good() {
    if !pdfium_available() {
        return;
    }
    let content = format!("{LINES} q 100 0 0 100 300 500 cm /Im1 Do Q");
    let input = make_pdf_with_image("redact.pdf", &content, 20, 20);
    let items = text_items(&input, None, 0, None).unwrap();
    let hello = item(&items, "Hello world");
    // A box over "world" and the left half of the picture.
    let (wx, wy) = (hello.x + hello.w * 0.55, hello.y - 0.002);
    let edits = [
        TextEdit::Redact { page: 0, x: wx, y: wy, w: hello.x + hello.w - wx + 0.002, h: hello.h + 0.004, fill: None },
        TextEdit::Redact { page: 0, x: 300.0 / 612.0, y: (792.0 - 600.0) / 792.0, w: 50.0 / 612.0, h: 100.0 / 792.0, fill: Some([0, 0, 0]) },
    ];
    let out = tmp("redact-out.pdf");
    apply_edits(&input, None, &edits, &out, None).unwrap();
    let after = texts(&text_items(&out, None, 0, None).unwrap());
    assert!(after.iter().any(|t| t.starts_with("Hello")) && !after.iter().any(|t| t.contains("world")), "{after:?}");
    assert!(after.contains(&"Second line".to_string()) && after.contains(&", again".to_string()), "{after:?}");
    // The following text keeps its place: the removed glyphs left their space.
    let again = |p: &Path| item(&text_items(p, None, 0, None).unwrap(), ", again").x;
    assert!((again(&out) - again(&input)).abs() < 0.001);
    assert_eq!(picture_pixel(&out, 0.25, 0.5), [0, 0, 0]);
    assert_eq!(picture_pixel(&out, 0.75, 0.5), [255, 255, 255]);
    // Compacted, the removed text is not in the file at all.
    let out2 = tmp("redact-compact.pdf");
    compact(&out, None, &out2, None).unwrap();
    let doc = Document::load(&out2).unwrap();
    let all: Vec<u8> = doc.objects.values().filter_map(|o| o.as_stream().ok()).flat_map(|s| s.decompressed_content().unwrap_or_default()).collect();
    let hay = String::from_utf8_lossy(&all);
    assert!(!hay.contains("world") && !hay.contains("776F726C64"), "redacted text still in the file");
}

#[test]
fn search_matches_are_redacted() {
    if !pdfium_available() {
        return;
    }
    let content = "BT /F1 10 Tf 72 700 Td (Account number 1234-5678 belongs to) Tj 0 -14 Td (the customer named John Smith.) Tj ET";
    let input = make_pdf("redact-find.pdf", content, 0);
    let found = find_text(&input, None, "1234-5678", FindOptions::default(), None).unwrap();
    assert_eq!(found.len(), 1);
    let out = tmp("redact-find-out.pdf");
    pdf2word_core::edit::redact_matches(&input, None, &found, None, &out, None).unwrap();
    let all = texts(&text_items(&out, None, 0, None).unwrap()).join(" ");
    assert!(!all.contains("1234") && all.contains("Account number") && all.contains("belongs to"), "{all}");
}

#[test]
fn pictures_are_moved_replaced_deleted_and_added() {
    if !pdfium_available() {
        return;
    }
    let input = make_pdf_with_image("pics.pdf", "q 100 0 0 50 300 500 cm /Im1 Do Q", 20, 10);
    let pt = page_text(&input, None, 0, None).unwrap();
    assert_eq!(pt.images.len(), 1);
    let im = &pt.images[0];
    assert!((im.x - 300.0 / 612.0).abs() < 0.001 && (im.w - 100.0 / 612.0).abs() < 0.001, "{im:?}");

    // Move and enlarge.
    let out = tmp("pics-place.pdf");
    let change = ImageChange::Place { x: 0.1, y: 0.1, w: 0.4, h: 0.1 };
    apply_edits(&input, None, &[TextEdit::Image { page: 0, path: im.path.clone(), change }], &out, None).unwrap();
    let moved = &page_text(&out, None, 0, None).unwrap().images[0];
    assert!((moved.x - 0.1).abs() < 0.002 && (moved.w - 0.4).abs() < 0.002 && (moved.h - 0.1).abs() < 0.002, "{moved:?}");

    // Replace with a red PNG (square: fitted into the 2:1 box).
    let png = tmp("red.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0])).save(&png).unwrap();
    let out2 = tmp("pics-replace.pdf");
    apply_edits(&input, None, &[TextEdit::Image { page: 0, path: im.path.clone(), change: ImageChange::Replace { file: png.clone() } }], &out2, None).unwrap();
    let r = &page_text(&out2, None, 0, None).unwrap().images[0];
    assert!((r.h - im.h).abs() < 0.002 && r.w < im.w * 0.6, "{r:?}");
    assert_eq!(picture_pixel(&out2, 0.5, 0.5), [255, 0, 0]);

    // Delete, and add one.
    let out3 = tmp("pics-delete.pdf");
    let edits = [
        TextEdit::Image { page: 0, path: im.path.clone(), change: ImageChange::Delete },
        TextEdit::InsertImage { page: 0, x: 0.5, y: 0.5, w: 0.2, h: 0.2, file: png },
    ];
    apply_edits(&input, None, &edits, &out3, None).unwrap();
    let imgs = page_text(&out3, None, 0, None).unwrap().images;
    assert_eq!(imgs.len(), 1);
    assert!((imgs[0].x - 0.5).abs() < 0.002 && (imgs[0].w - 0.2).abs() < 0.002, "{:?}", imgs[0]);
}
