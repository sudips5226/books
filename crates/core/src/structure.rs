//! Document-level structure: headers and footers, Word lists, footnotes,
//! captions and paragraph styles.

use std::collections::HashMap;

use crate::model::*;

// ---------------------------------------------------------------------------
// Headers and footers (on raw pages, before layout)
// ---------------------------------------------------------------------------

/// A horizontal run of glyphs (part of a text line).
struct LinePart {
    glyphs: Vec<usize>,
    bbox: Rect,
    text: String,
}

fn line_parts(raw: &RawPage) -> Vec<LinePart> {
    let mut order: Vec<usize> = (0..raw.glyphs.len()).collect();
    order.sort_by(|&a, &b| {
        let (ga, gb) = (&raw.glyphs[a], &raw.glyphs[b]);
        (ga.baseline.round() as i64, ga.bbox.x0 as i64).cmp(&(gb.baseline.round() as i64, gb.bbox.x0 as i64))
    });
    let mut parts: Vec<LinePart> = Vec::new();
    for i in order {
        let g = &raw.glyphs[i];
        let size = raw.styles[g.style].size;
        if let Some(p) = parts.last_mut() {
            let last = &raw.glyphs[*p.glyphs.last().unwrap()];
            let gap = g.bbox.x0 - last.bbox.x1;
            if (g.baseline - last.baseline).abs() < 0.35 * size && gap < 2.5 * size && gap > -size {
                if g.space_before || gap > 0.2 * size {
                    p.text.push(' ');
                }
                p.text.push(g.ch);
                p.glyphs.push(i);
                p.bbox = p.bbox.union(&g.bbox);
                continue;
            }
        }
        parts.push(LinePart { glyphs: vec![i], bbox: g.bbox, text: g.ch.to_string() });
    }
    parts
}

fn normalise(text: &str) -> String {
    let mut s = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !s.ends_with('#') {
                s.push('#');
            }
        } else if c.is_whitespace() {
            if !s.ends_with(' ') {
                s.push(' ');
            }
        } else {
            s.extend(c.to_lowercase());
        }
    }
    s.trim().to_string()
}

/// Page numbers in any of the usual shapes ("3", "- 3 -", "Page 3 of 10", "3/10").
fn is_page_number(norm: &str) -> bool {
    let t = norm.trim_matches(|c: char| c == '-' || c == '–' || c == '—' || c == ' ' || c == '[' || c == ']');
    let t = t.strip_prefix("page").or_else(|| t.strip_prefix("seite")).or_else(|| t.strip_prefix("p.")).or_else(|| t.strip_prefix("pg.")).unwrap_or(t).trim();
    matches!(t, "#" | "# of #" | "# / #" | "#/#" | "# von #" | "# de #" | "# sur #")
        || t.chars().all(|c| "ivxlc".contains(c)) && !t.is_empty() && t.len() <= 6
}

/// Headers/footers cut out of the pages, ready to be laid out.
pub struct ExtractedHeaderFooter {
    /// Header material and the (0-based) PDF page index it was taken from.
    pub header: Option<(RawPage, usize)>,
    pub footer: Option<(RawPage, usize)>,
    /// Different material on even pages (two-sided layouts): `header`/`footer` are then the odd pages'.
    pub header_even: Option<(RawPage, usize)>,
    pub footer_even: Option<(RawPage, usize)>,
    pub blank_first_header: bool,
    pub blank_first_footer: bool,
    /// PDF page indices of pages without the header / footer (chapter openings …).
    pub pages_without_header: Vec<usize>,
    pub pages_without_footer: Vec<usize>,
    /// Printed page number minus physical page number (front matter, chapters).
    pub number_offset: i64,
    /// Printed page number (value, roman numerals) by PDF page index, where found.
    pub page_numbers: HashMap<usize, (i64, bool)>,
}

/// Finds text, rules and images repeated at the top or bottom of most pages,
/// removes them from the pages and returns them as separate raw pages
/// (taken from a representative page, whose index is returned).
pub fn extract_headers_footers(raws: &mut [RawPage]) -> ExtractedHeaderFooter {
    let none = ExtractedHeaderFooter { header: None, footer: None, header_even: None, footer_even: None, blank_first_header: false, blank_first_footer: false, pages_without_header: Vec::new(), pages_without_footer: Vec::new(), number_offset: 0, page_numbers: HashMap::new() };
    let n = raws.len();
    if n < 2 {
        return none;
    }
    let need = if n == 2 { 2 } else { n.div_ceil(2) };
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum Band {
        Top,
        Bottom,
    }
    let band_of = |r: &Rect, h: f32| -> Option<Band> {
        if r.y1 <= h * 0.12 {
            Some(Band::Top)
        } else if r.y0 >= h * 0.88 {
            Some(Band::Bottom)
        } else {
            None
        }
    };
    // Candidate keys per page.
    type Key = (Band, String, i32, i32);
    let mut counts: HashMap<Key, usize> = HashMap::new();
    let mut pn_counts: HashMap<Band, usize> = HashMap::new();
    let mut per_page: Vec<Vec<(Key, LinePart, bool)>> = Vec::new();
    for raw in raws.iter() {
        let mut items = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut pn_seen = std::collections::HashSet::new();
        let parts: Vec<LinePart> = line_parts(raw).into_iter().filter(|p| band_of(&p.bbox, raw.height).is_some()).collect();
        // Only the page-number-like text closest to the page edge is the page number
        // (other lone digits near the top are superscripts, labels …).
        let edge_pn = |band: Band| -> Option<usize> {
            let cands = parts.iter().enumerate().filter(|(_, p)| band_of(&p.bbox, raw.height) == Some(band) && is_page_number(&normalise(&p.text)));
            match band {
                Band::Top => cands.min_by(|a, b| a.1.bbox.y0.total_cmp(&b.1.bbox.y0)).map(|c| c.0),
                Band::Bottom => cands.max_by(|a, b| a.1.bbox.y1.total_cmp(&b.1.bbox.y1)).map(|c| c.0),
            }
        };
        let pn_parts = [edge_pn(Band::Top), edge_pn(Band::Bottom)];
        for (pi, part) in parts.into_iter().enumerate() {
            let Some(band) = band_of(&part.bbox, raw.height) else { continue };
            let norm = normalise(&part.text);
            let pn = pn_parts.contains(&Some(pi));
            // Distance from the page edge the band belongs to.
            let edge = match band {
                Band::Top => part.bbox.y0,
                Band::Bottom => raw.height - part.bbox.y1,
            };
            let key = (band, norm, (part.bbox.x0 / 8.0).round() as i32, (edge / 6.0).round() as i32);
            if seen.insert(key.clone()) {
                *counts.entry(key.clone()).or_default() += 1;
            }
            if pn && pn_seen.insert(band) {
                *pn_counts.entry(band).or_default() += 1;
            }
            items.push((key, part, pn));
        }
        per_page.push(items);
    }
    let repeated = |key: &Key, pn: bool| counts.get(key).copied().unwrap_or(0) >= need || (pn && pn_counts.get(&key.0).copied().unwrap_or(0) >= need);

    // Rules and images repeated in the bands.
    let rule_key = |r: &Rule, h: f32| band_of(&r.bbox, h).map(|b| (b, (r.bbox.cy() / 3.0).round() as i32, (r.bbox.x0 / 6.0).round() as i32, (r.bbox.width() / 6.0).round() as i32));
    let mut rule_counts: HashMap<(Band, i32, i32, i32), usize> = HashMap::new();
    let img_key = |im: &RawImage, h: f32| band_of(&im.bbox, h).map(|b| (b, (im.bbox.x0 / 4.0).round() as i32, (im.bbox.y0 / 4.0).round() as i32, (im.bbox.width() / 4.0).round() as i32));
    let mut img_counts: HashMap<(Band, i32, i32, i32), usize> = HashMap::new();
    for raw in raws.iter() {
        let mut seen = std::collections::HashSet::new();
        for r in &raw.rules {
            if let Some(k) = rule_key(r, raw.height) {
                if seen.insert(k) {
                    *rule_counts.entry(k).or_default() += 1;
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for im in &raw.images {
            if let Some(k) = img_key(im, raw.height) {
                if seen.insert(k) {
                    *img_counts.entry(k).or_default() += 1;
                }
            }
        }
    }

    let mut has_band: HashMap<Band, Vec<bool>> = HashMap::new();
    let mut scores: Vec<(Band, usize, usize)> = Vec::new(); // (band, page, items)
    let mut extracted: HashMap<(Band, usize), RawPage> = HashMap::new();
    let mut offsets: HashMap<i64, usize> = HashMap::new();
    // Printed page number of each page (value, roman), by PDF page index.
    let mut page_numbers: HashMap<usize, (i64, bool)> = HashMap::new();
    // Horizontal position (left/centre/right third) of each page's page number, per band.
    let mut pn_pos: HashMap<Band, Vec<(usize, u8)>> = HashMap::new();
    for (pi, items) in per_page.into_iter().enumerate() {
        let raw = &mut raws[pi];
        let mut remove = vec![false; raw.glyphs.len()];
        let mut found: HashMap<Band, usize> = HashMap::new();
        // A line mixing repeated material with text that varies (a running title)
        // stays in the body as a whole: a static header could not reproduce it.
        let same_line = |a: &Rect, b: &Rect| a.y0.max(b.y0) < a.y1.min(b.y1) - 0.3 * a.height().min(b.height());
        let mixed: Vec<Band> = [Band::Top, Band::Bottom]
            .into_iter()
            .filter(|&band| {
                items.iter().any(|(k, part, pn)| {
                    k.0 == band && !repeated(k, *pn) && items.iter().any(|(k2, p2, pn2)| k2.0 == band && repeated(k2, *pn2) && same_line(&part.bbox, &p2.bbox))
                })
            })
            .collect();
        for (key, part, pn) in &items {
            if repeated(key, *pn) && !mixed.contains(&key.0) {
                if *pn {
                    let third = ((part.bbox.cx() / raw.width.max(1.0)) * 3.0).clamp(0.0, 2.0) as u8;
                    pn_pos.entry(key.0).or_default().push((pi, third));
                    let digits: String = part.text.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(v) = digits.parse::<i64>() {
                        *offsets.entry(v - raw.index as i64 - 1).or_default() += 1;
                        page_numbers.entry(raw.index).or_insert((v, false));
                    } else if let Some(v) = part.text.split_whitespace().rev().find_map(roman_value) {
                        page_numbers.entry(raw.index).or_insert((v as i64, true));
                    }
                }
                for &g in &part.glyphs {
                    remove[g] = true;
                }
                *found.entry(key.0).or_default() += 1;
            }
        }
        for band in [Band::Top, Band::Bottom] {
            has_band.entry(band).or_default().push(found.contains_key(&band));
        }
        if found.is_empty() {
            continue;
        }
        // Copy the removed material into per-band raw pages.
        for band in [Band::Top, Band::Bottom] {
            let Some(&cnt) = found.get(&band) else { continue };
            let mut hf = RawPage { index: raw.index, width: raw.width, height: raw.height, styles: raw.styles.clone(), links: raw.links.clone(), ..Default::default() };
            for (i, g) in raw.glyphs.iter().enumerate() {
                if remove[i] && band_of(&g.bbox, raw.height) == Some(band) {
                    hf.glyphs.push(g.clone());
                }
            }
            for r in &raw.rules {
                if rule_key(r, raw.height).is_some_and(|k| k.0 == band && rule_counts[&k] >= need) {
                    hf.rules.push(*r);
                }
            }
            for im in &raw.images {
                if img_key(im, raw.height).is_some_and(|k| k.0 == band && img_counts[&k] >= need) {
                    hf.images.push(im.clone());
                }
            }
            // Pages with an arabic page number make the best template (it becomes a PAGE field).
            let arabic = items.iter().any(|(k, part, pn)| *pn && k.0 == band && part.text.chars().any(|c| c.is_ascii_digit()));
            let score = 2 * (cnt + hf.rules.len() + hf.images.len()) + arabic as usize;
            scores.push((band, pi, score));
            extracted.insert((band, pi), hf);
        }
        let mut k = 0;
        raw.glyphs.retain(|_| {
            let keep = !remove[k];
            k += 1;
            keep
        });
        let h = raw.height;
        raw.rules.retain(|r| !rule_key(r, h).is_some_and(|k| rule_counts[&k] >= need && found.contains_key(&k.0)));
        raw.images.retain(|im| !img_key(im, h).is_some_and(|k| img_counts[&k] >= need && found.contains_key(&k.0)));
    }
    let number_offset = offsets.into_iter().max_by_key(|(o, n)| (*n, -o.abs())).map(|(o, _)| o).unwrap_or(0);
    let parity = |pi: usize| (raws[pi].index as i64 + 1 + number_offset).rem_euclid(2) as usize; // 1 = odd
    // Two-sided layout: page numbers on one side for odd pages, another for even ones.
    let alternates = |band: Band| -> bool {
        let Some(v) = pn_pos.get(&band) else { return false };
        let side = |p: usize| -> Option<u8> {
            let s: Vec<u8> = v.iter().filter(|(pi, _)| parity(*pi) == p).map(|x| x.1).collect();
            (s.len() >= 2 && s.iter().filter(|&&t| t == s[0]).count() * 5 >= s.len() * 4).then(|| s[0])
        };
        matches!((side(0), side(1)), (Some(a), Some(b)) if a != b)
    };
    let best = |band: Band, want: Option<usize>| -> Option<usize> {
        scores.iter().filter(|(b, pi, _)| *b == band && want.is_none_or(|w| parity(*pi) == w)).max_by_key(|(_, pi, s)| (*s, std::cmp::Reverse(*pi))).map(|x| x.1)
    };
    let mut take = |band: Band, pi: Option<usize>| pi.and_then(|pi| extracted.remove(&(band, pi))).map(|r| {
        let index = r.index;
        (r, index)
    });
    let (mut header, mut footer, mut header_even, mut footer_even) = (None, None, None, None);
    for band in [Band::Top, Band::Bottom] {
        let (main, even) = if alternates(band) { (take(band, best(band, Some(1))), take(band, best(band, Some(0)))) } else { (take(band, best(band, None)), None) };
        match band {
            Band::Top => (header, header_even) = (main, even),
            Band::Bottom => (footer, footer_even) = (main, even),
        }
    }
    // A title page without header/footer while the others have them.
    let lacks_first = |band: Band| has_band.get(&band).is_some_and(|v| !v[0] && v[1..].iter().filter(|b| **b).count() >= need.min(v.len() - 1));
    let blank_first_header = header.is_some() && lacks_first(Band::Top);
    let blank_first_footer = footer.is_some() && lacks_first(Band::Bottom);
    let without = |band: Band| -> Vec<usize> {
        has_band.get(&band).map(|v| v.iter().enumerate().filter(|(_, b)| !**b).map(|(pi, _)| raws[pi].index).collect()).unwrap_or_default()
    };
    let pages_without_header = if header.is_some() { without(Band::Top) } else { Vec::new() };
    let pages_without_footer = if footer.is_some() { without(Band::Bottom) } else { Vec::new() };
    ExtractedHeaderFooter { header, footer, header_even, footer_even, pages_without_header, pages_without_footer, blank_first_header, blank_first_footer, number_offset, page_numbers }
}

/// Height the blocks take when written (paragraph lines, rows, pictures).
pub fn blocks_height(blocks: &[Block]) -> f32 {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p.space_before + p.space_after + p.n_lines.max(1) as f32 * p.line_pitch.unwrap_or(1.2 * p.font_size.max(1.0)),
            Block::Table(t) => {
                t.space_before
                    + t.rows
                        .iter()
                        .map(|r| r.height.max(r.cells.iter().map(|c| blocks_height(&c.blocks)).fold(0.0, f32::max)))
                        .sum::<f32>()
            }
            Block::Picture(p) => p.space_before + p.height,
            Block::Rule(r) => r.space_before + r.width.max(1.0),
        })
        .sum()
}

/// Header/footer tables with a single cell are unwrapped: their border-less
/// content is all that matters there, and tables in headers/footers upset
/// some consumers.
pub fn unwrap_single_cell_tables(blocks: &mut Vec<Block>) {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks.drain(..) {
        match b {
            Block::Table(t) if t.rows.len() == 1 && t.rows[0].cells.len() == 1 => {
                let mut inner = t.rows.into_iter().next().unwrap().cells.into_iter().next().unwrap().blocks;
                let indent = t.indent.max(0.0);
                shift_blocks(&mut inner, indent, 0.0);
                out.extend(inner);
            }
            other => out.push(other),
        }
    }
    *blocks = out;
}

/// Shifts header/footer content laid out with its own margins onto the body
/// margins (indents are relative to the margins).
pub fn shift_blocks(blocks: &mut [Block], dl: f32, dr: f32) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                p.indent_left = (p.indent_left + dl).max(0.0);
                p.indent_right = (p.indent_right + dr).max(0.0);
                for t in &mut p.tabs {
                    t.pos += dl;
                }
            }
            Block::Table(t) => t.indent += dl,
            Block::Picture(p) => p.indent = (p.indent + dl).max(0.0),
            Block::Rule(r) => {
                r.indent_left = (r.indent_left + dl).max(0.0);
                r.indent_right = (r.indent_right + dr).max(0.0);
            }
        }
    }
}

/// Replaces the page number of the source page (and the page count) by fields.
pub fn insert_page_fields(blocks: &mut [Block], page_number: usize, page_count: usize) {
    fn fix(inlines: &mut Vec<Inline>, page: &str, count: &str) {
        let mut out = Vec::new();
        for inl in inlines.drain(..) {
            let Inline::Text { text, style, link, pdf_width, pdf_tracking } = inl else {
                out.push(inl);
                continue;
            };
            // Split into digit runs and the rest.
            let mut pieces: Vec<(bool, String)> = Vec::new();
            for c in text.chars() {
                let d = c.is_ascii_digit();
                match pieces.last_mut() {
                    Some((pd, s)) if *pd == d => s.push(c),
                    _ => pieces.push((d, c.to_string())),
                }
            }
            let mut seen_page = false;
            let mut buf = String::new();
            let flush = |buf: &mut String, out: &mut Vec<Inline>| {
                if !buf.is_empty() {
                    out.push(Inline::Text { text: std::mem::take(buf), style: style.clone(), link: link.clone(), pdf_width: 0.0, pdf_tracking: 0.0 });
                }
            };
            let mut replaced = false;
            for (is_digit, piece) in pieces {
                if is_digit && !seen_page && piece == page {
                    flush(&mut buf, &mut out);
                    out.push(Inline::Field { kind: FieldKind::PageNumber, text: piece, style: style.clone() });
                    seen_page = true;
                    replaced = true;
                } else if is_digit && seen_page && piece == count {
                    flush(&mut buf, &mut out);
                    out.push(Inline::Field { kind: FieldKind::PageCount, text: piece, style: style.clone() });
                    replaced = true;
                } else {
                    buf.push_str(&piece);
                }
            }
            if replaced {
                flush(&mut buf, &mut out);
            } else {
                out.push(Inline::Text { text: buf, style, link, pdf_width, pdf_tracking });
            }
        }
        *inlines = out;
    }
    // A count of 0 means "unknown": only the page number becomes a field.
    let (page, count) = (page_number.to_string(), if page_count == 0 { "-".to_string() } else { page_count.to_string() });
    for b in blocks {
        match b {
            Block::Paragraph(p) => fix(&mut p.inlines, &page, &count),
            Block::Table(t) => {
                for r in &mut t.rows {
                    for c in &mut r.cells {
                        insert_page_fields(&mut c.blocks, page_number, page_count);
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Paragraph visitors
// ---------------------------------------------------------------------------

/// Calls `f` for every paragraph (including those in table cells), in order.
pub fn for_each_paragraph(blocks: &mut [Block], f: &mut dyn FnMut(&mut Paragraph)) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => f(p),
            Block::Table(t) => {
                for r in &mut t.rows {
                    for c in &mut r.cells {
                        for_each_paragraph(&mut c.blocks, f);
                    }
                }
            }
            _ => {}
        }
    }
}

fn first_text(p: &Paragraph) -> Option<&str> {
    p.inlines.iter().find_map(|i| match i {
        Inline::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

/// The format of the run with the most characters.
fn dominant_style(p: &Paragraph) -> Option<TextStyle> {
    let mut best: Option<(&TextStyle, usize)> = None;
    let mut totals: Vec<(&TextStyle, usize)> = Vec::new();
    for i in &p.inlines {
        if let Inline::Text { text, style, .. } = i {
            let n = text.chars().filter(|c| !c.is_whitespace()).count();
            match totals.iter_mut().find(|(s, _)| s.same_run_format(style)) {
                Some(e) => e.1 += n,
                None => totals.push((style, n)),
            }
        }
    }
    for (s, n) in totals {
        if best.is_none_or(|b| n > b.1) {
            best = Some((s, n));
        }
    }
    best.map(|(s, _)| s.clone())
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

fn roman_value(s: &str) -> Option<u32> {
    let mut total = 0i32;
    let mut prev = 0i32;
    for c in s.chars().rev() {
        let v = match c.to_ascii_lowercase() {
            'i' => 1,
            'v' => 5,
            'x' => 10,
            'l' => 50,
            'c' => 100,
            _ => return None,
        };
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    (total > 0).then_some(total as u32)
}

/// Parses a list marker into (format, level text, value).
pub fn parse_marker(m: &str) -> Option<(NumFormat, String, u32)> {
    let m = m.trim();
    let mut chars = m.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if matches!(c, '•' | '◦' | '▪' | '▫' | '■' | '□' | '●' | '○' | '◆' | '◇' | '►' | '▶' | '➢' | '✓' | '✔' | '–' | '—' | '-' | '*' | '·' | '\u{F0B7}' | '\u{F0A7}' | '\u{F0D8}') {
            let c = match c {
                '\u{F0B7}' => '•',
                '\u{F0A7}' => '▪',
                '\u{F0D8}' => '➢',
                c => c,
            };
            return Some((NumFormat::Bullet, c.to_string(), 1));
        }
    }
    let (open, rest) = match m.strip_prefix('(') {
        Some(r) => ("(", r),
        None => ("", m),
    };
    let close_pos = rest.find(['.', ')', ':']).unwrap_or(rest.len());
    let (body, close) = rest.split_at(close_pos);
    if body.is_empty() || close.len() > 1 || (open.is_empty() && close.is_empty()) {
        return None;
    }
    let (fmt, value) = if body.chars().all(|c| c.is_ascii_digit()) {
        (NumFormat::Decimal, body.parse().ok()?)
    } else if body.len() <= 4 && body.chars().all(|c| "ivxlc".contains(c)) && body != "c" && body != "l" && body != "v" && body != "x" || body == "i" {
        (NumFormat::LowerRoman, roman_value(body)?)
    } else if body.len() <= 4 && body.chars().all(|c| "IVXLC".contains(c)) && (body.len() > 1 || body == "I") {
        (NumFormat::UpperRoman, roman_value(body)?)
    } else if body.len() == 1 && body.chars().all(|c| c.is_ascii_lowercase()) {
        (NumFormat::LowerLetter, (body.as_bytes()[0] - b'a' + 1) as u32)
    } else if body.len() == 1 && body.chars().all(|c| c.is_ascii_uppercase()) {
        (NumFormat::UpperLetter, (body.as_bytes()[0] - b'A' + 1) as u32)
    } else {
        return None;
    };
    Some((fmt, format!("{open}%N{close}"), value))
}

/// Paragraphs in reading order, descending into text columns; other blocks
/// (data tables, pictures, rules) appear as `None`.
fn flow_paragraphs<'a>(blocks: &'a mut [Block], out: &mut Vec<Option<&'a mut Paragraph>>) {
    for b in blocks.iter_mut() {
        match b {
            Block::Paragraph(p) => out.push(Some(p)),
            Block::Table(t) if t.layout_only => {
                for r in t.rows.iter_mut() {
                    for c in r.cells.iter_mut() {
                        flow_paragraphs(&mut c.blocks, out);
                    }
                }
            }
            _ => out.push(None),
        }
    }
}

/// Turns typed list markers (marker + tab) into Word lists.
pub fn detect_lists(pages: &mut [PageLayout]) -> Vec<ListDef> {
    let mut lists: Vec<ListDef> = Vec::new();
    // Current open list: (list index, per-level (format, last value, indent)).
    struct Open {
        list: usize,
        levels: Vec<(NumFormat, u32, f32)>,
        last_indent: f32,
    }
    let mut open: Option<Open> = None;
    for page in pages.iter_mut() {
        let mut seq = Vec::new();
        flow_paragraphs(&mut page.blocks, &mut seq);
        for item in seq {
            let Some(p) = item else {
                // Tables and pictures end a list.
                open = None;
                continue;
            };
            let marker = match (p.inlines.first(), p.inlines.get(1)) {
                (Some(Inline::Text { text, style, .. }), Some(Inline::Tab)) => parse_marker(text).map(|m| (m, style.clone())),
                _ => None,
            };
            let Some(((fmt, pattern, value), marker_style)) = marker else {
                // Continuation paragraphs indented under an item keep the list open.
                if open.as_ref().is_some_and(|o| p.indent_left + 1.0 < o.last_indent - 2.0) || p.heading.is_some() {
                    open = None;
                }
                continue;
            };
            let indent = p.indent_left;
            // Continue the open list when the level structure fits.
            let mut level: Option<usize> = None;
            if let Some(o) = open.as_mut() {
                if let Some(l) = o.levels.iter().position(|(_, _, ind)| (ind - indent).abs() < 6.0) {
                    let (f, last, _) = o.levels[l];
                    if f == fmt && (fmt == NumFormat::Bullet || value == last + 1) {
                        level = Some(l);
                        o.levels.truncate(l + 1);
                    }
                } else if indent > o.levels.last().map(|l| l.2).unwrap_or(0.0) + 6.0 && o.levels.len() < 9 {
                    // A deeper level starts.
                    o.levels.push((fmt, value.saturating_sub(1), indent));
                    let def = &mut lists[o.list];
                    if def.levels.len() < o.levels.len() {
                        def.levels.push(ListLevel { format: fmt, text: String::new(), start: value, marker_style: marker_style.clone(), indent_left: indent, hanging: -p.indent_first.min(0.0) });
                    }
                    level = Some(o.levels.len() - 1);
                }
            }
            let level = match level {
                Some(l) => l,
                None => {
                    lists.push(ListDef { levels: vec![ListLevel { format: fmt, text: String::new(), start: value, marker_style: marker_style.clone(), indent_left: indent, hanging: -p.indent_first.min(0.0) }] });
                    open = Some(Open { list: lists.len() - 1, levels: vec![(fmt, value.saturating_sub(1), indent)], last_indent: indent });
                    0
                }
            };
            let o = open.as_mut().unwrap();
            o.levels[level].1 = value;
            o.last_indent = indent;
            let def = &mut lists[o.list];
            let lvl = &mut def.levels[level];
            if lvl.text.is_empty() {
                lvl.text = if fmt == NumFormat::Bullet { pattern.clone() } else { pattern.replace("%N", &format!("%{}", level + 1)) };
            }
            // Remove the typed marker and its tab: Word draws them now.
            p.inlines.drain(..2);
            p.list = Some(ListRef { list: o.list, level: level as u8 });
        }
    }
    lists
}

// ---------------------------------------------------------------------------
// Footnotes
// ---------------------------------------------------------------------------

fn is_note_marker(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s.chars().count() <= 3 && (s.chars().all(|c| c.is_ascii_digit()) || s.chars().all(|c| "*†‡§¶".contains(c)) || (s.len() == 1 && s.chars().all(|c| c.is_ascii_lowercase())))
}

/// Moves notes at the bottom of pages whose marker appears as a superscript in
/// the body into Word footnotes.
pub fn detect_footnotes(pages: &mut [PageLayout], body_size: f32) -> Vec<Footnote> {
    let mut notes: Vec<Footnote> = Vec::new();
    for page in pages.iter_mut() {
        // Candidate note paragraphs at the end of the page.
        let mut cand: Vec<(usize, String)> = Vec::new();
        let mut i = page.blocks.len();
        while i > 0 {
            i -= 1;
            let Block::Paragraph(p) = &page.blocks[i] else { break };
            if p.font_size > body_size * 0.92 || p.bbox.y0 < page.height * 0.5 {
                break;
            }
            let marker = match p.inlines.first() {
                Some(Inline::Text { text, style, .. }) if style.vert == VertAlign::Superscript && is_note_marker(text) => Some(text.trim().to_string()),
                _ => None,
            };
            match marker {
                Some(m) => cand.push((i, m)),
                None if !cand.is_empty() || i + 1 == page.blocks.len() => continue,
                None => break,
            }
        }
        if cand.is_empty() {
            continue;
        }
        cand.reverse();
        let first_note = cand[0].0;
        // Match each marker with a superscript in the body above the notes.
        let mut matched: Vec<(usize, usize)> = Vec::new(); // (block index of note, footnote id)
        for (bi, marker) in &cand {
            let id = notes.len() + matched.len();
            let mut done = false;
            for b in page.blocks[..first_note].iter_mut() {
                if done {
                    break;
                }
                for_each_paragraph(std::slice::from_mut(b), &mut |p: &mut Paragraph| {
                    if done {
                        return;
                    }
                    for k in 0..p.inlines.len() {
                        if let Inline::Text { text, style, .. } = &p.inlines[k] {
                            if style.vert == VertAlign::Superscript && text.trim() == marker {
                                // Keep the space that followed the marker, in the surrounding style.
                                let trailing = text.len() > text.trim_end().len();
                                let base = p.inlines[..k].iter().rev().find_map(|i| match i {
                                    Inline::Text { style, .. } if style.vert == VertAlign::Baseline => Some(style.clone()),
                                    _ => None,
                                });
                                p.inlines[k] = Inline::FootnoteRef(id);
                                if trailing {
                                    let style = base.unwrap_or_default();
                                    p.inlines.insert(k + 1, Inline::Text { text: " ".into(), style, link: None, pdf_width: 0.0, pdf_tracking: 0.0 });
                                }
                                done = true;
                                return;
                            }
                        }
                    }
                });
            }
            if done {
                matched.push((*bi, id));
            }
        }
        if matched.is_empty() {
            continue;
        }
        // Collect note paragraphs (each note runs until the next marker).
        let note_starts: Vec<usize> = matched.iter().map(|m| m.0).collect();
        let mut new_notes: Vec<Footnote> = Vec::new();
        let mut remove = vec![false; page.blocks.len()];
        for (k, (start, _)) in matched.iter().enumerate() {
            let end = note_starts.get(k + 1).copied().unwrap_or(page.blocks.len());
            let mut blocks = Vec::new();
            for (j, b) in page.blocks.iter().enumerate().take(end).skip(*start) {
                if let Block::Paragraph(p) = b {
                    let mut p = p.clone();
                    if j == *start {
                        p.inlines.remove(0);
                        if let Some(Inline::Text { text, .. }) = p.inlines.first_mut() {
                            *text = text.trim_start().to_string();
                        }
                    }
                    p.space_before = 0.0;
                    p.indent_left = 0.0;
                    p.indent_first = 0.0;
                    p.line_pitch = None;
                    blocks.push(Block::Paragraph(p));
                }
                remove[j] = true;
            }
            new_notes.push(Footnote { blocks });
        }
        // The separator line above the notes goes too.
        if first_note > 0 {
            if let Block::Rule(r) = &page.blocks[first_note - 1] {
                if r.bbox.width() < page.width * 0.5 {
                    remove[first_note - 1] = true;
                }
            }
        }
        let mut k = 0;
        page.blocks.retain(|_| {
            let keep = !remove[k];
            k += 1;
            keep
        });
        notes.extend(new_notes);
    }
    notes
}

// ---------------------------------------------------------------------------
// Table header rows
// ---------------------------------------------------------------------------

fn row_signature(r: &Row) -> (bool, Option<Rgb>) {
    let mut bold = true;
    let mut any = false;
    for c in &r.cells {
        for b in &c.blocks {
            if let Block::Paragraph(p) = b {
                if let Some(st) = dominant_style(p) {
                    any = true;
                    bold &= st.bold;
                }
            }
        }
    }
    let shading = r.cells.iter().find_map(|c| c.shading);
    (any && bold, shading)
}

/// A first row set apart (bold or shaded unlike the next row) becomes a
/// header row, which Word repeats when the table breaks across pages.
pub fn mark_header_rows(pages: &mut [PageLayout]) {
    fn visit(blocks: &mut [Block]) {
        for b in blocks {
            if let Block::Table(t) = b {
                if !t.layout_only && t.header_rows == 0 && t.rows.len() >= 3 {
                    let (b0, s0) = row_signature(&t.rows[0]);
                    let (b1, s1) = row_signature(&t.rows[1]);
                    if (b0 && !b1) || (s0.is_some() && s0 != s1) {
                        t.header_rows = 1;
                    }
                }
                for r in &mut t.rows {
                    for c in &mut r.cells {
                        visit(&mut c.blocks);
                    }
                }
            }
        }
    }
    for p in pages {
        visit(&mut p.blocks);
    }
}

// ---------------------------------------------------------------------------
// Captions and styles
// ---------------------------------------------------------------------------

fn is_caption_text(t: &str) -> bool {
    let t = t.trim_start();
    const WORDS: &[&str] = &["figure", "fig.", "table", "chart", "image", "exhibit", "listing", "abbildung", "abb.", "tabelle", "tableau", "tabla", "figura"];
    let lower = t.to_lowercase();
    WORDS.iter().any(|w| {
        lower.strip_prefix(w).is_some_and(|rest| {
            let rest = rest.trim_start();
            rest.chars().next().is_some_and(|c| c.is_ascii_digit() || c.is_ascii_uppercase() && rest.len() > 1 && rest.as_bytes()[1] == b'.')
        })
    })
}

/// Run format key: font, half-point size, bold, italic, colour.
type Sig = (String, i32, bool, bool, Rgb);

fn signature(st: &TextStyle) -> Sig {
    (st.font.clone(), (st.size * 2.0).round() as i32, st.bold, st.italic, st.color)
}

fn def_from(id: &str, name: &str, st: &TextStyle, outline: Option<u8>) -> StyleDef {
    StyleDef { id: id.into(), name: name.into(), font: st.font.clone(), size: st.size, bold: st.bold, italic: st.italic, color: st.color, outline }
}

/// Marks captions and builds paragraph styles: Normal from the dominant body
/// format, Heading 1–3 and Caption from their paragraphs, and named styles for
/// other recurring formats. Paragraphs reference the styles; the writer then
/// only writes run formatting that differs from the paragraph's style.
pub fn assign_styles(pages: &mut [PageLayout], footnotes: &mut [Footnote]) -> Vec<StyleDef> {
    // Captions: next to a picture or table.
    for page in pages.iter_mut() {
        let n = page.blocks.len();
        for i in 0..n {
            let near_figure = (i > 0 && matches!(page.blocks[i - 1], Block::Picture(_) | Block::Table(_)))
                || (i + 1 < n && matches!(page.blocks[i + 1], Block::Picture(_) | Block::Table(_)));
            if let Block::Paragraph(p) = &mut page.blocks[i] {
                if near_figure && p.heading.is_none() && p.n_lines <= 3 && first_text(p).is_some_and(is_caption_text) {
                    p.style = Some("Caption".into());
                }
            }
        }
    }
    // Format statistics.
    let mut body: HashMap<Sig, (usize, TextStyle)> = HashMap::new();
    let mut heading: HashMap<(u8, Sig), (usize, TextStyle)> = HashMap::new();
    let mut caption: HashMap<Sig, (usize, TextStyle)> = HashMap::new();
    let mut paragraphs = 0usize;
    for page in pages.iter_mut() {
        for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| {
            let Some(st) = dominant_style(p) else { return };
            let chars = p.plain_text().chars().filter(|c| !c.is_whitespace()).count().max(1);
            let sig = signature(&st);
            if let Some(h) = p.heading {
                heading.entry((h, sig)).or_insert((0, st)).0 += chars;
            } else if p.style.as_deref() == Some("Caption") {
                caption.entry(sig).or_insert((0, st)).0 += chars;
            } else {
                paragraphs += 1;
                body.entry(sig).or_insert((0, st)).0 += chars;
            }
        });
    }
    let mut styles = Vec::new();
    let normal = body.values().max_by_key(|(n, _)| *n).map(|(_, st)| st.clone()).unwrap_or_default();
    styles.push(def_from("Normal", "Normal", &normal, None));
    for level in 1..=3u8 {
        if let Some((_, st)) = heading.iter().filter(|((l, _), _)| *l == level).map(|(_, v)| v).max_by_key(|(n, _)| *n) {
            styles.push(def_from(&format!("Heading{level}"), &format!("heading {level}"), st, Some(level - 1)));
        }
    }
    if let Some((_, st)) = caption.values().max_by_key(|(n, _)| *n) {
        styles.push(def_from("Caption", "caption", st, None));
    }
    // Other recurring body formats (by number of paragraphs, not characters).
    let mut para_counts: HashMap<Sig, usize> = HashMap::new();
    for page in pages.iter_mut() {
        for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| {
            if p.heading.is_none() && p.style.is_none() {
                if let Some(st) = dominant_style(p) {
                    *para_counts.entry(signature(&st)).or_default() += 1;
                }
            }
        });
    }
    let normal_sig = signature(&normal);
    let mut extra: Vec<(Sig, usize)> =
        para_counts.into_iter().filter(|(sig, n)| *sig != normal_sig && *n >= 3 && *n * 20 >= paragraphs).collect();
    extra.sort_by_key(|e| std::cmp::Reverse(e.1));
    let mut extra_ids: HashMap<Sig, String> = HashMap::new();
    for (k, (sig, _)) in extra.into_iter().take(4).enumerate() {
        let st = &body[&sig].1;
        let (id, name) = if st.size < normal.size * 0.9 {
            (format!("SmallText{}", k + 1), format!("Small Text {}", k + 1))
        } else {
            (format!("BodyText{}", k + 2), format!("Body Text {}", k + 2))
        };
        styles.push(def_from(&id, &name, st, None));
        extra_ids.insert(sig, id);
    }
    for page in pages.iter_mut() {
        for_each_paragraph(&mut page.blocks, &mut |p: &mut Paragraph| {
            if p.heading.is_none() && p.style.is_none() {
                if let Some(id) = dominant_style(p).and_then(|st| extra_ids.get(&signature(&st))) {
                    p.style = Some(id.clone());
                }
            }
        });
    }
    // Footnote text uses its own style when notes exist.
    if let Some(st) = footnotes.iter_mut().flat_map(|f| f.blocks.iter_mut()).find_map(|b| match b {
        Block::Paragraph(p) => dominant_style(p),
        _ => None,
    }) {
        styles.push(def_from("FootnoteText", "footnote text", &st, None));
        for f in footnotes.iter_mut() {
            for_each_paragraph(&mut f.blocks, &mut |p: &mut Paragraph| p.style = Some("FootnoteText".into()));
        }
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers() {
        assert_eq!(parse_marker("•").map(|m| m.0), Some(NumFormat::Bullet));
        assert_eq!(parse_marker("3."), Some((NumFormat::Decimal, "%N.".into(), 3)));
        assert_eq!(parse_marker("(b)"), Some((NumFormat::LowerLetter, "(%N)".into(), 2)));
        assert_eq!(parse_marker("iv)"), Some((NumFormat::LowerRoman, "%N)".into(), 4)));
        assert_eq!(parse_marker("IV."), Some((NumFormat::UpperRoman, "%N.".into(), 4)));
        assert_eq!(parse_marker("Word"), None);
        assert_eq!(parse_marker("12"), None);
    }

    #[test]
    fn page_numbers_and_captions() {
        assert!(is_page_number(&normalise("Page 3 of 10")));
        assert!(is_page_number(&normalise("- 12 -")));
        assert!(is_page_number(&normalise("iv")));
        assert!(!is_page_number(&normalise("Chapter 3")));
        assert!(is_caption_text("Figure 1: Revenue"));
        assert!(is_caption_text("Table 2.3 Results"));
        assert!(!is_caption_text("Figures show that"));
    }

    #[test]
    fn page_fields() {
        let st = TextStyle::default();
        let mut blocks = vec![Block::Paragraph(Paragraph {
            inlines: vec![Inline::Text { text: "Page 7 of 12".into(), style: st, link: None, pdf_width: 0.0, pdf_tracking: 0.0 }],
            ..Default::default()
        })];
        insert_page_fields(&mut blocks, 7, 12);
        let Block::Paragraph(p) = &blocks[0] else { panic!() };
        assert!(matches!(p.inlines[1], Inline::Field { kind: FieldKind::PageNumber, .. }));
        assert!(matches!(p.inlines[3], Inline::Field { kind: FieldKind::PageCount, .. }));
    }
}
