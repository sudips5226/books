//! Continuous layout: the pages become one flowing document.
//!
//! Sections are only started where the page size changes or the number of
//! text columns changes; multi-column pages become real Word columns, and
//! paragraphs and tables that the PDF broke across pages or columns are joined
//! again (a repeated table header row becomes a Word header row).

use crate::model::*;
use crate::structure::shift_blocks;

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    pub width: f32,
    /// Space after the column.
    pub space: f32,
}

/// Something anchored at a position relative to a block of a section.
#[derive(Clone, Debug)]
pub enum AnchorItem {
    Picture(Picture),
    TextBox(TextBox),
}

#[derive(Clone, Debug)]
pub struct Anchored {
    /// Index of the block the item is anchored to (written just before it).
    pub before: usize,
    /// Vertical offset from the top of that block; None = page-relative.
    pub dy: Option<f32>,
    pub item: AnchorItem,
}

#[derive(Clone, Debug, Default)]
pub struct FlowSection {
    pub width: f32,
    pub height: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    /// Text columns; empty = a single column.
    pub columns: Vec<ColumnDef>,
    /// Starts on a new page (first section or a page size change).
    pub new_page: bool,
    pub blocks: Vec<Block>,
    pub anchors: Vec<Anchored>,
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

fn block_top(b: &Block) -> f32 {
    match b {
        Block::Paragraph(p) => p.bbox.y0,
        Block::Table(t) => t.bbox.y0,
        Block::Picture(p) => p.bbox.y0,
        Block::Rule(r) => r.bbox.y0,
    }
}

fn block_bottom(b: &Block) -> f32 {
    match b {
        Block::Paragraph(p) => p.bbox.y1,
        Block::Table(t) => t.bbox.y1,
        Block::Picture(p) => p.bbox.y1,
        Block::Rule(r) => r.bbox.y1,
    }
}

fn lines_of(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p.n_lines,
            Block::Table(t) => t.rows.iter().flat_map(|r| &r.cells).map(|c| lines_of(&c.blocks)).sum(),
            _ => 0,
        })
        .sum()
}

/// A layout table that is really running text in columns.
fn text_columns(t: &Table, text_width: f32) -> bool {
    if !t.layout_only || t.bidi || t.rows.len() != 1 {
        return false;
    }
    let cells = &t.rows[0].cells;
    let total: f32 = t.col_widths.iter().sum();
    if !(2..=4).contains(&cells.len()) || cells.len() != t.col_widths.len() || total + t.indent < 0.6 * text_width {
        return false;
    }
    if t.col_widths.iter().any(|w| *w < 0.18 * total) {
        return false;
    }
    let lines: Vec<usize> = cells.iter().map(|c| lines_of(&c.blocks)).collect();
    lines.iter().all(|&n| n >= 2) && lines.iter().any(|&n| n >= 6)
}

fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end();
    let mut rev = t.chars().rev();
    match rev.next() {
        None => true,
        Some(c) if ".!?:;。！？".contains(c) => true,
        Some(c) if "\"”’)]»".contains(c) => rev.next().is_some_and(|d| ".!?".contains(d)),
        _ => false,
    }
}

/// The second paragraph continues the first one (split by a page or column break).
fn continues(a: &Paragraph, b: &Paragraph) -> bool {
    if a.heading.is_some() || b.heading.is_some() || b.list.is_some() || a.rtl != b.rtl || a.style != b.style {
        return false;
    }
    if a.list.is_some() && (b.indent_left - a.indent_left).abs() > 3.0 {
        return false;
    }
    if (a.font_size - b.font_size).abs() > 0.6 || a.inlines.iter().any(|i| matches!(i, Inline::Math(_))) && a.n_lines == 1 {
        return false;
    }
    let (ta, tb) = (a.plain_text(), b.plain_text());
    let (ta, tb) = (ta.trim_end(), tb.trim_start());
    let Some(first) = tb.chars().next() else { return false };
    if ta.is_empty() || ends_sentence(ta) {
        return false;
    }
    let hyphen = ta.ends_with('-') && ta.chars().rev().nth(1).is_some_and(|c| c.is_alphabetic());
    first.is_lowercase() || hyphen || (a.n_lines >= 2 && b.indent_first.abs() < 1.0)
}

fn merge_paragraphs(a: &mut Paragraph, b: Paragraph) {
    let text = a.plain_text();
    let ta = text.trim_end();
    let next_lower = b.plain_text().trim_start().chars().next().is_some_and(|c| c.is_lowercase());
    let joiner = if ta.ends_with('-') && next_lower && ta.chars().rev().nth(1).is_some_and(|c| c.is_alphabetic()) {
        // Hyphenation at the break: drop the hyphen.
        if let Some(Inline::Text { text, .. }) = a.inlines.iter_mut().rev().find(|i| matches!(i, Inline::Text { .. })) {
            let t = text.trim_end().trim_end_matches('-').to_string();
            *text = t;
        }
        Joiner::Hyphen
    } else if ta.ends_with(['-', '/', '–', '—']) {
        Joiner::Direct
    } else {
        Joiner::Space
    };
    // Trailing spaces would double up with the joiner.
    while let Some(Inline::Text { text, .. }) = a.inlines.last_mut() {
        let t = text.trim_end().to_string();
        if t.is_empty() {
            a.inlines.pop();
        } else {
            *text = t;
            break;
        }
    }
    a.inlines.push(Inline::SoftBreak(joiner));
    a.inlines.extend(b.inlines);
    a.n_lines += b.n_lines;
    a.space_after = b.space_after;
    if a.align == Align::Left && b.align == Align::Justify {
        a.align = Align::Justify;
    }
}

fn row_text(r: &Row) -> String {
    let mut s = String::new();
    for c in &r.cells {
        for b in &c.blocks {
            if let Block::Paragraph(p) = b {
                s.push_str(p.plain_text().trim());
            }
        }
        s.push('|');
    }
    s
}

fn tables_continue(a: &Table, b: &Table) -> bool {
    !a.layout_only
        && !b.layout_only
        && a.bidi == b.bidi
        && a.col_widths.len() == b.col_widths.len()
        && a.col_widths.iter().zip(&b.col_widths).all(|(x, y)| (x - y).abs() <= 4.0)
        && (a.indent - b.indent).abs() <= 6.0
}

fn merge_tables(a: &mut Table, mut b: Table) {
    if let (Some(ha), Some(hb)) = (a.rows.first(), b.rows.first()) {
        if b.rows.len() > 1 && row_text(ha) == row_text(hb) {
            b.rows.remove(0);
            a.header_rows = a.header_rows.max(1);
        }
    }
    a.rows.extend(b.rows);
}

/// The last block written (in the current section, or the previous one when
/// the current section has just started).
fn last_block(sections: &mut [FlowSection]) -> Option<(usize, usize, &mut Block)> {
    let n = sections.len();
    let si = if sections.last().is_some_and(|s| s.blocks.is_empty()) && n >= 2 { n - 2 } else { n.checked_sub(1)? };
    let len = sections[si].blocks.len();
    sections[si].blocks.last_mut().map(|b| (si, len - 1, b))
}

/// Appends blocks to the last section, joining what was split by a break.
/// Returns the (section, block) position of each appended block (merged ones
/// map to the block they were merged into).
fn append(sections: &mut [FlowSection], blocks: Vec<Block>, at_break: bool, gap: f32) -> Vec<(usize, usize)> {
    let mut idx = Vec::with_capacity(blocks.len());
    let cur = sections.len() - 1;
    for (k, mut b) in blocks.into_iter().enumerate() {
        let brk = k == 0 && at_break;
        // On the same page only an unmistakable continuation is joined (a
        // sentence running from a full-width region into a column, or around a figure).
        if !brk {
            if let (Some((si, bi, Block::Paragraph(a))), Block::Paragraph(p)) = (last_block(sections), &b) {
                if continues(a, p) && p.plain_text().trim_start().chars().next().is_some_and(char::is_lowercase) {
                    let Block::Paragraph(p) = b else { unreachable!() };
                    merge_paragraphs(a, p);
                    idx.push((si, bi));
                    continue;
                }
            }
        }
        if brk {
            match (last_block(sections), b) {
                (Some((si, bi, Block::Paragraph(a))), Block::Paragraph(p)) if continues(a, &p) => {
                    merge_paragraphs(a, p);
                    idx.push((si, bi));
                    continue;
                }
                (Some((si, bi, Block::Table(a))), Block::Table(t)) if tables_continue(a, &t) => {
                    merge_tables(a, t);
                    idx.push((si, bi));
                    continue;
                }
                (_, other) => b = other,
            }
            // A new paragraph after a break gets ordinary paragraph spacing.
            if !sections[cur].blocks.is_empty() {
                match &mut b {
                    Block::Paragraph(p) if p.space_before < 0.5 => p.space_before = gap,
                    Block::Table(t) if t.space_before < 0.5 => t.space_before = gap,
                    Block::Picture(p) if p.space_before < 0.5 => p.space_before = gap,
                    _ => {}
                }
            }
        }
        sections[cur].blocks.push(b);
        idx.push((cur, sections[cur].blocks.len() - 1));
    }
    idx
}

/// Horizontal extent of blocks on the page.
fn extent(blocks: &[Block]) -> Option<(f32, f32)> {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => (p.bbox.x0, p.bbox.x1),
            Block::Table(t) => (t.bbox.x0, t.bbox.x1),
            Block::Picture(p) => (p.bbox.x0, p.bbox.x1),
            Block::Rule(r) => (r.bbox.x0, r.bbox.x1),
        })
        .filter(|(a, b)| b > a)
        .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
}

/// Builds continuous sections from analysed pages.
pub fn build(pages: Vec<PageLayout>) -> Vec<FlowSection> {
    // Typical paragraph gap (for paragraphs that started a page).
    let gaps: Vec<f32> = pages
        .iter()
        .flat_map(|p| p.blocks.iter().skip(1))
        .filter_map(|b| match b {
            Block::Paragraph(p) if p.space_before > 0.5 => Some(p.space_before),
            _ => None,
        })
        .collect();
    let gap = median(gaps).min(18.0);

    let mut sections: Vec<FlowSection> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        // A run of pages with the same size shares margins.
        let (w, h) = (pages[i].width, pages[i].height);
        let mut j = i;
        while j < pages.len() && (pages[j].width - w).abs() < 2.0 && (pages[j].height - h).abs() < 2.0 {
            j += 1;
        }
        let run = &pages[i..j];
        let ml = median(run.iter().map(|p| p.margin_left).collect());
        let mr = median(run.iter().map(|p| p.margin_right).collect());
        let mt = median(run.iter().map(|p| p.margin_top).collect());
        let mb = median(run.iter().map(|p| p.margin_bottom).collect()).max(18.0);
        let text_width = (w - ml - mr).max(36.0);
        let new_section = |columns: Vec<ColumnDef>, new_page: bool| FlowSection {
            width: w,
            height: h,
            margin_left: ml,
            margin_right: mr,
            margin_top: mt,
            margin_bottom: mb,
            columns,
            new_page,
            ..Default::default()
        };
        sections.push(new_section(Vec::new(), true));
        let mut first_page = true;
        for page in &pages[i..j] {
            let mut page = page.clone();
            shift_blocks(&mut page.blocks, page.margin_left - ml, page.margin_right - mr);
            // The first page keeps its own top offset.
            if first_page {
                let extra = page.margin_top - mt;
                if extra > 1.0 {
                    if let Some(Block::Paragraph(p)) = page.blocks.first_mut() {
                        p.space_before += extra;
                    }
                }
            }
            // Floating items: anchored at the first block at or below them.
            let mut floats: Vec<(usize, f32, bool, AnchorItem)> = Vec::new(); // (page block, y0, page-relative, item)
            let target = |y0: f32, blocks: &[Block]| blocks.iter().position(|b| block_bottom(b) >= y0).unwrap_or(blocks.len().saturating_sub(1));
            for f in std::mem::take(&mut page.floating) {
                let full = f.bbox.area() > 0.8 * page.width * page.height;
                floats.push((target(f.bbox.y0, &page.blocks), f.bbox.y0, full, AnchorItem::Picture(f)));
            }
            for tb in std::mem::take(&mut page.text_boxes) {
                let y0 = tb.cy - tb.height.max(tb.width) / 2.0;
                floats.push((target(y0, &page.blocks), y0, false, AnchorItem::TextBox(tb)));
            }
            let page_tops: Vec<f32> = page.blocks.iter().map(block_top).collect();
            let mut placed: Vec<Option<(usize, usize)>> = vec![None; page.blocks.len()]; // (section, block)
            let mut at_break = true;
            for (bi, b) in page.blocks.into_iter().enumerate() {
                match b {
                    Block::Table(t) if text_columns(&t, text_width) => {
                        // Word columns at the text's own column edges; the gutters become column spacing.
                        let n = t.col_widths.len();
                        let cells: Vec<Vec<Block>> = t.rows.into_iter().next().unwrap().cells.into_iter().map(|c| c.blocks).collect();
                        let mut bounds = vec![ml + t.indent];
                        for w in &t.col_widths {
                            bounds.push(bounds.last().unwrap() + w);
                        }
                        let ext: Vec<(f32, f32)> = cells.iter().enumerate().map(|(k, c)| extent(c).unwrap_or((bounds[k], bounds[k + 1]))).collect();
                        // Column k spans starts[k]..ends[k] (relative to the left margin).
                        let mut starts: Vec<f32> = ext.iter().map(|e| e.0 - ml).collect();
                        starts[0] = 0.0;
                        let mut ends: Vec<f32> = ext.iter().map(|e| e.1 - ml).collect();
                        for k in 0..n - 1 {
                            ends[k] = ends[k].min(starts[k + 1] - 6.0).max(starts[k] + 36.0);
                        }
                        ends[n - 1] = text_width.max(starts[n - 1] + 36.0);
                        let columns: Vec<ColumnDef> = (0..n)
                            .map(|k| ColumnDef { width: ends[k] - starts[k], space: if k + 1 < n { (starts[k + 1] - ends[k]).max(0.0) } else { 0.0 } })
                            .collect();
                        let cur = sections.last().unwrap();
                        let same = cur.columns.len() == n && cur.columns.iter().zip(&columns).all(|(a, b)| (a.width - b.width).abs() <= 0.1 * b.width);
                        if !same {
                            sections.push(new_section(columns, false));
                        }
                        for (k, mut blocks) in cells.into_iter().enumerate() {
                            // Indents were relative to the cell; now to the column.
                            let (old_l, old_r) = (bounds[k], bounds[k + 1]);
                            let (new_l, new_r) = (ml + starts[k], ml + ends[k]);
                            shift_blocks(&mut blocks, old_l - new_l, new_r - old_r);
                            let idx = append(&mut sections, blocks, at_break || k > 0, gap);
                            if placed[bi].is_none() {
                                placed[bi] = idx.first().copied();
                            }
                            at_break = false;
                        }
                        // Column bottoms are a break for the next content.
                        at_break = true;
                    }
                    other => {
                        if !sections.last().unwrap().columns.is_empty() {
                            sections.push(new_section(Vec::new(), false));
                        }
                        let idx = append(&mut sections, vec![other], at_break, gap);
                        placed[bi] = idx.first().copied();
                        at_break = false;
                    }
                }
            }
            for (bi, y0, full, item) in floats {
                let Some((si, block)) = placed.get(bi).copied().flatten().or_else(|| {
                    let si = sections.len() - 1;
                    Some((si, sections[si].blocks.len()))
                }) else {
                    continue;
                };
                let dy = if full { None } else { Some(y0 - page_tops.get(bi).copied().unwrap_or(y0)) };
                sections[si].anchors.push(Anchored { before: block, dy, item });
            }
            first_page = false;
        }
        i = j;
    }
    sections.retain(|s| !s.blocks.is_empty() || !s.anchors.is_empty() || s.new_page);
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str, lines: usize) -> Paragraph {
        Paragraph {
            inlines: vec![Inline::Text { text: text.into(), style: TextStyle { size: 10.0, ..Default::default() }, link: None, pdf_width: 0.0, pdf_tracking: 0.0 }],
            n_lines: lines,
            font_size: 10.0,
            ..Default::default()
        }
    }

    #[test]
    fn joins_split_paragraphs() {
        let mut a = para("the quick brown fox jumps over the lazy", 3);
        let b = para("dog and runs away.", 1);
        assert!(continues(&a, &b));
        merge_paragraphs(&mut a, b);
        assert_eq!(a.plain_text(), "the quick brown fox jumps over the lazy dog and runs away.");
        assert_eq!(a.n_lines, 4);
        // Hyphenated word split by the page break.
        let mut a = para("an exam-", 2);
        let b = para("ple of hyphenation.", 1);
        assert!(continues(&a, &b));
        merge_paragraphs(&mut a, b);
        assert_eq!(a.plain_text().replace(' ', ""), "anexampleofhyphenation.");
        // Finished sentences and headings are not joined.
        assert!(!continues(&para("The end.", 3), &para("Next paragraph", 2)));
        let mut h = para("Title", 1);
        h.heading = Some(1);
        assert!(!continues(&h, &para("text", 1)));
    }

    #[test]
    fn repeated_header_row_is_dropped() {
        let row = |a: &str, b: &str| Row { cells: vec![Cell { blocks: vec![Block::Paragraph(para(a, 1))], col_span: 1, ..Default::default() }, Cell { blocks: vec![Block::Paragraph(para(b, 1))], col_span: 1, ..Default::default() }], height: 12.0 };
        let mut t1 = Table { col_widths: vec![100.0, 50.0], rows: vec![row("Item", "Qty"), row("A", "1")], ..Default::default() };
        let t2 = Table { col_widths: vec![100.0, 51.0], rows: vec![row("Item", "Qty"), row("B", "2")], ..Default::default() };
        assert!(tables_continue(&t1, &t2));
        merge_tables(&mut t1, t2);
        assert_eq!(t1.rows.len(), 3);
        assert_eq!(t1.header_rows, 1);
    }
}
