//! Page operations on generated sample PDFs. Skipped without PDFium.

use std::path::{Path, PathBuf};

use pdf2word_core::pages::{apply_page_ops, extract, merge, split, PageOp};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("pdf2word-pages-{}-{name}", std::process::id()))
}

fn pdfium_available() -> bool {
    pdf2word_core::load_pdfium(None).is_ok()
}

fn count(p: &Path) -> usize {
    pdf2word_core::inspect(p, None, None).unwrap().pages
}

/// The first text run of a page.
fn first_text(p: &Path, page: usize) -> String {
    pdf2word_core::edit::text_items(p, None, page, None).unwrap().first().map(|i| i.text.clone()).unwrap_or_default()
}

fn rotation(p: &Path, page: usize) -> i32 {
    let pdfium = pdf2word_core::load_pdfium(None).unwrap();
    let doc = pdfium.load_pdf_from_file(p, None).unwrap();
    let r = doc.pages().get(page as i32).unwrap().rotation().unwrap();
    use pdfium_render::prelude::PdfPageRenderRotation as R;
    match r {
        R::None => 0,
        R::Degrees90 => 90,
        R::Degrees180 => 180,
        R::Degrees270 => 270,
    }
}

#[test]
fn rotate_delete_reorder_and_insert() {
    if !pdfium_available() {
        return;
    }
    let input = fixture("report.pdf");
    assert_eq!(count(&input), 2);
    let (p1, p2) = (first_text(&input, 0), first_text(&input, 1));

    let out = tmp("rot.pdf");
    apply_page_ops(&input, None, &[PageOp::Rotate { pages: vec![1], degrees: 90 }, PageOp::Rotate { pages: vec![1], degrees: 180 }], &out).unwrap();
    assert_eq!((rotation(&out, 0), rotation(&out, 1)), (0, 270));
    // An incremental update of the original.
    assert!(std::fs::read(&out).unwrap().starts_with(&std::fs::read(&input).unwrap()));

    let out = tmp("order.pdf");
    apply_page_ops(&input, None, &[PageOp::Reorder { order: vec![1, 0] }], &out).unwrap();
    assert_eq!((first_text(&out, 0), first_text(&out, 1)), (p2.clone(), p1.clone()));

    let out = tmp("move.pdf");
    apply_page_ops(&input, None, &[PageOp::InsertBlank { at: 1 }, PageOp::Move { from: 2, to: 0 }], &out).unwrap();
    assert_eq!(count(&out), 3);
    assert_eq!(first_text(&out, 0), p2);
    assert_eq!(first_text(&out, 1), p1);
    assert_eq!(first_text(&out, 2), "");

    let out = tmp("del.pdf");
    apply_page_ops(&input, None, &[PageOp::Delete { pages: vec![0] }], &out).unwrap();
    assert_eq!(count(&out), 1);
    assert_eq!(first_text(&out, 0), p2);
    assert!(apply_page_ops(&input, None, &[PageOp::Delete { pages: vec![0, 1] }], &tmp("none.pdf")).is_err());

    let out = tmp("ins.pdf");
    let other = fixture("twocol.pdf");
    apply_page_ops(&input, None, &[PageOp::InsertPdf { at: 1, file: other.clone(), password: None, pages: None }], &out).unwrap();
    assert_eq!(count(&out), 2 + count(&other));
    assert_eq!(first_text(&out, 1), first_text(&other, 0));
    assert_eq!(first_text(&out, 1 + count(&other)), p2);
}

#[test]
fn merge_extract_and_split() {
    if !pdfium_available() {
        return;
    }
    let (a, b) = (fixture("report.pdf"), fixture("tables.pdf"));
    let out = tmp("merged.pdf");
    merge(&[(a.clone(), None), (b.clone(), None)], &out).unwrap();
    assert_eq!(count(&out), count(&a) + count(&b));
    assert_eq!(first_text(&out, 0), first_text(&a, 0));
    assert_eq!(first_text(&out, count(&a)), first_text(&b, 0));

    let out = tmp("extract.pdf");
    extract(&a, None, &[1], &out).unwrap();
    assert_eq!(count(&out), 1);
    assert_eq!(first_text(&out, 0), first_text(&a, 1));

    let dir = tmp("split");
    std::fs::create_dir_all(&dir).unwrap();
    let parts = split(&a, None, 1, &dir).unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(first_text(&parts[1], 0), first_text(&a, 1));
}
