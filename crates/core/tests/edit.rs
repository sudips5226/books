//! Editing text in PDFs: changed runs, untouched content, fonts.
//! Skipped when the PDFium library cannot be found (set `PDFIUM_DYNAMIC_LIB_PATH`).

use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, Object, Stream};
use pdf2word_core::edit::{apply_edits, find_replace, text_items, NewTextStyle, TextEdit, TextItem};

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
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
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
    let (n, _) = find_replace(&fixture("report.pdf"), None, "Quarterly", "Annual", &out, None).unwrap();
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
fn right_to_left_text_is_refused() {
    if !pdfium_available() {
        return;
    }
    let input = fixture("rtl-office.pdf");
    let items = text_items(&input, None, 0, None).unwrap();
    let hebrew = items.iter().find(|i| i.text.chars().any(|c| ('\u{5d0}'..='\u{5ea}').contains(&c))).unwrap();
    let err = apply_edits(&input, None, &[TextEdit::Replace { page: 0, objects: hebrew.objects.clone(), text: "שלום".into(), fit: false }], &tmp("rtl.pdf"), None)
        .unwrap_err();
    assert!(format!("{err:#}").contains("right-to-left"), "{err:#}");
}
