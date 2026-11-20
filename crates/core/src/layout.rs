//! Layout reconstruction: glyphs → segments → regions (recursive XY-cut) →
//! paragraphs, multi-column layouts, tables, lists, pictures and rules.

use std::collections::HashMap;

use crate::model::*;

#[derive(Clone, Debug)]
pub struct LayoutOptions {
    /// Detect ruled (grid) tables and whitespace-aligned tables.
    pub detect_tables: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self { detect_tables: true }
    }
}

/// Approximate descent of a font relative to its size (used to place
/// baselines inside exactly-spaced Word lines).
pub const DESCENT: f32 = 0.22;

// ---------------------------------------------------------------------------
// Segments: runs of glyphs on one baseline without large gaps
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Seg {
    glyphs: Vec<usize>,
    bbox: Rect,
    baseline: f32,
    size: f32,
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

fn gsize(raw: &RawPage, g: &Glyph) -> f32 {
    raw.styles[g.style].size.max(1.0)
}

/// Maximum horizontal gap (relative to font size) inside a segment.
const SPLIT_GAP: f32 = 1.0;

fn is_bullet(c: char) -> bool {
    matches!(
        c,
        '•' | '◦' | '▪' | '▫' | '■' | '□' | '●' | '○' | '◆' | '◇' | '►' | '▶' | '➢' | '✓' | '✔' | '–' | '—' | '-' | '*' | '·' | '\u{F0B7}' | '\u{F0A7}' | '\u{F0D8}'
    )
}

/// True if `s` looks like a list marker: "•", "1.", "a)", "(iv)", "2.3".
fn is_list_marker(s: &str) -> bool {
    let s = s.trim();
    let n = s.chars().count();
    if n == 0 || n > 6 {
        return false;
    }
    if n == 1 && is_bullet(s.chars().next().unwrap()) {
        return true;
    }
    let inner = s.trim_start_matches('(').trim_end_matches(['.', ')', ':']);
    if inner.is_empty() || inner.len() == s.len() {
        return false;
    }
    let numeric = inner.chars().all(|c| c.is_ascii_digit() || c == '.');
    let alpha = inner.chars().count() == 1 && inner.chars().all(|c| c.is_ascii_alphabetic());
    let roman = inner.chars().all(|c| "ivxlcIVXLC".contains(c)) && inner.len() <= 4;
    numeric || alpha || roman
}

/// Whether two boxes with the given baselines and font sizes sit on the same
/// text line. Smaller raised/lowered text (super/subscripts) counts as well.
fn same_line(a_box: &Rect, a_base: f32, a_size: f32, b_box: &Rect, b_base: f32, b_size: f32) -> bool {
    let size = a_size.max(b_size);
    let d = (a_base - b_base).abs();
    if d <= 0.25 * size {
        return true;
    }
    let script = a_size.min(b_size) < 0.92 * size;
    script && d <= 0.6 * size && a_box.v_overlap(b_box) >= 0.45 * a_box.height().min(b_box.height())
}

fn seg_text(raw: &RawPage, seg: &Seg) -> String {
    seg.glyphs.iter().map(|&i| raw.glyphs[i].ch).collect()
}

fn build_segments(raw: &RawPage) -> Vec<Seg> {
    let mut segs: Vec<Seg> = Vec::new();
    let mut cur: Option<Seg> = None;
    for (i, g) in raw.glyphs.iter().enumerate() {
        let size = gsize(raw, g);
        if let Some(c) = cur.as_mut() {
            let last = &raw.glyphs[*c.glyphs.last().unwrap()];
            let min_size = size.min(c.size);
            let last_size = gsize(raw, last);
            let same_line = same_line(&last.bbox, last.baseline, last_size, &g.bbox, g.baseline, size)
                || ((g.baseline - c.baseline).abs() <= 0.25 * c.size.max(size)
                    && g.bbox.v_overlap(&c.bbox) >= 0.4 * g.bbox.height().min(c.bbox.height()));
            let gap = g.bbox.x0 - last.bbox.x1;
            let forward = gap >= -0.6 * min_size;
            if same_line && forward && gap <= SPLIT_GAP * c.size.max(size) {
                c.glyphs.push(i);
                c.bbox = c.bbox.union(&g.bbox);
                if size > c.size * 1.1 {
                    c.baseline = g.baseline;
                    c.size = size;
                }
                continue;
            }
            segs.push(cur.take().unwrap());
        }
        cur = Some(Seg { glyphs: vec![i], bbox: g.bbox, baseline: g.baseline, size });
    }
    if let Some(c) = cur {
        segs.push(c);
    }

    // Merge fragments that belong together (out-of-order content streams,
    // superscripts, list markers followed by their text).
    segs.sort_by(|a, b| a.baseline.total_cmp(&b.baseline).then(a.bbox.x0.total_cmp(&b.bbox.x0)));
    let mut merged = true;
    while merged {
        merged = false;
        let mut i = 0;
        while i < segs.len() {
            let mut j = i + 1;
            while j < segs.len() {
                let (a, b) = (&segs[i], &segs[j]);
                if b.bbox.y0 > a.bbox.y1 + a.size {
                    break;
                }
                let size = a.size.max(b.size);
                if same_line(&a.bbox, a.baseline, a.size, &b.bbox, b.baseline, b.size) {
                    let (l, r) = if a.bbox.x0 <= b.bbox.x0 { (a, b) } else { (b, a) };
                    let gap = r.bbox.x0 - l.bbox.x1;
                    let marker_join = is_list_marker(&seg_text(raw, l)) && gap <= 4.0 * size && gap > 0.0;
                    if gap <= SPLIT_GAP * size || marker_join {
                        let b = segs.remove(j);
                        let a = &mut segs[i];
                        a.glyphs.extend(b.glyphs);
                        a.glyphs.sort_by(|&x, &y| raw.glyphs[x].bbox.x0.total_cmp(&raw.glyphs[y].bbox.x0));
                        a.bbox = a.bbox.union(&b.bbox);
                        if b.size > a.size * 1.1 {
                            a.size = b.size;
                            a.baseline = b.baseline;
                        }
                        merged = true;
                        continue;
                    }
                }
                j += 1;
            }
            i += 1;
        }
    }
    // Make sure glyphs are ordered left to right.
    for s in &mut segs {
        s.glyphs.sort_by(|&x, &y| raw.glyphs[x].bbox.x0.total_cmp(&raw.glyphs[y].bbox.x0));
    }
    segs
}

// ---------------------------------------------------------------------------
// Decorations: underline / strike-through / highlight from vector graphics
// ---------------------------------------------------------------------------

fn apply_decorations(raw: &mut RawPage) {
    let mut used_rules = vec![false; raw.rules.len()];
    let mut used_boxes = vec![false; raw.boxes.len()];
    let mut new_styles: Vec<(usize, TextStyle)> = Vec::new();
    for (gi, g) in raw.glyphs.iter().enumerate() {
        let st = &raw.styles[g.style];
        let size = st.size;
        let mut style = st.clone();
        let mut changed = false;
        for (ri, r) in raw.rules.iter().enumerate() {
            if r.orientation != RuleOrientation::Horizontal || r.width > 0.15 * size + 0.5 {
                continue;
            }
            if r.bbox.h_overlap(&g.bbox) < 0.6 * g.bbox.width() {
                continue;
            }
            let y = r.bbox.cy();
            if y >= g.baseline - 0.05 * size && y <= g.baseline + 0.3 * size {
                style.underline = true;
                used_rules[ri] = true;
                changed = true;
            } else if y >= g.baseline - 0.5 * size && y <= g.baseline - 0.15 * size {
                style.strike = true;
                used_rules[ri] = true;
                changed = true;
            }
        }
        for (bi, b) in raw.boxes.iter().enumerate() {
            if b.bbox.height() > 2.5 * size || b.bbox.height() < 0.6 * size {
                continue;
            }
            if b.bbox.contains_point(g.bbox.cx(), g.baseline - 0.3 * size) && b.color != style.color {
                style.highlight = Some(b.color);
                used_boxes[bi] = true;
                changed = true;
            }
        }
        if changed {
            new_styles.push((gi, style));
        }
    }
    for (gi, st) in new_styles {
        let id = raw.style_id(st);
        raw.glyphs[gi].style = id;
    }
    let mut k = 0;
    raw.rules.retain(|_| {
        let keep = !used_rules[k];
        k += 1;
        keep
    });
    let mut k = 0;
    raw.boxes.retain(|_| {
        let keep = !used_boxes[k];
        k += 1;
        keep
    });
}

// ---------------------------------------------------------------------------
// Ruled tables
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct GridCell {
    rect: Rect,
    row: usize,
    col: usize,
    row_span: usize,
    col_span: usize,
    borders: [Option<Border>; 4], // top, bottom, left, right
    shading: Option<Rgb>,
}

#[derive(Clone, Debug)]
struct RuledTable {
    bbox: Rect,
    xs: Vec<f32>,
    ys: Vec<f32>,
    cells: Vec<GridCell>,
}

fn cluster_positions(mut v: Vec<f32>, tol: f32) -> Vec<f32> {
    v.sort_by(|a, b| a.total_cmp(b));
    let mut out: Vec<(f32, usize)> = Vec::new();
    for x in v {
        if let Some(last) = out.last_mut() {
            if x - last.0 / last.1 as f32 <= tol {
                last.0 += x;
                last.1 += 1;
                continue;
            }
        }
        out.push((x, 1));
    }
    out.into_iter().map(|(s, n)| s / n as f32).collect()
}

/// Merges collinear, touching rules.
fn merge_rules(rules: &[Rule]) -> Vec<Rule> {
    let mut hs: Vec<Rule> = rules.iter().filter(|r| r.orientation == RuleOrientation::Horizontal).copied().collect();
    let mut vs: Vec<Rule> = rules.iter().filter(|r| r.orientation == RuleOrientation::Vertical).copied().collect();
    hs.sort_by(|a, b| a.bbox.cy().total_cmp(&b.bbox.cy()).then(a.bbox.x0.total_cmp(&b.bbox.x0)));
    vs.sort_by(|a, b| a.bbox.cx().total_cmp(&b.bbox.cx()).then(a.bbox.y0.total_cmp(&b.bbox.y0)));
    let mut out = Vec::new();
    let mut acc: Option<Rule> = None;
    for r in hs {
        match acc.as_mut() {
            Some(a) if (a.bbox.cy() - r.bbox.cy()).abs() <= 1.5 && r.bbox.x0 <= a.bbox.x1 + 2.0 => {
                a.bbox = a.bbox.union(&r.bbox);
            }
            _ => {
                if let Some(a) = acc.take() {
                    out.push(a);
                }
                acc = Some(r);
            }
        }
    }
    out.extend(acc.take());
    for r in vs {
        match acc.as_mut() {
            Some(a) if (a.bbox.cx() - r.bbox.cx()).abs() <= 1.5 && r.bbox.y0 <= a.bbox.y1 + 2.0 => {
                a.bbox = a.bbox.union(&r.bbox);
            }
            _ => {
                if let Some(a) = acc.take() {
                    out.push(a);
                }
                acc = Some(r);
            }
        }
    }
    out.extend(acc);
    out
}

fn detect_ruled_tables(raw: &RawPage) -> (Vec<RuledTable>, Vec<bool>) {
    let rules = merge_rules(&raw.rules);
    let n = rules.len();
    let mut consumed_raw = vec![false; raw.rules.len()];
    if n < 4 {
        return (vec![], consumed_raw);
    }
    let tol = 2.5;
    let touches = |a: &Rule, b: &Rule| a.bbox.expand(tol).intersects(&b.bbox.expand(tol));
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            if rules[i].orientation != rules[j].orientation && touches(&rules[i], &rules[j]) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        comps.entry(r).or_default().push(i);
    }
    let mut tables = Vec::new();
    for (_, idxs) in comps {
        let hs: Vec<&Rule> = idxs.iter().map(|&i| &rules[i]).filter(|r| r.orientation == RuleOrientation::Horizontal).collect();
        let vs: Vec<&Rule> = idxs.iter().map(|&i| &rules[i]).filter(|r| r.orientation == RuleOrientation::Vertical).collect();
        if hs.len() < 2 || vs.len() < 2 {
            continue;
        }
        let xs = cluster_positions(vs.iter().map(|r| r.bbox.cx()).collect(), 3.0);
        let ys = cluster_positions(hs.iter().map(|r| r.bbox.cy()).collect(), 3.0);
        if xs.len() < 2 || ys.len() < 2 {
            continue;
        }
        let bbox = Rect::new(xs[0], ys[0], *xs.last().unwrap(), *ys.last().unwrap());
        if bbox.width() < 20.0 || bbox.height() < 8.0 {
            continue;
        }
        let (nc, nr) = (xs.len() - 1, ys.len() - 1);
        // Remove degenerate thin columns/rows (double borders).
        if xs.windows(2).any(|w| w[1] - w[0] < 3.0) || ys.windows(2).any(|w| w[1] - w[0] < 3.0) {
            // Still usable, but cluster tolerance should have merged them; skip odd grids.
            continue;
        }
        // Coverage helpers.
        let h_cover = |y: f32, xa: f32, xb: f32| -> Option<&Rule> {
            hs.iter()
                .find(|r| (r.bbox.cy() - y).abs() <= tol && r.bbox.x0 <= xa + tol + 1.0 && r.bbox.x1 >= xb - tol - 1.0)
                .copied()
        };
        let v_cover = |x: f32, ya: f32, yb: f32| -> Option<&Rule> {
            vs.iter()
                .find(|r| (r.bbox.cx() - x).abs() <= tol && r.bbox.y0 <= ya + tol + 1.0 && r.bbox.y1 >= yb - tol - 1.0)
                .copied()
        };
        // Union-find over grid cells for merged cells.
        let idx = |r: usize, c: usize| r * nc + c;
        let mut p: Vec<usize> = (0..nr * nc).collect();
        for r in 0..nr {
            for c in 0..nc {
                if c + 1 < nc && v_cover(xs[c + 1], ys[r], ys[r + 1]).is_none() {
                    let (a, b) = (find(&mut p, idx(r, c)), find(&mut p, idx(r, c + 1)));
                    if a != b {
                        p[a] = b;
                    }
                }
                if r + 1 < nr && h_cover(ys[r + 1], xs[c], xs[c + 1]).is_none() {
                    let (a, b) = (find(&mut p, idx(r, c)), find(&mut p, idx(r + 1, c)));
                    if a != b {
                        p[a] = b;
                    }
                }
            }
        }
        let mut groups: HashMap<usize, (usize, usize, usize, usize, usize)> = HashMap::new(); // r0,c0,r1,c1,count
        for r in 0..nr {
            for c in 0..nc {
                let root = find(&mut p, idx(r, c));
                let e = groups.entry(root).or_insert((r, c, r, c, 0));
                e.0 = e.0.min(r);
                e.1 = e.1.min(c);
                e.2 = e.2.max(r);
                e.3 = e.3.max(c);
                e.4 += 1;
            }
        }
        // Every merged group must be rectangular, otherwise fall back to no merging.
        let rectangular = groups.values().all(|&(r0, c0, r1, c1, n)| (r1 - r0 + 1) * (c1 - c0 + 1) == n);
        let mut cells = Vec::new();
        let mk_border = |r: Option<&Rule>| r.map(|r| Border { color: r.color, width: r.width });
        if rectangular {
            for &(r0, c0, r1, c1, _) in groups.values() {
                let rect = Rect::new(xs[c0], ys[r0], xs[c1 + 1], ys[r1 + 1]);
                cells.push(GridCell {
                    rect,
                    row: r0,
                    col: c0,
                    row_span: r1 - r0 + 1,
                    col_span: c1 - c0 + 1,
                    borders: [
                        mk_border(h_cover(rect.y0, rect.x0, rect.x1)),
                        mk_border(h_cover(rect.y1, rect.x0, rect.x1)),
                        mk_border(v_cover(rect.x0, rect.y0, rect.y1)),
                        mk_border(v_cover(rect.x1, rect.y0, rect.y1)),
                    ],
                    shading: None,
                });
            }
        } else {
            for r in 0..nr {
                for c in 0..nc {
                    let rect = Rect::new(xs[c], ys[r], xs[c + 1], ys[r + 1]);
                    cells.push(GridCell {
                        rect,
                        row: r,
                        col: c,
                        row_span: 1,
                        col_span: 1,
                        borders: [
                            mk_border(h_cover(rect.y0, rect.x0, rect.x1)),
                            mk_border(h_cover(rect.y1, rect.x0, rect.x1)),
                            mk_border(v_cover(rect.x0, rect.y0, rect.y1)),
                            mk_border(v_cover(rect.x1, rect.y0, rect.y1)),
                        ],
                        shading: None,
                    });
                }
            }
        }
        for cell in &mut cells {
            cell.shading = raw
                .boxes
                .iter()
                .filter(|b| cell.rect.fraction_inside(&b.bbox.expand(2.0)) > 0.8)
                .map(|b| b.color)
                .next_back();
        }
        cells.sort_by_key(|c| (c.row, c.col));
        tables.push(RuledTable { bbox, xs, ys, cells });
    }
    // Mark original rules that belong to a table.
    for (i, r) in raw.rules.iter().enumerate() {
        if tables.iter().any(|t| r.bbox.fraction_inside(&t.bbox.expand(3.0)) > 0.9) {
            consumed_raw[i] = true;
        }
    }
    tables.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
    (tables, consumed_raw)
}

// ---------------------------------------------------------------------------
// XY-cut
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum ItemKind {
    Seg(usize),
    Image(usize),
    Table(usize),
    Rule(usize),
}

#[derive(Clone, Copy, Debug)]
struct Item {
    kind: ItemKind,
    bbox: Rect,
}

#[derive(Debug)]
enum Node {
    Leaf(Vec<usize>),
    Rows(Vec<Node>),
    Cols(Vec<Node>),
}

impl Node {
    fn items(&self, out: &mut Vec<usize>) {
        match self {
            Node::Leaf(v) => out.extend(v.iter().copied()),
            Node::Rows(c) | Node::Cols(c) => c.iter().for_each(|n| n.items(out)),
        }
    }
    fn all_items(&self) -> Vec<usize> {
        let mut v = Vec::new();
        self.items(&mut v);
        v
    }
}

struct Ctx<'a> {
    raw: &'a RawPage,
    segs: &'a [Seg],
    items: &'a [Item],
    body_size: f32,
}

impl Ctx<'_> {
    fn bbox(&self, idxs: &[usize]) -> Rect {
        Rect::union_all(idxs.iter().map(|&i| &self.items[i].bbox)).unwrap_or_default()
    }

    fn size_of(&self, idxs: &[usize]) -> f32 {
        let mut v: Vec<f32> = idxs
            .iter()
            .filter_map(|&i| match self.items[i].kind {
                ItemKind::Seg(s) => Some(self.segs[s].size),
                _ => None,
            })
            .collect();
        if v.is_empty() {
            return self.body_size;
        }
        // Lower median: a few large headings should not inflate the thresholds.
        v.sort_by(|a, b| a.total_cmp(b));
        v[(v.len() - 1) / 2]
    }

    fn line_count(&self, idxs: &[usize]) -> usize {
        let mut bases: Vec<f32> = idxs
            .iter()
            .filter_map(|&i| match self.items[i].kind {
                ItemKind::Seg(s) => Some(self.segs[s].baseline),
                _ => None,
            })
            .collect();
        bases.sort_by(|a, b| a.total_cmp(b));
        let mut n = 0;
        let mut last = f32::MIN;
        for b in bases {
            if b - last > 2.0 {
                n += 1;
                last = b;
            }
        }
        n
    }

    /// Whitespace gutters running across the whole group (vertical cuts).
    fn gutters(&self, idxs: &[usize]) -> Vec<(f32, f32)> {
        let size = self.size_of(idxs);
        let lines = self.line_count(idxs);
        let has_non_text = idxs.iter().any(|&i| !matches!(self.items[i].kind, ItemKind::Seg(_)));
        let min_gap = if lines <= 1 && !has_non_text {
            SPLIT_GAP * size
        } else if lines <= 2 {
            (1.2 * size).max(8.0)
        } else {
            (0.75 * size).max(6.0)
        };
        let mut iv: Vec<(f32, f32)> = idxs.iter().map(|&i| (self.items[i].bbox.x0, self.items[i].bbox.x1)).collect();
        iv.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps = Vec::new();
        let mut end = iv[0].1;
        for &(a, b) in &iv[1..] {
            if a - end >= min_gap {
                gaps.push((end, a));
            }
            end = end.max(b);
        }
        gaps
    }

    fn split_vertical(&self, idxs: &[usize]) -> Option<Vec<Vec<usize>>> {
        if idxs.len() < 2 {
            return None;
        }
        let gaps = self.gutters(idxs);
        if gaps.is_empty() {
            return None;
        }
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); gaps.len() + 1];
        for &i in idxs {
            let cx = self.items[i].bbox.x0;
            let k = gaps.iter().take_while(|g| cx >= g.1 - 0.01).count();
            groups[k].push(i);
        }
        groups.retain(|g| !g.is_empty());
        (groups.len() > 1).then_some(groups)
    }

    fn split_horizontal(&self, idxs: &[usize]) -> Option<Vec<Vec<usize>>> {
        if idxs.len() < 2 {
            return None;
        }
        let size = self.size_of(idxs);
        let min_gap = (0.45 * size).max(2.5);
        let mut order: Vec<usize> = idxs.to_vec();
        order.sort_by(|&a, &b| self.items[a].bbox.y0.total_cmp(&self.items[b].bbox.y0));
        let is_text = |i: usize| matches!(self.items[i].kind, ItemKind::Seg(_));
        let mut groups: Vec<Vec<usize>> = vec![vec![order[0]]];
        let mut end = self.items[order[0]].bbox.y1;
        let mut end_text = is_text(order[0]);
        for &i in &order[1..] {
            let b = self.items[i].bbox;
            // Text needs a paragraph-sized gap; pictures, tables and rules separate on any clear gap.
            let threshold = if end_text && is_text(i) { min_gap } else { 1.5 };
            if b.y0 - end >= threshold {
                groups.push(vec![i]);
                end = b.y1;
                end_text = is_text(i);
            } else {
                groups.last_mut().unwrap().push(i);
                if b.y1 > end {
                    end = b.y1;
                    end_text = is_text(i);
                }
            }
        }
        (groups.len() > 1).then_some(groups)
    }

    fn cut(&self, idxs: Vec<usize>, depth: usize) -> Node {
        if idxs.len() <= 1 || depth > 40 {
            return Node::Leaf(idxs);
        }
        if let Some(cols) = self.split_vertical(&idxs) {
            return Node::Cols(cols.into_iter().map(|g| self.cut(g, depth + 1)).collect());
        }
        if let Some(rows) = self.split_horizontal(&idxs) {
            let rows = self.merge_aligned_bands(rows);
            if rows.len() == 1 {
                let only = rows.into_iter().next().unwrap();
                return match self.split_vertical(&only) {
                    Some(cols) => Node::Cols(cols.into_iter().map(|g| self.cut(g, depth + 1)).collect()),
                    None => Node::Leaf(only),
                };
            }
            return Node::Rows(rows.into_iter().map(|g| self.cut(g, depth + 1)).collect());
        }
        Node::Leaf(idxs)
    }

    /// Consecutive horizontal bands that share a column gutter belong to the
    /// same multi-column area and are merged back together.
    fn merge_aligned_bands(&self, rows: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = Vec::new();
        for band in rows {
            if let Some(prev) = out.last_mut() {
                let gp = self.gutters(prev);
                let gb = self.gutters(&band);
                if !gp.is_empty() && !gb.is_empty() {
                    let mut merged = prev.clone();
                    merged.extend(band.iter().copied());
                    let aligned = gp.iter().any(|a| gb.iter().any(|b| a.1.min(b.1) - a.0.max(b.0) > 2.0));
                    // The merged band must still have a column structure.
                    if aligned && !self.gutters(&merged).is_empty() {
                        *prev = merged;
                        continue;
                    }
                }
            }
            out.push(band);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Lines and paragraphs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Line {
    segs: Vec<usize>,
    bbox: Rect,
    baseline: f32,
    size: f32,
    /// Height of the line's main-size text (without super/subscripts).
    main_height: f32,
    all_bold: bool,
    /// x position where the text after a list marker starts.
    marker_text_x: Option<f32>,
    starts_with_marker: bool,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    x0: f32,
    x1: f32,
}

impl Frame {
    fn width(&self) -> f32 {
        (self.x1 - self.x0).max(1.0)
    }
}

struct Emitter<'a> {
    ctx: Ctx<'a>,
    tables: &'a [RuledTable],
    floating: Vec<Picture>,
    detect_tables: bool,
}

/// Spacing bookkeeping inside a container: predicted Word y position.
struct Cursor {
    y: f32,
}

impl Emitter<'_> {
    fn raw(&self) -> &RawPage {
        self.ctx.raw
    }

    fn make_lines(&self, seg_ids: &[usize]) -> Vec<Line> {
        let segs = self.ctx.segs;
        let raw = self.raw();
        let mut order: Vec<usize> = seg_ids.to_vec();
        order.sort_by(|&a, &b| segs[a].baseline.total_cmp(&segs[b].baseline));
        let mut lines: Vec<Line> = Vec::new();
        for s in order {
            let seg = &segs[s];
            let found = lines.iter_mut().rev().take(3).find(|l| {
                same_line(&l.bbox, l.baseline, l.size, &seg.bbox, seg.baseline, seg.size)
                    && l.segs.iter().all(|&o| segs[o].bbox.h_overlap(&seg.bbox) < 0.3 * seg.bbox.width().min(segs[o].bbox.width()))
            });
            match found {
                Some(l) => {
                    l.segs.push(s);
                    l.bbox = l.bbox.union(&seg.bbox);
                    if seg.size > l.size * 1.1 {
                        l.size = seg.size;
                        l.baseline = seg.baseline;
                    }
                }
                None => lines.push(Line {
                    segs: vec![s],
                    bbox: seg.bbox,
                    baseline: seg.baseline,
                    size: seg.size,
                    main_height: 0.0,
                    all_bold: false,
                    marker_text_x: None,
                    starts_with_marker: false,
                }),
            }
        }
        for l in &mut lines {
            l.segs.sort_by(|&a, &b| segs[a].bbox.x0.total_cmp(&segs[b].bbox.x0));
            l.main_height = l
                .segs
                .iter()
                .flat_map(|&s| segs[s].glyphs.iter().map(|&g| &raw.glyphs[g]))
                .filter(|g| gsize(raw, g) >= 0.92 * l.size)
                .map(|g| g.bbox.height())
                .fold(0.0, f32::max);
            let glyphs: Vec<&Glyph> = l.segs.iter().flat_map(|&s| segs[s].glyphs.iter().map(|&g| &raw.glyphs[g])).collect();
            l.all_bold = glyphs.iter().filter(|g| g.ch.is_alphanumeric()).all(|g| raw.styles[g.style].bold)
                && glyphs.iter().any(|g| g.ch.is_alphanumeric());
            // List marker: first "word" of the line followed by a gap.
            let first_seg = &segs[l.segs[0]];
            let mut word = String::new();
            let mut end_x = first_seg.bbox.x0;
            let mut next_x = None;
            for (k, &gi) in first_seg.glyphs.iter().enumerate() {
                let g = &raw.glyphs[gi];
                if k > 0 && (g.space_before || g.bbox.x0 - end_x > 0.25 * gsize(raw, g)) {
                    next_x = Some(g.bbox.x0);
                    break;
                }
                word.push(g.ch);
                end_x = g.bbox.x1;
            }
            if next_x.is_none() && l.segs.len() > 1 {
                next_x = Some(segs[l.segs[1]].bbox.x0);
            }
            if is_list_marker(&word) {
                if let Some(nx) = next_x {
                    l.starts_with_marker = true;
                    if nx - end_x > 0.4 * l.size {
                        l.marker_text_x = Some(nx);
                    }
                }
            }
        }
        lines.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
        lines
    }

    /// Splits lines (top to bottom) into paragraph groups.
    fn group_paragraphs(&self, lines: &[Line], frame: Frame) -> Vec<Vec<usize>> {
        if lines.is_empty() {
            return vec![];
        }
        let mut pitches: Vec<f32> = lines
            .windows(2)
            .map(|w| w[1].baseline - w[0].baseline)
            .filter(|&p| p > 0.0 && p < 2.2 * lines[0].size.max(6.0))
            .collect();
        let typ_pitch = median(&mut pitches);
        let left = lines.iter().map(|l| l.bbox.x0).fold(f32::MAX, f32::min);
        let right = lines.iter().map(|l| l.bbox.x1).fold(f32::MIN, f32::max).max(frame.x1.min(right_guess(lines)));

        let mut groups: Vec<Vec<usize>> = vec![vec![0]];
        let mut para_text_x: Option<f32> = lines[0].marker_text_x;
        for i in 1..lines.len() {
            let (prev, cur) = (&lines[i - 1], &lines[i]);
            let size = prev.size.max(cur.size);
            let pitch = cur.baseline - prev.baseline;
            let group_len = groups.last().unwrap().len();
            let mut brk = false;
            // Extra vertical space.
            match typ_pitch {
                Some(tp) if pitch > tp * 1.3 + 1.0 => brk = true,
                None if pitch > 1.7 * size => brk = true,
                _ => {}
            }
            // Font size change.
            if (prev.size - cur.size).abs() > 0.15 * size {
                brk = true;
            }
            // Bold heading line followed by regular text (or vice versa).
            if prev.all_bold != cur.all_bold {
                brk = true;
            }
            // New list item.
            if cur.starts_with_marker {
                brk = true;
            }
            // Previous line ended well before the right edge → end of paragraph.
            let short_prev = prev.bbox.x1 < right - (3.0 * size).max(0.12 * (right - left).min(frame.width()));
            let centered_like = {
                let lm = prev.bbox.x0 - left;
                let rm = right - prev.bbox.x1;
                lm > 2.0 * size && (lm - rm).abs() < 2.0 * size
            };
            if short_prev && !centered_like {
                brk = true;
            }
            // Indentation changes.
            let indent_tol = 0.8 * cur.size;
            if let Some(tx) = para_text_x {
                // Inside a list item: continuation lines align with the item text.
                if (cur.bbox.x0 - tx).abs() > indent_tol && !cur.starts_with_marker {
                    brk = true;
                }
            } else if group_len == 1 {
                // Second line of a paragraph: it may be outdented (first-line indent).
                if cur.bbox.x0 > prev.bbox.x0 + indent_tol && !centered_like {
                    brk = true;
                }
            } else if (cur.bbox.x0 - prev.bbox.x0).abs() > indent_tol && !centered_like {
                brk = true;
            }
            if brk {
                groups.push(vec![i]);
                para_text_x = cur.marker_text_x;
            } else {
                groups.last_mut().unwrap().push(i);
            }
        }
        groups
    }

    /// Builds the inline content of one line.
    fn line_inlines(&self, line: &Line, frame: Frame, allow_tabs: bool, tabs: &mut Vec<TabStop>) -> Vec<Inline> {
        let atoms = self.line_atoms(line, frame, allow_tabs, tabs);
        atoms_to_inlines(atoms)
    }

    /// The characters, spaces and tabs of a line in visual (left-to-right) order.
    fn line_atoms(&self, line: &Line, frame: Frame, allow_tabs: bool, tabs: &mut Vec<TabStop>) -> Vec<Atom> {
        let raw = self.raw();
        let segs = self.ctx.segs;
        let mut out: Vec<Atom> = Vec::new();
        let line_size = line.size;
        let mut prev_x1: Option<f32> = None;

        for (si, &s) in line.segs.iter().enumerate() {
            let seg = &segs[s];
            if si > 0 {
                let gap = seg.bbox.x0 - prev_x1.unwrap_or(seg.bbox.x0);
                let is_marker_gap = si == 1 && line.marker_text_x.is_some_and(|x| (x - seg.bbox.x0).abs() < 1.0);
                if (allow_tabs && gap > SPLIT_GAP * line_size) || is_marker_gap {
                    out.push(Atom::Tab);
                    tabs.push(TabStop { pos: seg.bbox.x0 - frame.x0, kind: TabKind::Left });
                } else {
                    out.push(Atom::Space);
                }
            }
            let mut last_x1: Option<f32> = None;
            for &gi in &seg.glyphs {
                let g = &raw.glyphs[gi];
                let base_style = &raw.styles[g.style];
                let size = base_style.size;
                if let Some(lx) = last_x1 {
                    let gap = g.bbox.x0 - lx;
                    let marker_gap = line.marker_text_x.is_some_and(|x| si == 0 && (x - g.bbox.x0).abs() < 0.5);
                    if marker_gap {
                        out.push(Atom::Tab);
                        tabs.push(TabStop { pos: g.bbox.x0 - frame.x0, kind: TabKind::Left });
                    } else if (g.space_before && gap > 0.06 * size) || gap > 0.22 * size.min(line_size) {
                        out.push(Atom::Space);
                    }
                }
                last_x1 = Some(g.bbox.x1);
                // Super/subscript relative to the line baseline.
                let mut style = base_style.clone();
                if size < 0.85 * line_size {
                    let off = line.baseline - g.baseline;
                    if off > 0.12 * line_size {
                        style.vert = VertAlign::Superscript;
                    } else if off < -0.08 * line_size {
                        style.vert = VertAlign::Subscript;
                    }
                    if style.vert != VertAlign::Baseline {
                        // Keep the real size and reproduce the exact baseline offset.
                        style.baseline_shift = (off * 2.0).round() / 2.0;
                    }
                }
                out.push(Atom::Char {
                    text: expand_ligature(g.ch),
                    style,
                    link: g.link.and_then(|l| raw.links.get(l)).cloned(),
                    width: g.bbox.width(),
                    glyph: gi,
                });
            }
            prev_x1 = Some(seg.bbox.x1);
        }
        apply_letter_spacing(&mut out, raw);
        out
    }

    fn make_paragraph(&self, lines: &[&Line], frame: Frame) -> Paragraph {
        let n = lines.len();
        let bbox = Rect::union_all(lines.iter().map(|l| &l.bbox)).unwrap();
        let size = lines.iter().map(|l| l.size).fold(0.0, f32::max);
        let tol = (0.35 * size).max(2.0);
        let max_right = lines.iter().map(|l| l.bbox.x1).fold(f32::MIN, f32::max);
        let body_lefts: Vec<f32> = if n > 1 { lines[1..].iter().map(|l| l.bbox.x0).collect() } else { vec![lines[0].bbox.x0] };
        let body_left = body_lefts.iter().copied().fold(f32::MAX, f32::min);
        let lefts_aligned = body_lefts.iter().all(|x| (x - body_left).abs() <= tol);
        let rights: Vec<f32> = lines[..n.saturating_sub(1).max(1)].iter().map(|l| l.bbox.x1).collect();
        let rights_aligned = rights.iter().all(|x| (max_right - x).abs() <= tol);
        let centers: Vec<f32> = lines.iter().map(|l| l.bbox.cx()).collect();
        let c_mean = centers.iter().sum::<f32>() / n as f32;
        let centers_aligned = centers.iter().all(|c| (c - c_mean).abs() <= tol);
        let all_rights_aligned = lines.iter().all(|l| (max_right - l.bbox.x1).abs() <= tol);
        let fw = frame.width();
        let frame_c = (frame.x0 + frame.x1) / 2.0;

        let all_lefts_aligned = lines.iter().all(|l| (l.bbox.x0 - min_x0(lines)).abs() <= tol);
        let reaches_right = max_right >= frame.x1 - 2.0 * size;
        let align = if n >= 3 {
            if lefts_aligned && rights_aligned && (reaches_right || rights.len() >= 2) {
                Align::Justify
            } else if centers_aligned && !lefts_aligned {
                Align::Center
            } else if all_rights_aligned && !lefts_aligned {
                Align::Right
            } else {
                Align::Left
            }
        } else if n == 2 {
            let rights2 = (lines[0].bbox.x1 - lines[1].bbox.x1).abs() <= tol;
            if all_lefts_aligned && lines[0].bbox.x1 >= frame.x1 - size && reaches_right {
                Align::Justify
            } else if all_lefts_aligned {
                Align::Left
            } else if rights2 {
                Align::Right
            } else if centers_aligned {
                Align::Center
            } else {
                Align::Left
            }
        } else {
            let lm = bbox.x0 - frame.x0;
            let rm = frame.x1 - bbox.x1;
            if lm > 0.08 * fw && (lm - rm).abs() < (0.05 * fw).max(1.5 * size) {
                Align::Center
            } else if rm < (0.02 * fw).max(3.0) && lm > 0.3 * fw {
                Align::Right
            } else {
                Align::Left
            }
        };

        let mut tabs = Vec::new();
        let allow_tabs = n == 1 || align == Align::Left;
        let mut inlines: Vec<Inline> = Vec::new();
        // Lines that end well before the container's right edge were broken on purpose
        // (addresses, poetry, centred titles): keep those breaks in flow layout too.
        let hard_breaks = matches!(align, Align::Center | Align::Right)
            || lines[..n - 1].iter().all(|l| l.bbox.x1 < frame.x1 - (3.0 * size).max(0.15 * fw));
        for (k, l) in lines.iter().enumerate() {
            let li = self.line_inlines(l, frame, allow_tabs, &mut tabs);
            if k > 0 {
                if hard_breaks {
                    strip_trailing_space(&mut inlines);
                    inlines.push(Inline::LineBreak);
                } else {
                    let joiner = line_joiner(&mut inlines, &li);
                    inlines.push(Inline::SoftBreak(joiner));
                }
            }
            inlines.extend(li);
        }

        let (mut indent_left, mut indent_right, mut indent_first) = (0.0, 0.0, 0.0);
        match align {
            Align::Left | Align::Justify => {
                indent_left = (body_left - frame.x0).max(0.0);
                indent_first = lines[0].bbox.x0 - frame.x0 - indent_left;
                if let Some(tx) = lines[0].marker_text_x {
                    // Hanging indent for list items.
                    if n == 1 || (body_left - tx).abs() < tol {
                        indent_left = (tx - frame.x0).max(0.0);
                        indent_first = lines[0].bbox.x0 - tx;
                    }
                }
                let rm = frame.x1 - max_right;
                if hard_breaks {
                    indent_right = 0.0;
                } else if align == Align::Justify && rm > 1.0 {
                    indent_right = rm;
                } else if n >= 2 && rm > 3.0 * size {
                    indent_right = (rm - 1.0 * size).max(0.0);
                }
            }
            Align::Center => {
                let off = c_mean - frame_c;
                if off > 0.0 {
                    indent_left = 2.0 * off;
                } else {
                    indent_right = -2.0 * off;
                }
            }
            Align::Right => {
                indent_right = (frame.x1 - max_right).max(0.0);
            }
        }
        // Tab stops are measured from the container's left edge.
        tabs.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        tabs.dedup_by(|a, b| (a.pos - b.pos).abs() < 1.0);
        // A last tab stop flush with the right edge becomes a right tab.
        if let Some(last) = tabs.last_mut() {
            if n == 1 && (frame.x1 - max_right) < 2.0 && max_right - (last.pos + frame.x0) < 0.4 * fw {
                last.pos = max_right - frame.x0;
                last.kind = TabKind::Right;
            }
        }

        let mut pitches: Vec<f32> = lines.windows(2).map(|w| w[1].baseline - w[0].baseline).collect();
        let main_h = lines.iter().map(|l| l.main_height).fold(0.0, f32::max);
        let pitch = median(&mut pitches).filter(|p| *p >= 0.8 * size).unwrap_or((1.2 * size).max(main_h.min(1.5 * size)));

        Paragraph {
            inlines,
            align,
            indent_left,
            indent_right,
            indent_first,
            space_before: 0.0,
            space_after: 0.0,
            line_pitch: Some(pitch),
            first_baseline: lines[0].baseline,
            n_lines: n,
            font_size: size,
            tabs,
            heading: None,
            bbox,
            shading: None,
            rtl: false,
        }
    }

    /// Lays out the text segments of a container into paragraphs.
    fn paragraphs(&self, seg_ids: &[usize], frame: Frame) -> Vec<Paragraph> {
        let lines = self.make_lines(seg_ids);
        let groups = self.group_paragraphs(&lines, frame);
        groups
            .iter()
            .map(|g| {
                let ls: Vec<&Line> = g.iter().map(|&i| &lines[i]).collect();
                self.make_paragraph(&ls, frame)
            })
            .collect()
    }

    fn place_paragraph(&self, mut p: Paragraph, cursor: &mut Cursor, out: &mut Vec<Block>) {
        let pitch = p.line_pitch.unwrap_or(1.2 * p.font_size);
        let top = p.first_baseline - pitch + DESCENT * p.font_size;
        p.space_before = (top - cursor.y).max(0.0);
        cursor.y = cursor.y.max(top) + pitch * p.n_lines as f32;
        out.push(Block::Paragraph(p));
    }

    fn emit(&mut self, node: &Node, frame: Frame, cursor: &mut Cursor, out: &mut Vec<Block>) {
        match node {
            Node::Leaf(items) => self.emit_leaf(items, frame, cursor, out),
            Node::Rows(children) => {
                for c in children {
                    self.emit(c, frame, cursor, out);
                }
            }
            Node::Cols(children) => self.emit_cols(children, frame, cursor, out),
        }
    }

    fn emit_leaf(&mut self, items: &[usize], frame: Frame, cursor: &mut Cursor, out: &mut Vec<Block>) {
        let it = self.ctx.items;
        let seg_ids: Vec<usize> = items
            .iter()
            .filter_map(|&i| match it[i].kind {
                ItemKind::Seg(s) => Some(s),
                _ => None,
            })
            .collect();
        let text_bbox = Rect::union_all(seg_ids.iter().map(|&s| &self.ctx.segs[s].bbox));
        enum Unit {
            Para(Paragraph),
            Other(usize),
        }
        let mut units: Vec<(f32, Unit)> = Vec::new();
        for p in self.paragraphs(&seg_ids, frame) {
            units.push((p.bbox.y0, Unit::Para(p)));
        }
        for &i in items {
            if !matches!(it[i].kind, ItemKind::Seg(_)) {
                units.push((it[i].bbox.y0, Unit::Other(i)));
            }
        }
        units.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, u) in units {
            match u {
                Unit::Para(p) => self.place_paragraph(p, cursor, out),
                Unit::Other(i) => {
                    let item = it[i];
                    match item.kind {
                        ItemKind::Image(im) => {
                            let img = &self.raw().images[im];
                            // Text beside the picture → float it with square wrapping.
                            let beside_text = text_bbox.is_some_and(|tb| {
                                tb.v_overlap(&img.bbox) > 0.3 * img.bbox.height().min(tb.height())
                                    && tb.h_overlap(&img.bbox) < 0.5 * img.bbox.width()
                            });
                            let pic = self.picture(im, frame, cursor);
                            if beside_text {
                                let mut pic = pic;
                                pic.behind_text = false;
                                self.floating.push(pic);
                            } else {
                                cursor.y = cursor.y.max(img.bbox.y0) + img.bbox.height();
                                out.push(Block::Picture(pic));
                            }
                        }
                        ItemKind::Table(t) => {
                            let table = self.ruled_table(t, frame, cursor);
                            cursor.y = cursor.y.max(table.bbox.y1);
                            out.push(Block::Table(table));
                        }
                        ItemKind::Rule(r) => {
                            let rule = &self.raw().rules[r];
                            let b = rule.bbox;
                            let rb = RuleBlock {
                                bbox: b,
                                color: rule.color,
                                width: rule.width,
                                indent_left: (b.x0 - frame.x0).max(0.0),
                                indent_right: (frame.x1 - b.x1).max(0.0),
                                space_before: (b.y0 - 1.0 - cursor.y).max(0.0),
                            };
                            cursor.y = cursor.y.max(b.y0 - 1.0) + 1.0 + rule.width;
                            out.push(Block::Rule(rb));
                        }
                        ItemKind::Seg(_) => unreachable!(),
                    }
                }
            }
        }
    }

    fn picture(&self, im: usize, frame: Frame, cursor: &Cursor) -> Picture {
        let img = &self.raw().images[im];
        let b = img.bbox;
        let lm = b.x0 - frame.x0;
        let rm = frame.x1 - b.x1;
        let align = if lm > 10.0 && (lm - rm).abs() < 0.04 * frame.width() + 2.0 { Align::Center } else { Align::Left };
        Picture {
            data: img.data.clone(),
            width: b.width(),
            height: b.height(),
            bbox: b,
            indent: if align == Align::Left { lm.max(0.0) } else { 0.0 },
            space_before: (b.y0 - cursor.y).max(0.0),
            align,
            behind_text: img.behind_text,
        }
    }

    fn ruled_table(&mut self, t: usize, frame: Frame, cursor: &Cursor) -> Table {
        let rt = &self.tables[t];
        let it = self.ctx.items;
        let segs = self.ctx.segs;
        // Segments inside the table are stored on the table item.
        let inside: Vec<usize> = (0..segs.len())
            .filter(|&s| rt.bbox.expand(1.0).contains_point(segs[s].bbox.cx(), segs[s].bbox.cy()))
            .filter(|&s| !it.iter().any(|i| i.kind == ItemKind::Seg(s)))
            .collect();
        let nr = rt.ys.len() - 1;
        let col_widths: Vec<f32> = rt.xs.windows(2).map(|w| w[1] - w[0]).collect();
        let mut rows: Vec<Row> = (0..nr)
            .map(|r| Row { cells: Vec::new(), height: rt.ys[r + 1] - rt.ys[r] })
            .collect();
        // Map (row, col) → cell for continuation rows of vertical merges.
        for r in 0..nr {
            let mut c = 0;
            while c < rt.xs.len() - 1 {
                let owner = rt
                    .cells
                    .iter()
                    .find(|gc| gc.col == c && gc.row <= r && r < gc.row + gc.row_span);
                let Some(gc) = owner else {
                    rows[r].cells.push(Cell { col_span: 1, ..Default::default() });
                    c += 1;
                    continue;
                };
                let mut cell = Cell {
                    col_span: gc.col_span,
                    v_merge_start: gc.row_span > 1 && gc.row == r,
                    v_merge_continue: gc.row_span > 1 && gc.row != r,
                    shading: gc.shading,
                    border_top: gc.borders[0].clone(),
                    border_bottom: gc.borders[1].clone(),
                    border_left: gc.borders[2].clone(),
                    border_right: gc.borders[3].clone(),
                    blocks: Vec::new(),
                };
                if gc.row == r {
                    let cell_segs: Vec<usize> = inside
                        .iter()
                        .copied()
                        .filter(|&s| gc.rect.contains_point(segs[s].bbox.cx(), segs[s].bbox.cy()))
                        .collect();
                    let cframe = Frame { x0: gc.rect.x0, x1: gc.rect.x1 };
                    let mut cur = Cursor { y: gc.rect.y0 };
                    for p in self.paragraphs(&cell_segs, cframe) {
                        self.place_paragraph(p, &mut cur, &mut cell.blocks);
                    }
                    // Images inside the cell.
                    for (ii, img) in self.raw().images.iter().enumerate() {
                        if gc.rect.contains_point(img.bbox.cx(), img.bbox.cy()) {
                            let pic = self.picture(ii, cframe, &cur);
                            cur.y = img.bbox.y1;
                            cell.blocks.push(Block::Picture(pic));
                        }
                    }
                }
                c += gc.col_span.max(1);
                rows[r].cells.push(cell);
            }
        }
        let mut table = Table {
            col_widths,
            rows,
            indent: rt.bbox.x0 - frame.x0,
            space_before: (rt.bbox.y0 - cursor.y).max(0.0),
            bbox: rt.bbox,
            layout_only: false,
        };
        normalize_column_alignment(&mut table);
        table
    }

    fn emit_cols(&mut self, children: &[Node], frame: Frame, cursor: &mut Cursor, out: &mut Vec<Block>) {
        let it = self.ctx.items;
        let child_items: Vec<Vec<usize>> = children.iter().map(|c| c.all_items()).collect();
        let child_boxes: Vec<Rect> = child_items.iter().map(|v| self.ctx.bbox(v)).collect();
        let all_text = child_items.iter().flatten().all(|&i| matches!(it[i].kind, ItemKind::Seg(_)));
        let child_lines: Vec<usize> = child_items.iter().map(|v| self.ctx.line_count(v)).collect();
        let bbox = Rect::union_all(child_boxes.iter()).unwrap();

        // 1. Every column is a single line → one paragraph with tab stops.
        if all_text && child_lines.iter().all(|&n| n <= 1) {
            let seg_ids: Vec<usize> = child_items
                .iter()
                .flatten()
                .filter_map(|&i| match it[i].kind {
                    ItemKind::Seg(s) => Some(s),
                    _ => None,
                })
                .collect();
            let lines = self.make_lines(&seg_ids);
            if lines.len() == 1 {
                let p = self.make_paragraph(&[&lines[0]], frame);
                self.place_paragraph(p, cursor, out);
                return;
            }
        }

        // Column boundaries at gutter midpoints.
        let mut bounds = vec![child_boxes[0].x0.max(frame.x0)];
        for w in child_boxes.windows(2) {
            bounds.push((w[0].x1 + w[1].x0) / 2.0);
        }
        bounds.push(child_boxes.last().unwrap().x1.min(frame.x1).max(child_boxes.last().unwrap().x1));

        // 2. Whitespace-aligned data table.
        if self.detect_tables && all_text {
            if let Some(table) = self.try_text_table(&child_items, &bounds, frame, cursor) {
                cursor.y = cursor.y.max(table.bbox.y1);
                out.push(Block::Table(table));
                return;
            }
        }

        // 3. Multi-column layout → borderless one-row table.
        let mut row = Row { cells: Vec::new(), height: 0.0 };
        for (k, child) in children.iter().enumerate() {
            let cframe = Frame { x0: bounds[k], x1: bounds[k + 1] };
            let mut cur = Cursor { y: bbox.y0 };
            let mut blocks = Vec::new();
            self.emit(child, cframe, &mut cur, &mut blocks);
            row.cells.push(Cell { blocks, col_span: 1, ..Default::default() });
        }
        let table = Table {
            col_widths: bounds.windows(2).map(|w| w[1] - w[0]).collect(),
            rows: vec![row],
            indent: bounds[0] - frame.x0,
            space_before: (bbox.y0 - cursor.y).max(0.0),
            bbox,
            layout_only: true,
        };
        cursor.y = cursor.y.max(bbox.y1);
        out.push(Block::Table(table));
    }

    fn try_text_table(&mut self, child_items: &[Vec<usize>], bounds: &[f32], frame: Frame, cursor: &Cursor) -> Option<Table> {
        let it = self.ctx.items;
        let ncols = child_items.len();
        let col_lines: Vec<Vec<Line>> = child_items
            .iter()
            .map(|v| {
                let segs: Vec<usize> = v
                    .iter()
                    .filter_map(|&i| match it[i].kind {
                        ItemKind::Seg(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                self.make_lines(&segs)
            })
            .collect();
        // Prose columns fill their width; table columns usually don't.
        let mut fills = Vec::new();
        for (k, lines) in col_lines.iter().enumerate() {
            let w = (bounds[k + 1] - bounds[k]).max(1.0);
            let cw = Rect::union_all(lines.iter().map(|l| &l.bbox)).map(|r| r.width()).unwrap_or(w).max(1.0);
            let n = lines.len();
            if n >= 3 {
                let f: f32 = lines[..n - 1].iter().map(|l| l.bbox.width() / cw).sum::<f32>() / (n - 1) as f32;
                fills.push(f);
            }
            let _ = w;
        }
        let avg_fill = if fills.is_empty() { 0.0 } else { fills.iter().sum::<f32>() / fills.len() as f32 };
        let avg_chars: f32 = {
            let raw = self.raw();
            let segs = self.ctx.segs;
            let (mut tot, mut n) = (0usize, 0usize);
            for l in col_lines.iter().flatten() {
                tot += l.segs.iter().map(|&s| segs[s].glyphs.len()).sum::<usize>();
                n += 1;
            }
            let _ = raw;
            tot as f32 / n.max(1) as f32
        };
        if avg_fill > 0.82 && avg_chars > 25.0 {
            return None;
        }

        // Row bands across columns.
        let mut all: Vec<(usize, usize)> = Vec::new(); // (col, line)
        for (c, ls) in col_lines.iter().enumerate() {
            for l in 0..ls.len() {
                all.push((c, l));
            }
        }
        all.sort_by(|a, b| col_lines[a.0][a.1].bbox.y0.total_cmp(&col_lines[b.0][b.1].bbox.y0));
        struct Band {
            rect: Rect,
            members: Vec<(usize, usize)>,
        }
        let mut bands: Vec<Band> = Vec::new();
        for (c, l) in all {
            let r = col_lines[c][l].bbox;
            if let Some(b) = bands.last_mut() {
                if r.v_overlap(&b.rect) > 0.3 * r.height().min(b.rect.height()) {
                    b.rect = b.rect.union(&r);
                    b.members.push((c, l));
                    continue;
                }
            }
            bands.push(Band { rect: r, members: vec![(c, l)] });
        }
        if bands.len() < 2 {
            return None;
        }
        let multi = bands
            .iter()
            .filter(|b| {
                let mut cols: Vec<usize> = b.members.iter().map(|m| m.0).collect();
                cols.dedup();
                cols.len() >= 2
            })
            .count();
        if (multi as f32) < 0.6 * bands.len() as f32 && !(ncols >= 3 && multi as f32 >= 0.4 * bands.len() as f32) {
            return None;
        }
        // Merge continuation bands (wrapped cell text) into the band above.
        let mut merged: Vec<Band> = Vec::new();
        for b in bands {
            if let Some(prev) = merged.last_mut() {
                let gap = b.rect.y0 - prev.rect.y1;
                let size = col_lines[b.members[0].0][b.members[0].1].size;
                let prev_cols: Vec<usize> = prev.members.iter().map(|m| m.0).collect();
                let b_cols: Vec<usize> = b.members.iter().map(|m| m.0).collect();
                let continuation = b_cols.len() < prev_cols.len()
                    && !b_cols.contains(&0)
                    && gap < 0.4 * size
                    && b_cols.iter().all(|c| prev_cols.contains(c));
                if continuation {
                    prev.rect = prev.rect.union(&b.rect);
                    prev.members.extend(b.members);
                    continue;
                }
            }
            merged.push(b);
        }

        let mut rows = Vec::new();
        let tbbox = Rect::union_all(merged.iter().map(|b| &b.rect)).unwrap();
        for (bi, band) in merged.iter().enumerate() {
            let top = if bi == 0 { band.rect.y0 } else { (merged[bi - 1].rect.y1 + band.rect.y0) / 2.0 };
            let bottom = if bi + 1 < merged.len() { (band.rect.y1 + merged[bi + 1].rect.y0) / 2.0 } else { band.rect.y1 };
            let mut row = Row { cells: Vec::new(), height: bottom - top };
            for c in 0..ncols {
                let cframe = Frame { x0: bounds[c], x1: bounds[c + 1] };
                let mut lines: Vec<&Line> = band.members.iter().filter(|m| m.0 == c).map(|m| &col_lines[c][m.1]).collect();
                lines.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
                let mut blocks = Vec::new();
                let mut cur = Cursor { y: top };
                if !lines.is_empty() {
                    let owned: Vec<Line> = lines.iter().map(|l| (*l).clone()).collect();
                    let groups = self.group_paragraphs(&owned, cframe);
                    for g in groups {
                        let ls: Vec<&Line> = g.iter().map(|&i| &owned[i]).collect();
                        let p = self.make_paragraph(&ls, cframe);
                        self.place_paragraph(p, &mut cur, &mut blocks);
                    }
                }
                row.cells.push(Cell { blocks, col_span: 1, ..Default::default() });
            }
            rows.push(row);
        }
        let mut table = Table {
            col_widths: bounds.windows(2).map(|w| w[1] - w[0]).collect(),
            rows,
            indent: bounds[0] - frame.x0,
            space_before: (tbbox.y0 - cursor.y).max(0.0),
            bbox: Rect::new(bounds[0], tbbox.y0, *bounds.last().unwrap(), tbbox.y1),
            layout_only: false,
        };
        normalize_column_alignment(&mut table);
        Some(table)
    }
}

/// A small PNG filled with one colour (page backgrounds).
fn solid_png(c: Rgb) -> Option<ImageData> {
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([c.0, c.1, c.2]));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(ImageData { bytes: buf.into_inner(), ext: "png", px_width: 8, px_height: 8 })
}

fn min_x0(lines: &[&Line]) -> f32 {
    lines.iter().map(|l| l.bbox.x0).fold(f32::MAX, f32::min)
}

fn strip_trailing_space(inlines: &mut [Inline]) {
    if let Some(Inline::Text { text, .. }) = inlines.iter_mut().rev().find(|i| matches!(i, Inline::Text { .. })) {
        let t = text.trim_end().to_string();
        *text = t;
    }
}

/// Makes the cells of each table column share one alignment (numbers right, text left…).
fn normalize_column_alignment(table: &mut Table) {
    let ncols = table.col_widths.len();
    let mut col_x0 = vec![0.0f32; ncols + 1];
    for c in 0..ncols {
        col_x0[c + 1] = col_x0[c] + table.col_widths[c];
    }
    for c in 0..ncols {
        // Single-line paragraphs of cells that occupy exactly column c.
        let mut paras: Vec<(f32, f32, f32)> = Vec::new(); // (x0, x1, size)
        for row in &table.rows {
            let mut col = 0;
            for cell in &row.cells {
                if col == c && cell.col_span <= 1 {
                    for b in &cell.blocks {
                        if let Block::Paragraph(p) = b {
                            if p.n_lines == 1 {
                                paras.push((p.bbox.x0, p.bbox.x1, p.font_size));
                            }
                        }
                    }
                }
                col += cell.col_span.max(1);
            }
        }
        if paras.len() < 2 {
            continue;
        }
        let tol = paras.iter().map(|p| p.2).fold(0.0, f32::max) * 0.4 + 1.0;
        let min_x0 = paras.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        let max_x1 = paras.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        let left = paras.iter().filter(|p| (p.0 - min_x0).abs() <= tol).count();
        let right = paras.iter().filter(|p| (max_x1 - p.1).abs() <= tol).count();
        let align = if left >= right { Align::Left } else { Align::Right };
        for row in &mut table.rows {
            let mut col = 0;
            for cell in &mut row.cells {
                if col == c && cell.col_span <= 1 {
                    for b in &mut cell.blocks {
                        if let Block::Paragraph(p) = b {
                            if p.n_lines == 1 && matches!(p.align, Align::Left | Align::Center | Align::Right) {
                                p.align = align;
                                let cell_x0 = table.bbox.x0 + col_x0[c];
                                let cell_x1 = table.bbox.x0 + col_x0[c + 1];
                                if align == Align::Left {
                                    p.indent_left = (min_x0 - cell_x0).max(0.0);
                                    p.indent_right = 0.0;
                                } else {
                                    p.indent_left = 0.0;
                                    p.indent_right = (cell_x1 - max_x1).max(0.0);
                                }
                                p.indent_first = 0.0;
                            }
                        }
                    }
                }
                col += cell.col_span.max(1);
            }
        }
    }
}

fn right_guess(lines: &[Line]) -> f32 {
    let mut v: Vec<f32> = lines.iter().map(|l| l.bbox.x1).collect();
    v.sort_by(|a, b| b.total_cmp(a));
    // Use the 2nd largest right edge when available (robust to one overlong line).
    v.get(1).or(v.first()).copied().unwrap_or(0.0)
}

/// One element of a line before it is merged into runs.
#[derive(Clone, Debug)]
enum Atom {
    Char {
        text: String,
        style: TextStyle,
        link: Option<String>,
        /// Advance width of the glyph in the PDF.
        width: f32,
        #[allow(dead_code)]
        glyph: usize,
    },
    Space,
    Tab,
    Math(Vec<MathNode>),
}

/// Letter spacing (PDF `Tc`) is not part of a glyph's box: widen each glyph to
/// its pitch so runs are measured with their real, tracked width.
fn apply_letter_spacing(atoms: &mut [Atom], raw: &RawPage) {
    let x0 = |a: &Atom| match a {
        Atom::Char { glyph, .. } => Some(raw.glyphs[*glyph].bbox.x0),
        _ => None,
    };
    let mut extras = Vec::new();
    let mut per_atom = vec![None; atoms.len()];
    for i in 0..atoms.len().saturating_sub(1) {
        if let (Some(a), Some(b), Atom::Char { width, style, .. }) = (x0(&atoms[i]), x0(&atoms[i + 1]), &atoms[i]) {
            let extra = (b - a) - width;
            if extra.abs() < 0.6 * style.size {
                extras.push(extra);
                per_atom[i] = Some(extra);
            }
        }
    }
    let Some(typical) = median(&mut extras) else { return };
    for (i, a) in atoms.iter_mut().enumerate() {
        if let Atom::Char { width, style, .. } = a {
            let extra = per_atom[i].unwrap_or(typical).clamp(-0.15 * style.size, 0.6 * style.size);
            *width += extra;
        }
    }
}

/// Merges atoms into text runs (spaces take the style of the preceding character).
fn atoms_to_inlines(atoms: Vec<Atom>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut prev_style: Option<TextStyle> = None;
    let mut pending_space = false;
    let push = |out: &mut Vec<Inline>, text: &str, style: &TextStyle, link: Option<&String>, width: f32| {
        if let Some(Inline::Text { text: t, style: s, link: l, pdf_width }) = out.last_mut() {
            if s.same_run_format(style) && l.as_ref() == link {
                t.push_str(text);
                *pdf_width += width;
                return;
            }
        }
        out.push(Inline::Text { text: text.to_string(), style: style.clone(), link: link.cloned(), pdf_width: width });
    };
    for a in atoms {
        match a {
            Atom::Space => pending_space = !out.is_empty(),
            Atom::Tab => {
                pending_space = false;
                out.push(Inline::Tab);
            }
            Atom::Math(m) => {
                if pending_space {
                    if let Some(st) = &prev_style {
                        push(&mut out, " ", st, None, 0.0);
                    }
                    pending_space = false;
                }
                out.push(Inline::Math(m));
            }
            Atom::Char { text, style, link, width, .. } => {
                if pending_space {
                    let st = prev_style.clone().unwrap_or_else(|| style.clone());
                    push(&mut out, " ", &st, None, 0.0);
                    pending_space = false;
                }
                let w = if text.trim().is_empty() { 0.0 } else { width };
                push(&mut out, &text, &style, link.as_ref(), w);
                prev_style = Some(style);
            }
        }
    }
    out
}

fn expand_ligature(c: char) -> String {
    match c {
        '\u{FB00}' => "ff".into(),
        '\u{FB01}' => "fi".into(),
        '\u{FB02}' => "fl".into(),
        '\u{FB03}' => "ffi".into(),
        '\u{FB04}' => "ffl".into(),
        '\u{FB05}' | '\u{FB06}' => "st".into(),
        '\u{AD}' => "-".into(),
        c => c.to_string(),
    }
}

/// Decides how a line joins the next one in flow layout, stripping a
/// hyphenation hyphen from the end of `prev` if appropriate.
fn line_joiner(prev: &mut [Inline], next: &[Inline]) -> Joiner {
    let next_starts_lower = next.iter().find_map(|i| match i {
        Inline::Text { text, .. } => text.chars().next(),
        _ => None,
    });
    let last = prev.iter_mut().rev().find_map(|i| match i {
        Inline::Text { text, .. } => Some(text),
        _ => None,
    });
    let Some(text) = last else { return Joiner::Space };
    let trimmed = text.trim_end().to_string();
    *text = trimmed;
    let lower_next = next_starts_lower.is_some_and(|c| c.is_lowercase());
    if text.ends_with('\u{AD}') || (text.ends_with('-') && lower_next && text.chars().rev().nth(1).is_some_and(|c| c.is_alphabetic())) {
        // Hyphenation: remove the hyphen.
        if text.ends_with('\u{AD}') || lower_next {
            text.pop();
            return Joiner::Hyphen;
        }
    }
    if text.ends_with(['-', '–', '—', '/']) {
        return Joiner::Direct;
    }
    Joiner::Space
}

/// Analyses one page.
pub fn analyse_page(raw_in: &RawPage, opts: &LayoutOptions) -> PageLayout {
    let mut raw = raw_in.clone();
    apply_decorations(&mut raw);
    let segs = build_segments(&raw);

    let mut char_sizes: Vec<f32> = raw.glyphs.iter().map(|g| raw.styles[g.style].size).collect();
    let body_size = median(&mut char_sizes).unwrap_or(11.0);

    let (tables, consumed_rules) = if opts.detect_tables { detect_ruled_tables(&raw) } else { (vec![], vec![false; raw.rules.len()]) };

    // Background images (large, under a lot of text) float behind the text.
    let page_area = raw.width * raw.height;
    let mut floating: Vec<Picture> = Vec::new();
    let mut image_items: Vec<usize> = Vec::new();
    for (i, img) in raw.images.iter().enumerate() {
        let text_inside = raw.glyphs.iter().filter(|g| img.bbox.contains_point(g.bbox.cx(), g.bbox.cy())).count();
        if img.behind_text || (img.bbox.area() > 0.15 * page_area && text_inside > 20) {
            floating.push(Picture {
                data: img.data.clone(),
                width: img.bbox.width(),
                height: img.bbox.height(),
                bbox: img.bbox,
                indent: 0.0,
                space_before: 0.0,
                align: Align::Left,
                behind_text: true,
            });
        } else {
            image_items.push(i);
        }
    }

    if let Some(bg) = raw.background {
        if let Some(data) = solid_png(bg) {
            floating.insert(
                0,
                Picture {
                    data,
                    width: raw.width,
                    height: raw.height,
                    bbox: Rect::new(0.0, 0.0, raw.width, raw.height),
                    indent: 0.0,
                    space_before: 0.0,
                    align: Align::Left,
                    behind_text: true,
                },
            );
        }
    }

    let mut items: Vec<Item> = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        if tables.iter().any(|t| t.bbox.expand(1.0).contains_point(s.bbox.cx(), s.bbox.cy())) {
            continue;
        }
        items.push(Item { kind: ItemKind::Seg(i), bbox: s.bbox });
    }
    for &i in &image_items {
        let b = raw.images[i].bbox;
        if tables.iter().any(|t| t.bbox.contains_point(b.cx(), b.cy())) {
            continue;
        }
        items.push(Item { kind: ItemKind::Image(i), bbox: b });
    }
    for (i, t) in tables.iter().enumerate() {
        items.push(Item { kind: ItemKind::Table(i), bbox: t.bbox });
    }
    for (i, r) in raw.rules.iter().enumerate() {
        if consumed_rules[i] || r.orientation != RuleOrientation::Horizontal || r.bbox.width() < 24.0 {
            continue;
        }
        // Rules that are part of a whitespace table are handled there; long free rules are separators.
        items.push(Item { kind: ItemKind::Rule(i), bbox: r.bbox });
    }

    let mut layout = PageLayout {
        index: raw.index,
        width: raw.width,
        height: raw.height,
        ocr: raw.ocr,
        ..Default::default()
    };
    if items.is_empty() {
        layout.floating = floating;
        layout.margin_left = 72.0f32.min(raw.width / 4.0);
        layout.margin_right = layout.margin_left;
        layout.margin_top = 72.0f32.min(raw.height / 4.0);
        layout.margin_bottom = layout.margin_top;
        return layout;
    }

    let content = Rect::union_all(items.iter().map(|i| &i.bbox)).unwrap();
    let margin_left = content.x0.clamp(0.0, raw.width / 2.0 - 36.0).max(0.0).floor();
    let margin_right = (raw.width - content.x1).clamp(0.0, raw.width / 2.0 - 36.0).max(0.0).floor();
    let frame = Frame { x0: margin_left, x1: raw.width - margin_right };

    let ctx = Ctx { raw: &raw, segs: &segs, items: &items, body_size };
    let root = ctx.cut((0..items.len()).collect(), 0);
    let mut em = Emitter { ctx, tables: &tables, floating: Vec::new(), detect_tables: opts.detect_tables };
    let mut cursor = Cursor { y: 0.0 };
    let mut blocks = Vec::new();
    em.emit(&root, frame, &mut cursor, &mut blocks);
    floating.extend(em.floating);

    // The first block's leading space becomes the top margin.
    let top = match blocks.first_mut() {
        Some(Block::Paragraph(p)) => std::mem::take(&mut p.space_before),
        Some(Block::Picture(p)) => std::mem::take(&mut p.space_before),
        Some(Block::Table(t)) => std::mem::take(&mut t.space_before),
        Some(Block::Rule(r)) => std::mem::take(&mut r.space_before),
        None => 72.0,
    };
    layout.margin_top = top.min(raw.height / 2.0);
    layout.margin_bottom = (raw.height - content.y1 - 4.0).clamp(0.0, 24.0);
    layout.margin_left = margin_left;
    layout.margin_right = margin_right;
    layout.blocks = blocks;
    layout.floating = floating;
    layout
}

/// Assigns heading levels across the whole document based on font size.
pub fn detect_headings(pages: &mut [PageLayout]) {
    // Body size = most common size weighted by characters.
    let mut hist: HashMap<i32, usize> = HashMap::new();
    fn walk<'a>(blocks: &'a [Block], f: &mut dyn FnMut(&'a Paragraph)) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => f(p),
                Block::Table(t) if t.layout_only => {
                    for r in &t.rows {
                        for c in &r.cells {
                            walk(&c.blocks, f);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for page in pages.iter() {
        walk(&page.blocks, &mut |p| {
            for i in &p.inlines {
                if let Inline::Text { text, style, .. } = i {
                    *hist.entry((style.size * 2.0).round() as i32).or_default() += text.chars().count();
                }
            }
        });
    }
    let Some((&body2, _)) = hist.iter().max_by_key(|(_, n)| **n) else { return };
    let body = body2 as f32 / 2.0;

    let mut heading_sizes: Vec<i32> = Vec::new();
    let is_candidate = |p: &Paragraph| -> bool {
        let text = p.plain_text();
        let len = text.trim().chars().count();
        p.font_size >= body * 1.18 && p.n_lines <= 3 && len > 0 && len < 160 && text.chars().any(|c| c.is_alphabetic())
    };
    for page in pages.iter() {
        walk(&page.blocks, &mut |p| {
            if is_candidate(p) {
                heading_sizes.push((p.font_size * 2.0).round() as i32);
            }
        });
    }
    heading_sizes.sort_unstable_by(|a, b| b.cmp(a));
    heading_sizes.dedup();
    fn walk_mut(blocks: &mut [Block], f: &mut dyn FnMut(&mut Paragraph)) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => f(p),
                Block::Table(t) if t.layout_only => {
                    for r in &mut t.rows {
                        for c in &mut r.cells {
                            walk_mut(&mut c.blocks, f);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for page in pages.iter_mut() {
        walk_mut(&mut page.blocks, &mut |p| {
            if is_candidate(p) {
                let key = (p.font_size * 2.0).round() as i32;
                if let Some(pos) = heading_sizes.iter().position(|&s| s == key) {
                    p.heading = Some((pos as u8 + 1).min(3));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_with_text(lines: &[(&str, f32, f32, f32)]) -> RawPage {
        // (text, x, baseline, size)
        let mut raw = RawPage { width: 612.0, height: 792.0, ..Default::default() };
        let style = raw.style_id(TextStyle::default());
        for &(text, x, base, size) in lines {
            let mut cx = x;
            let mut space = false;
            for ch in text.chars() {
                let w = size * 0.5;
                if ch == ' ' {
                    cx += w * 0.5;
                    space = true;
                    continue;
                }
                raw.glyphs.push(Glyph {
                    ch,
                    bbox: Rect::new(cx, base - 0.8 * size, cx + w, base + 0.2 * size),
                    baseline: base,
                    style,
                    space_before: space,
                    link: None,
                });
                space = false;
                cx += w;
            }
        }
        let size = 11.0;
        raw.styles[style].size = size;
        raw
    }

    fn texts(blocks: &[Block]) -> Vec<String> {
        let mut v = Vec::new();
        for b in blocks {
            match b {
                Block::Paragraph(p) => v.push(p.plain_text()),
                Block::Table(t) => {
                    for r in &t.rows {
                        for c in &r.cells {
                            v.extend(texts(&c.blocks));
                        }
                    }
                }
                _ => {}
            }
        }
        v
    }

    #[test]
    fn list_markers() {
        assert!(is_list_marker("•"));
        assert!(is_list_marker("1."));
        assert!(is_list_marker("(iv)"));
        assert!(is_list_marker("a)"));
        assert!(!is_list_marker("Hello"));
        assert!(!is_list_marker("12"));
    }

    #[test]
    fn paragraphs_and_spacing() {
        let raw = page_with_text(&[
            ("The quick brown fox jumps over the lazy dog and keeps", 72.0, 100.0, 11.0),
            ("running across the field until the end of the line.", 72.0, 113.2, 11.0),
            ("Short last line.", 72.0, 126.4, 11.0),
            ("A new paragraph starts after a blank gap in the text", 72.0, 152.0, 11.0),
            ("and continues on the following line as expected.", 72.0, 165.2, 11.0),
        ]);
        let page = analyse_page(&raw, &LayoutOptions::default());
        let t = texts(&page.blocks);
        assert_eq!(t.len(), 2, "{t:?}");
        assert!(t[0].starts_with("The quick brown fox jumps over the lazy dog and keeps running"));
        assert!(t[1].starts_with("A new paragraph"));
        let Block::Paragraph(p2) = &page.blocks[1] else { panic!() };
        assert!(p2.space_before > 5.0, "space_before {}", p2.space_before);
    }

    #[test]
    fn two_columns() {
        let mut lines = Vec::new();
        let left = [
            "Left column prose that fills the column",
            "with ordinary sentences spanning the full",
            "width of the column until it finally ends",
            "here.",
        ];
        let right = [
            "Right column prose continues the article",
            "and also fills its column completely with",
            "words that wrap from one line to another",
            "end.",
        ];
        for (k, (l, r)) in left.iter().zip(right.iter()).enumerate() {
            let y = 100.0 + k as f32 * 13.2;
            lines.push((*l, 72.0, y, 11.0));
            lines.push((*r, 320.0, y, 11.0));
        }
        let raw = page_with_text(&lines);
        let page = analyse_page(&raw, &LayoutOptions::default());
        assert_eq!(page.blocks.len(), 1);
        let Block::Table(t) = &page.blocks[0] else { panic!("expected column table") };
        assert_eq!(t.col_widths.len(), 2);
        let all = texts(&page.blocks);
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all[0].starts_with("Left column prose that fills the column with ordinary"), "{all:?}");
        assert!(all[1].starts_with("Right column prose"), "{all:?}");
    }

    #[test]
    fn tab_row() {
        let raw = page_with_text(&[("Invoice", 72.0, 100.0, 11.0), ("Page 1", 500.0, 100.0, 11.0)]);
        let page = analyse_page(&raw, &LayoutOptions::default());
        assert_eq!(page.blocks.len(), 1);
        let Block::Paragraph(p) = &page.blocks[0] else { panic!() };
        assert_eq!(p.plain_text(), "Invoice\tPage 1");
        assert_eq!(p.tabs.len(), 1);
    }

    #[test]
    fn hyphen_join() {
        let st = TextStyle::default();
        let mut prev = vec![Inline::Text { text: "conver-".into(), style: st.clone(), link: None, pdf_width: 0.0 }];
        let next = vec![Inline::Text { text: "sion".into(), style: st.clone(), link: None, pdf_width: 0.0 }];
        assert_eq!(line_joiner(&mut prev, &next), Joiner::Hyphen);
        let Inline::Text { text, .. } = &prev[0] else { panic!() };
        assert_eq!(text, "conver");
        let mut prev = vec![Inline::Text { text: "end.".into(), style: st.clone(), link: None, pdf_width: 0.0 }];
        assert_eq!(line_joiner(&mut prev, &next), Joiner::Space);
    }
}
