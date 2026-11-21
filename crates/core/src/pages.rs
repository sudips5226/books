//! Page operations: rotate, delete, reorder and insert pages, merge, extract
//! and split PDFs.
//!
//! The page tree is flattened (every page a child of the root node, with the
//! attributes it inherited copied onto it), so pages can be moved freely.
//! Changes to a file are written as an incremental update, like text edits.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use lopdf::{Dictionary, Object, ObjectId, Stream};

/// A change to the pages of a PDF. Page numbers are 0-based and refer to the
/// pages as they are when the operation runs (after the operations before it).
#[derive(Clone, Debug, PartialEq)]
pub enum PageOp {
    /// Turns pages clockwise by a multiple of 90 degrees.
    Rotate { pages: Vec<usize>, degrees: i32 },
    Delete { pages: Vec<usize> },
    /// Puts the pages in this order; pages not listed are left out.
    Reorder { order: Vec<usize> },
    /// Moves page `from` so that it becomes page `to`.
    Move { from: usize, to: usize },
    /// Inserts a blank page before page `at` (the size of a neighbouring page).
    InsertBlank { at: usize },
    /// Inserts pages of another PDF before page `at` (all of them when `pages` is `None`).
    InsertPdf { at: usize, file: PathBuf, password: Option<String>, pages: Option<Vec<usize>> },
}

const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

fn load(path: &Path, password: Option<&str>) -> anyhow::Result<lopdf::Document> {
    let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    crate::edit::load_lopdf(&bytes, password)
}

/// Makes every page a child of the root page tree node, carrying the
/// attributes it inherited. Returns the root and the pages in order.
fn flatten(doc: &mut lopdf::Document) -> anyhow::Result<(ObjectId, Vec<ObjectId>)> {
    let root = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    for &p in &pages {
        let mut inherited: Vec<(Vec<u8>, Object)> = Vec::new();
        let mut parent = doc.get_dictionary(p)?.get(b"Parent").and_then(|o| o.as_reference()).ok();
        let mut guard = 0;
        while let Some(id) = parent {
            guard += 1;
            if guard > 64 {
                break;
            }
            let d = doc.get_dictionary(id)?;
            for k in INHERITED {
                if let Ok(v) = d.get(k) {
                    inherited.push((k.to_vec(), v.clone()));
                }
            }
            parent = d.get(b"Parent").and_then(|o| o.as_reference()).ok();
        }
        let d = doc.get_dictionary_mut(p)?;
        for (k, v) in inherited {
            if !d.has(&k) {
                d.set(k, v);
            }
        }
        d.set("Parent", root);
    }
    set_kids(doc, root, &pages)?;
    Ok((root, pages))
}

fn set_kids(doc: &mut lopdf::Document, root: ObjectId, pages: &[ObjectId]) -> anyhow::Result<()> {
    let d = doc.get_dictionary_mut(root)?;
    d.set("Kids", Object::Array(pages.iter().map(|p| Object::Reference(*p)).collect()));
    d.set("Count", pages.len() as i64);
    Ok(())
}

fn check(pages: &[usize], n: usize) -> anyhow::Result<()> {
    if let Some(p) = pages.iter().find(|p| **p >= n) {
        bail!("page {} does not exist (the document has {n})", p + 1);
    }
    Ok(())
}

/// Objects reachable from `start` (not following /Parent links).
fn reachable(doc: &lopdf::Document, start: &[ObjectId]) -> BTreeSet<ObjectId> {
    fn refs(o: &Object, out: &mut Vec<ObjectId>) {
        match o {
            Object::Reference(id) => out.push(*id),
            Object::Array(a) => a.iter().for_each(|x| refs(x, out)),
            Object::Dictionary(d) => {
                for (k, v) in d.iter() {
                    if k != b"Parent" {
                        refs(v, out);
                    }
                }
            }
            Object::Stream(s) => {
                for (k, v) in s.dict.iter() {
                    if k != b"Parent" {
                        refs(v, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut keep = BTreeSet::new();
    let mut stack: Vec<ObjectId> = start.to_vec();
    while let Some(id) = stack.pop() {
        if !keep.insert(id) {
            continue;
        }
        if let Ok(o) = doc.get_object(id) {
            refs(o, &mut stack);
        }
    }
    keep
}

/// Copies pages of `other` into `doc` (objects renumbered); returns the new page ids.
fn import_pages(doc: &mut lopdf::Document, root: ObjectId, mut other: lopdf::Document, pages: Option<&[usize]>) -> anyhow::Result<Vec<ObjectId>> {
    flatten(&mut other)?;
    other.renumber_objects_with(doc.max_id + 1);
    let all: Vec<ObjectId> = other.get_pages().into_values().collect();
    let chosen: Vec<ObjectId> = match pages {
        Some(p) => {
            check(p, all.len())?;
            p.iter().map(|i| all[*i]).collect()
        }
        None => all,
    };
    for id in reachable(&other, &chosen) {
        if let Some(o) = other.objects.remove(&id) {
            doc.objects.insert(id, o);
        }
    }
    doc.max_id = doc.max_id.max(other.max_id);
    for &p in &chosen {
        doc.get_dictionary_mut(p)?.set("Parent", root);
    }
    Ok(chosen)
}

fn blank_page(doc: &mut lopdf::Document, root: ObjectId, like: Option<ObjectId>) -> ObjectId {
    let media = like
        .and_then(|p| doc.get_dictionary(p).ok())
        .and_then(|d| d.get(b"MediaBox").ok().cloned())
        .unwrap_or_else(|| Object::Array(vec![0.into(), 0.into(), 595.into(), 842.into()]));
    let content = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"Page".to_vec()));
    d.set("Parent", root);
    d.set("MediaBox", media);
    d.set("Resources", Dictionary::new());
    d.set("Contents", content);
    doc.add_object(d)
}

fn apply_ops(doc: &mut lopdf::Document, ops: &[PageOp]) -> anyhow::Result<()> {
    let (root, mut pages) = flatten(doc)?;
    for op in ops {
        match op {
            PageOp::Rotate { pages: which, degrees } => {
                check(which, pages.len())?;
                if degrees % 90 != 0 {
                    bail!("pages turn by multiples of 90 degrees");
                }
                for &i in which {
                    let d = doc.get_dictionary_mut(pages[i])?;
                    let cur = d.get(b"Rotate").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0);
                    d.set("Rotate", (cur + *degrees as i64).rem_euclid(360));
                }
            }
            PageOp::Delete { pages: which } => {
                check(which, pages.len())?;
                if which.iter().collect::<BTreeSet<_>>().len() >= pages.len() {
                    bail!("a PDF needs at least one page");
                }
                let gone: BTreeSet<usize> = which.iter().copied().collect();
                pages = pages.iter().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, p)| *p).collect();
            }
            PageOp::Reorder { order } => {
                check(order, pages.len())?;
                if order.is_empty() {
                    bail!("a PDF needs at least one page");
                }
                pages = order.iter().map(|i| pages[*i]).collect();
            }
            PageOp::Move { from, to } => {
                check(&[*from, *to], pages.len())?;
                let p = pages.remove(*from);
                pages.insert(*to, p);
            }
            PageOp::InsertBlank { at } => {
                let at = (*at).min(pages.len());
                let like = pages.get(at.saturating_sub(1)).or(pages.first()).copied();
                let p = blank_page(doc, root, like);
                pages.insert(at, p);
            }
            PageOp::InsertPdf { at, file, password, pages: which } => {
                let other = load(file, password.as_deref())?;
                let new = import_pages(doc, root, other, which.as_deref())?;
                let at = (*at).min(pages.len());
                pages.splice(at..at, new);
            }
        }
    }
    set_kids(doc, root, &pages)
}

/// Applies page operations to `input` and writes `output` (a different file).
pub fn apply_page_ops(input: &Path, password: Option<&str>, ops: &[PageOp], output: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("could not read {}", input.display()))?;
    let prev = crate::edit::load_lopdf(&bytes, password)?;
    let mut doc = prev.clone();
    apply_ops(&mut doc, ops)?;
    crate::edit::save_incremental(bytes, prev, doc, output)
}

/// Joins PDFs (with their passwords) into one.
pub fn merge(inputs: &[(PathBuf, Option<String>)], output: &Path) -> anyhow::Result<()> {
    let ((first, pw), rest) = inputs.split_first().ok_or_else(|| anyhow!("no PDFs to merge"))?;
    let mut doc = load(first, pw.as_deref())?;
    let (root, mut pages) = flatten(&mut doc)?;
    for (path, pw) in rest {
        let other = load(path, pw.as_deref())?;
        pages.extend(import_pages(&mut doc, root, other, None)?);
    }
    set_kids(&mut doc, root, &pages)?;
    doc.prune_objects();
    doc.save(output).with_context(|| format!("could not write {}", output.display()))?;
    Ok(())
}

/// Writes the given pages (in this order) to a new PDF.
pub fn extract(input: &Path, password: Option<&str>, pages: &[usize], output: &Path) -> anyhow::Result<()> {
    let mut doc = load(input, password)?;
    apply_ops(&mut doc, &[PageOp::Reorder { order: pages.to_vec() }])?;
    doc.prune_objects();
    doc.save(output).with_context(|| format!("could not write {}", output.display()))?;
    Ok(())
}

/// Splits a PDF into files of `every` pages, named `<stem>-<first>-<last>.pdf` in `dir`.
pub fn split(input: &Path, password: Option<&str>, every: usize, dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if every == 0 {
        bail!("pages per file must be at least 1");
    }
    let n = load(input, password)?.get_pages().len();
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let mut out = Vec::new();
    for start in (0..n).step_by(every) {
        let end = (start + every).min(n);
        let path = dir.join(if end - start == 1 { format!("{stem}-{}.pdf", start + 1) } else { format!("{stem}-{}-{}.pdf", start + 1, end) });
        extract(input, password, &(start..end).collect::<Vec<_>>(), &path)?;
        out.push(path);
    }
    Ok(out)
}
