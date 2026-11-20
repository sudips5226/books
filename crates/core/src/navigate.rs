//! Navigation and review structure applied after layout: outline headings
//! and bookmarks, internal links, the table of contents, comments and form
//! controls.

use std::collections::HashMap;

use crate::annots::{parse_dest_link, OutlineEntry};
use crate::model::*;
use crate::structure::for_each_paragraph;

/// Letters and digits only, lower case (for matching titles).
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

fn page_of(pages: &[PageLayout], index: usize) -> Option<usize> {
    pages.iter().position(|p| p.index == index)
}

/// Paragraph boxes of a page in reading order (including text columns and table cells).
fn paragraph_boxes(page: &mut PageLayout) -> Vec<(Rect, String, Option<u8>)> {
    let mut v = Vec::new();
    for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| v.push((p.bbox, p.plain_text(), p.heading)));
    v
}

/// Calls `f` on the paragraph with the given reading-order index.
fn with_paragraph(page: &mut PageLayout, index: usize, f: &mut dyn FnMut(&mut Paragraph)) {
    let mut k = 0;
    for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| {
        if k == index {
            f(p);
        }
        k += 1;
    });
}

/// Every paragraph of the document, including headers, footers, footnotes and text boxes.
pub fn for_all_paragraphs(pages: &mut [PageLayout], st: &mut DocStructure, f: &mut dyn FnMut(&mut Paragraph)) {
    for page in pages.iter_mut() {
        for_each_paragraph(&mut page.blocks, f);
        for tb in &mut page.text_boxes {
            let mut p = Paragraph { inlines: std::mem::take(&mut tb.inlines), ..Default::default() };
            f(&mut p);
            tb.inlines = p.inlines;
        }
    }
    for hf in [st.header.as_mut(), st.footer.as_mut(), st.header_even.as_mut(), st.footer_even.as_mut()].into_iter().flatten() {
        for_each_paragraph(&mut hf.blocks, f);
    }
    for n in &mut st.footnotes {
        for_each_paragraph(&mut n.blocks, f);
    }
}

/// Uses the PDF outline for the heading levels (and the navigation pane):
/// each entry's paragraph becomes a heading with a bookmark. When most
/// entries are found, headings guessed from font sizes but absent from the
/// outline are dropped. Returns the number of entries found.
pub fn apply_outline(pages: &mut [PageLayout], outline: &[OutlineEntry]) -> usize {
    let mut found = 0;
    let mut considered = 0;
    let mut marked: Vec<(usize, usize)> = Vec::new();
    for (k, e) in outline.iter().enumerate() {
        let Some(pi) = page_of(pages, e.page) else { continue };
        considered += 1;
        let t = norm(&e.title);
        if t.is_empty() {
            continue;
        }
        let boxes = paragraph_boxes(&mut pages[pi]);
        let best = boxes
            .iter()
            .enumerate()
            .filter_map(|(i, (bbox, text, _))| {
                let p = norm(text);
                let quality = if p == t {
                    3
                } else if p.starts_with(&t) && t.len() * 2 >= p.len() || t.starts_with(&p) && p.len() * 3 >= t.len() * 2 && p.len() >= 3 {
                    2
                } else if p.len() < 3 * t.len() && p.contains(&t) && t.len() >= 4 {
                    1
                } else {
                    0
                };
                let dist = e.y.map(|y| (bbox.y0 - y).abs()).unwrap_or(0.0);
                (quality > 0).then_some((i, quality, dist))
            })
            .max_by(|a, b| a.1.cmp(&b.1).then(b.2.total_cmp(&a.2)));
        let Some((i, _, _)) = best else { continue };
        if marked.contains(&(pi, i)) {
            continue;
        }
        marked.push((pi, i));
        found += 1;
        let level = (e.level + 1).min(6) as u8;
        with_paragraph(&mut pages[pi], i, &mut |p| {
            p.heading = Some(level);
            p.bookmarks.push(format!("_Toc{k:05}"));
        });
    }
    // A reliable outline replaces the font-size guesses.
    if considered >= 2 && found * 5 >= considered * 3 {
        for (pi, page) in pages.iter_mut().enumerate() {
            let mut k = 0;
            for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| {
                if p.heading.is_some() && !marked.contains(&(pi, k)) {
                    p.heading = None;
                }
                k += 1;
            });
        }
    }
    found
}

/// Turns `#dest:` links into links to bookmarks placed on their target
/// paragraphs; targets outside the converted pages lose the link.
pub fn resolve_internal_links(pages: &mut [PageLayout], st: &mut DocStructure) {
    let mut targets: Vec<String> = Vec::new();
    for_all_paragraphs(pages, st, &mut |p: &mut Paragraph| {
        for i in &p.inlines {
            if let Inline::Text { link: Some(l), .. } = i {
                if l.starts_with("#dest:") && !targets.contains(l) {
                    targets.push(l.clone());
                }
            }
        }
    });
    if targets.is_empty() {
        return;
    }
    let mut names: HashMap<String, Option<String>> = HashMap::new();
    let mut counter = 0;
    for t in targets {
        let Some((page, x, y)) = parse_dest_link(&t) else {
            names.insert(t, None);
            continue;
        };
        let Some(pi) = page_of(pages, page) else {
            names.insert(t, None);
            continue;
        };
        let boxes = paragraph_boxes(&mut pages[pi]);
        // The first paragraph at or below the target point (in its column, if known).
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, (b, text, _))| !text.trim().is_empty() && y.is_none_or(|y| b.y1 >= y - 2.0) && x.is_none_or(|x| b.x1 >= x - 2.0))
            .min_by(|a, b| a.1 .0.y0.total_cmp(&b.1 .0.y0))
            .or_else(|| boxes.iter().enumerate().find(|(_, (_, text, _))| !text.trim().is_empty()))
            .map(|(i, _)| i);
        let Some(i) = pick else {
            names.insert(t, None);
            continue;
        };
        let mut name = String::new();
        with_paragraph(&mut pages[pi], i, &mut |p| {
            if let Some(b) = p.bookmarks.first() {
                name = b.clone();
            } else {
                counter += 1;
                name = format!("_Ref{counter:05}");
                p.bookmarks.push(name.clone());
            }
        });
        names.insert(t, Some(name));
    }
    for_all_paragraphs(pages, st, &mut |p: &mut Paragraph| {
        for i in p.inlines.iter_mut() {
            if let Inline::Text { link, .. } = i {
                if let Some(l) = link.as_ref().filter(|l| l.starts_with("#dest:")) {
                    *link = names.get(l).cloned().flatten().map(|n| format!("#{n}"));
                }
            }
        }
    });
}

/// Anchors review comments at the paragraph they point to.
pub fn attach_comments(pages: &mut [PageLayout], comments: Vec<(usize, RawComment)>, st: &mut DocStructure) {
    for (page, c) in comments {
        let Some(pi) = page_of(pages, page) else { continue };
        let boxes = paragraph_boxes(&mut pages[pi]);
        let target = boxes
            .iter()
            .enumerate()
            .filter(|(_, (_, text, _))| !text.trim().is_empty())
            .map(|(i, (b, _, _))| {
                let overlap = b.intersection(&c.bbox.expand(2.0)).area().max(0.0);
                let dy = if c.bbox.cy() < b.y0 { b.y0 - c.bbox.cy() } else if c.bbox.cy() > b.y1 { c.bbox.cy() - b.y1 } else { 0.0 };
                let dx = if c.bbox.cx() < b.x0 { b.x0 - c.bbox.cx() } else if c.bbox.cx() > b.x1 { c.bbox.cx() - b.x1 } else { 0.0 };
                (i, overlap, dy * 3.0 + dx)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)))
            .map(|x| x.0);
        let id = st.comments.len();
        st.comments.push(c);
        match target {
            Some(i) => with_paragraph(&mut pages[pi], i, &mut |p| p.comments.push(id)),
            None => {
                // An empty page: the comment gets a paragraph of its own.
                pages[pi].blocks.push(Block::Paragraph(Paragraph { comments: vec![id], ..Default::default() }));
            }
        }
    }
}

/// Replaces the placeholder characters of form fields by content controls.
pub fn insert_controls(pages: &mut [PageLayout], fields: &HashMap<usize, Vec<FormControl>>) {
    for page in pages.iter_mut() {
        let Some(list) = fields.get(&page.index) else { continue };
        for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| {
            let is_control = |c: char| (c as u32) >= CONTROL_BASE && ((c as u32 - CONTROL_BASE) as usize) < list.len();
            if !p.inlines.iter().any(|i| matches!(i, Inline::Text { text, .. } if text.chars().any(is_control))) {
                return;
            }
            let mut out = Vec::with_capacity(p.inlines.len() + 2);
            for inl in p.inlines.drain(..) {
                let Inline::Text { text, style, link, pdf_width, pdf_tracking } = inl else {
                    out.push(inl);
                    continue;
                };
                if !text.chars().any(is_control) {
                    out.push(Inline::Text { text, style, link, pdf_width, pdf_tracking });
                    continue;
                }
                let mut buf = String::new();
                for c in text.chars() {
                    if is_control(c) {
                        if !buf.trim().is_empty() || (!buf.is_empty() && !out.is_empty()) {
                            out.push(Inline::Text { text: std::mem::take(&mut buf), style: style.clone(), link: link.clone(), pdf_width: 0.0, pdf_tracking: 0.0 });
                        }
                        buf.clear();
                        let control = list[(c as u32 - CONTROL_BASE) as usize].clone();
                        out.push(Inline::Control { control, style: style.clone() });
                    } else {
                        buf.push(c);
                    }
                }
                if !buf.is_empty() {
                    out.push(Inline::Text { text: buf, style, link, pdf_width: 0.0, pdf_tracking: 0.0 });
                }
            }
            p.inlines = out;
        });
    }
}

/// A table-of-contents line: title, leader dots (or a tab) and a page number.
/// Returns (title, number, dotted leader).
fn toc_entry(text: &str) -> Option<(String, String, bool)> {
    let t = text.trim_end();
    let num_start = t.rfind(|c: char| c.is_whitespace() || c == '.' || c == '\t' || c == '…').map(|i| i + t[i..].chars().next().unwrap().len_utf8())?;
    let number = &t[num_start..];
    let is_number = (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) && number.len() <= 4)
        || (!number.is_empty() && number.len() <= 6 && number.chars().all(|c| "ivxlcIVXLC".contains(c)));
    if !is_number {
        return None;
    }
    let before = &t[..num_start];
    // Leader: a tab, or at least three dots (possibly spaced).
    let lead_start = before.trim_end_matches(|c: char| c == '.' || c == '·' || c == '…' || c == '_' || c.is_whitespace());
    let leader = &before[lead_start.len()..];
    let dots = leader.chars().filter(|c| matches!(c, '.' | '·' | '_')).count() + 3 * leader.chars().filter(|&c| c == '…').count();
    if dots < 3 && !leader.contains('\t') {
        return None;
    }
    let title = lead_start.trim().to_string();
    if !title.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    Some((title, number.to_string(), dots >= 3))
}

/// Finds tables of contents (three or more consecutive entries) and rebuilds
/// them: right tab with dot leader, links to the headings, TOC paragraph
/// styles and, outside exact layout, a TOC field Word can update.
/// Returns the TOC paragraph styles to add.
pub fn detect_toc(pages: &mut [PageLayout], field: bool) -> Vec<StyleDef> {
    // Headings by normalised text, with their bookmark (created on demand).
    let mut headings: HashMap<String, (usize, usize)> = HashMap::new();
    for (pi, page) in pages.iter_mut().enumerate() {
        for (i, (_, text, h)) in paragraph_boxes(page).into_iter().enumerate() {
            if h.is_some() {
                headings.entry(norm(&text)).or_insert((pi, i));
            }
        }
    }
    // Candidate entries in reading order: (page, paragraph index, title, number).
    let mut cands: Vec<(usize, usize, String, String, bool)> = Vec::new();
    for (pi, page) in pages.iter_mut().enumerate() {
        for (i, (_, text, h)) in paragraph_boxes(page).into_iter().enumerate() {
            if h.is_some() || text.contains('\n') {
                continue;
            }
            if let Some((title, number, dots)) = toc_entry(&text) {
                cands.push((pi, i, title, number, dots));
            }
        }
    }
    // Runs of entries (consecutive paragraphs on the same or the next page).
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for (k, c) in cands.iter().enumerate() {
        let joins = runs.last().and_then(|r| r.last()).is_some_and(|&prev| {
            let p = &cands[prev];
            (p.0 == c.0 && c.1 <= p.1 + 2) || (c.0 == p.0 + 1 && c.1 <= 2)
        });
        if joins {
            runs.last_mut().unwrap().push(k);
        } else {
            runs.push(vec![k]);
        }
    }
    runs.retain(|r| r.len() >= 3);
    if runs.is_empty() {
        return Vec::new();
    }
    let mut styles: Vec<StyleDef> = Vec::new();
    let mut bookmark_counter = 0;
    for run in runs {
        // Levels from the distinct left indents.
        let mut indents: Vec<f32> = Vec::new();
        for &k in &run {
            let (pi, i, ..) = &cands[k];
            with_paragraph(&mut pages[*pi], *i, &mut |p| indents.push(p.indent_left));
        }
        let mut levels: Vec<f32> = indents.clone();
        levels.sort_by(f32::total_cmp);
        levels.dedup_by(|a, b| (*a - *b).abs() < 4.0);
        let n = run.len();
        for (pos, &k) in run.iter().enumerate() {
            let (pi, i, title, number, dots) = cands[k].clone();
            let level = levels.iter().position(|l| (indents[pos] - l).abs() < 4.0).unwrap_or(0).min(8) + 1;
            // Link to the heading when the PDF had no link.
            let mut target: Option<String> = None;
            if let Some(&(hp, hi)) = headings.get(&norm(&title)).or_else(|| headings.iter().find(|(h, _)| h.ends_with(&norm(&title)) && norm(&title).len() >= 4).map(|(_, v)| v)) {
                with_paragraph(&mut pages[hp], hi, &mut |h| {
                    if h.bookmarks.is_empty() {
                        bookmark_counter += 1;
                        h.bookmarks.push(format!("_TocH{bookmark_counter:05}"));
                    }
                    target = h.bookmarks.first().cloned();
                });
            }
            let style_id = format!("TOC{level}");
            let mut entry_style: Option<TextStyle> = None;
            with_paragraph(&mut pages[pi], i, &mut |p| {
                let texts: Vec<(&TextStyle, &Option<String>)> = p.inlines.iter().filter_map(|x| match x {
                    Inline::Text { style, link, text, .. } if !text.trim().is_empty() => Some((style, link)),
                    _ => None,
                }).collect();
                let Some(&(first_style, _)) = texts.first() else { return };
                let first_style = first_style.clone();
                let last_style = texts.last().map(|t| t.0.clone()).unwrap_or_else(|| first_style.clone());
                let link = texts.iter().find_map(|t| t.1.clone()).or_else(|| target.as_ref().map(|t| format!("#{t}")));
                let container_x0 = p.bbox.x0 - p.indent_left - p.indent_first.min(0.0);
                let right = p.bbox.x1 - container_x0;
                let mut inlines = Vec::new();
                if field && pos == 0 {
                    inlines.push(Inline::FieldBegin(r#" TOC \o "1-3" \h \z \u "#.into()));
                }
                inlines.push(Inline::Text { text: title.clone(), style: first_style.clone(), link: link.clone(), pdf_width: 0.0, pdf_tracking: 0.0 });
                inlines.push(Inline::Tab);
                inlines.push(Inline::Text { text: number.clone(), style: last_style, link, pdf_width: 0.0, pdf_tracking: 0.0 });
                if field && pos + 1 == n {
                    inlines.push(Inline::FieldEnd);
                }
                p.inlines = inlines;
                p.tabs = vec![TabStop { pos: right, kind: TabKind::Right, leader: dots }];
                p.align = Align::Left;
                p.indent_first = 0.0;
                p.indent_right = 0.0;
                p.style = Some(style_id.clone());
                entry_style = Some(first_style);
            });
            if let Some(st) = entry_style {
                if !styles.iter().any(|s| s.id == style_id) {
                    styles.push(StyleDef { id: style_id.clone(), name: format!("toc {level}"), font: st.font.clone(), size: st.size, bold: st.bold, italic: st.italic, color: st.color, outline: None });
                }
            }
        }
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_lines() {
        assert_eq!(toc_entry("1 Introduction . . . . . . . . 2"), Some(("1 Introduction".into(), "2".into(), true)));
        assert_eq!(toc_entry("2.1 Data collection ........ 13"), Some(("2.1 Data collection".into(), "13".into(), true)));
        assert_eq!(toc_entry("Preface\tiv"), Some(("Preface".into(), "iv".into(), false)));
        assert_eq!(toc_entry("The year was 2024"), None);
        assert_eq!(toc_entry("Total ... 12"), Some(("Total".into(), "12".into(), true)));
        assert_eq!(toc_entry("1.2.3"), None);
    }
}
