//! Reconstruction of equations as Office Math (OMML) trees.
//!
//! Mathematical text is recognised by its fonts (Computer Modern math fonts,
//! Cambria Math, STIX …) and symbols. Display equations — formulas on lines
//! of their own — are cut out of the page together with their fraction and
//! radical bars and rebuilt from geometry:
//!
//! 1. radicals: a `√` glyph whose top-right corner starts a horizontal bar;
//!    everything under the bar is the radicand,
//! 2. fractions: remaining bars with material above (numerator) and below
//!    (denominator), innermost first,
//! 3. big operators (∑ ∏ ∫ …) with limits stacked above/below,
//! 4. super- and subscripts from baseline offsets and sizes.
//!
//! Inline maths inside text lines uses the same linear parser.

use crate::model::*;

/// Fonts that only ever contain mathematics.
pub fn is_math_font(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let n = n.split('+').next_back().unwrap_or(&n);
    const PREFIXES: &[&str] = &["cmmi", "cmsy", "cmex", "cmbsy", "msam", "msbm", "eufm", "eusm", "rsfs", "lmmathitalic", "lmmathsymbols", "lmmathextension", "stmary", "wasy"];
    PREFIXES.iter().any(|p| n.starts_with(p)) || n.contains("math") && !n.contains("mathematica")
}

/// The TeX extension font (big operators and delimiters) encodes its glyphs at
/// ASCII positions; PDFium then reports letters. Map them back.
pub fn cmex_char(c: char) -> char {
    match c {
        'P' | 'X' => '∑',
        'Q' | 'Y' => '∏',
        '`' | 'a' => '∐',
        'R' | 'Z' => '∫',
        'H' | 'I' => '∮',
        'S' | '[' => '⋃',
        'T' | '\\' => '⋂',
        'V' | '^' => '⋁',
        'W' | '_' => '⋀',
        'L' | 'M' => '⊕',
        'N' | 'O' => '⊗',
        'p' | 'q' | 'r' | 's' | 't' | 'v' => '√',
        '\u{0}' | '\u{10}' | '\u{12}' | '!' | '0' | '@' => '(',
        '\u{1}' | '\u{11}' | '\u{13}' | '"' | '1' | 'A' => ')',
        '\u{2}' | '\u{14}' | 'h' | '2' => '[',
        '\u{3}' | '\u{15}' | '#' | 'i' | '3' => ']',
        '\u{8}' | '\u{1a}' | '(' | 'n' | '8' | ':' | '<' | '>' => '{',
        '\u{9}' | '\u{1b}' | ')' | 'o' | '9' | ';' | '=' | '?' => '}',
        '\u{c}' => '|',
        c => c,
    }
}

fn is_math_symbol(c: char) -> bool {
    matches!(c as u32,
        0x0391..=0x03A9 | 0x03B1..=0x03C9 | 0x03D1 | 0x03D5 | 0x03F5 | 0x2190..=0x21FF | 0x2200..=0x22FF | 0x2A00..=0x2AFF | 0x1D400..=0x1D7FF | 0x00B1 | 0x00D7 | 0x00F7)
}

fn is_big_operator(c: char) -> bool {
    matches!(c, '∑' | '∏' | '∐' | '∫' | '∬' | '∭' | '∮' | '⋃' | '⋂' | '⋁' | '⋀' | '⨁' | '⨂' | '⊕' | '⊗')
}

fn is_relation(c: char) -> bool {
    matches!(c, '=' | '<' | '>' | '≤' | '≥' | '≈' | '≠' | '≡' | '∼' | '≃' | '→' | '⇒' | '⇔' | '∈' | '⊂' | '⊆')
}

/// A positioned piece of an equation.
#[derive(Clone, Debug)]
pub struct MAtom {
    pub text: String,
    pub bbox: Rect,
    pub baseline: f32,
    pub size: f32,
    pub upright: bool,
    pub bold: bool,
    /// Built structure (fraction, radical, operator); always on the main line.
    pub node: Option<MathNode>,
}

impl MAtom {
    fn ch(&self) -> char {
        self.text.chars().next().unwrap_or(' ')
    }
}

/// Builds an equation tree from atoms and horizontal bars (fraction / radical rules).
pub fn parse(mut atoms: Vec<MAtom>, bars: &[Rect]) -> Vec<MathNode> {
    let mut bars: Vec<Rect> = bars.to_vec();
    // Innermost structures first.
    bars.sort_by(|a, b| a.width().total_cmp(&b.width()));

    // 1. Radicals.
    let mut used_bar = vec![false; bars.len()];
    loop {
        let found = atoms.iter().enumerate().find_map(|(i, a)| {
            if a.ch() != '√' || a.node.is_some() {
                return None;
            }
            bars.iter().enumerate().find(|(k, b)| {
                !used_bar[*k] && (b.x0 - a.bbox.x1).abs() < 0.25 * a.size + 1.0 && b.cy() >= a.bbox.y0 - 1.0 && b.cy() <= a.bbox.cy()
            }).map(|(k, _)| (i, k))
        });
        let Some((ri, k)) = found else { break };
        used_bar[k] = true;
        let bar = bars[k];
        let rad = atoms[ri].clone();
        let mut body = Vec::new();
        let mut degree = Vec::new();
        let mut rest = Vec::new();
        for (i, a) in atoms.drain(..).enumerate() {
            if i == ri {
                continue;
            }
            if a.bbox.cx() > bar.x0 && a.bbox.cx() < bar.x1 + 0.5 && a.bbox.cy() > bar.cy() && a.bbox.y0 < rad.bbox.y1 {
                body.push(a);
            } else if a.size < 0.85 * rad.size && a.bbox.x1 <= rad.bbox.cx() + 1.0 && a.bbox.x0 >= rad.bbox.x0 - 0.6 * rad.size && a.bbox.cy() < rad.bbox.cy() {
                degree.push(a);
            } else {
                rest.push(a);
            }
        }
        let inner_bars: Vec<Rect> = bars.iter().enumerate().filter(|(j, b)| !used_bar[*j] && b.x0 >= bar.x0 && b.x1 <= bar.x1 + 1.0 && b.cy() > bar.cy()).map(|(_, b)| *b).collect();
        let bbox = body.iter().fold(rad.bbox.union(&bar), |r, a| r.union(&a.bbox));
        let baseline = main_baseline(&body).unwrap_or(rad.baseline);
        let node = MathNode::Radical {
            degree: (!degree.is_empty()).then(|| parse(degree, &[])),
            body: parse(body, &inner_bars),
        };
        rest.push(MAtom { text: String::new(), bbox, baseline, size: rad.size, upright: true, bold: false, node: Some(node) });
        atoms = rest;
        for (j, b) in bars.iter().enumerate() {
            if inner_bars.contains(b) {
                used_bar[j] = true;
            }
        }
    }

    // 2. Fractions.
    for k in 0..bars.len() {
        if used_bar[k] {
            continue;
        }
        let bar = bars[k];
        let size = atoms.iter().map(|a| a.size).fold(0.0, f32::max).max(1.0);
        let within = |a: &MAtom| a.bbox.cx() >= bar.x0 - 1.0 && a.bbox.cx() <= bar.x1 + 1.0;
        let (mut num, mut den, mut rest) = (Vec::new(), Vec::new(), Vec::new());
        for a in atoms.drain(..) {
            if within(&a) && a.bbox.cy() < bar.cy() && bar.cy() - a.bbox.y1 < 2.5 * size {
                num.push(a);
            } else if within(&a) && a.bbox.cy() > bar.cy() && a.bbox.y0 - bar.cy() < 2.5 * size {
                den.push(a);
            } else {
                rest.push(a);
            }
        }
        if num.is_empty() || den.is_empty() {
            rest.extend(num);
            rest.extend(den);
            atoms = rest;
            continue;
        }
        used_bar[k] = true;
        let bbox = num.iter().chain(den.iter()).fold(bar, |r, a| r.union(&a.bbox));
        // The fraction bar sits on the maths axis, about a quarter em above the baseline.
        let baseline = bar.cy() + 0.25 * size;
        let node = MathNode::Frac { num: parse(num, &[]), den: parse(den, &[]) };
        rest.push(MAtom { text: String::new(), bbox, baseline, size, upright: true, bold: false, node: Some(node) });
        atoms = rest;
    }

    // 3. Big operators with stacked limits.
    let ops: Vec<usize> = atoms.iter().enumerate().filter(|(_, a)| a.node.is_none() && is_big_operator(a.ch())).map(|(i, _)| i).collect();
    let mut replaced: Vec<(usize, MAtom)> = Vec::new();
    let mut consumed = vec![false; atoms.len()];
    for &oi in &ops {
        let op = &atoms[oi];
        let (mut over, mut under) = (Vec::new(), Vec::new());
        for (j, a) in atoms.iter().enumerate() {
            if j == oi || consumed[j] || a.size > 0.9 * op.size {
                continue;
            }
            let centred = a.bbox.cx() > op.bbox.x0 - 0.2 * op.size && a.bbox.cx() < op.bbox.x1 + 0.2 * op.size && a.bbox.x0 >= op.bbox.x0 - 0.8 * op.size;
            if centred && a.bbox.y1 <= op.bbox.y0 + 0.35 * op.size {
                over.push(j);
            } else if centred && a.bbox.y0 >= op.bbox.y1 - 0.35 * op.size {
                under.push(j);
            }
        }
        if over.is_empty() && under.is_empty() {
            continue;
        }
        for &j in over.iter().chain(under.iter()) {
            consumed[j] = true;
        }
        let take = |v: &[usize]| -> Option<Vec<MathNode>> { (!v.is_empty()).then(|| parse(v.iter().map(|&j| atoms[j].clone()).collect(), &[])) };
        let bbox = over.iter().chain(under.iter()).fold(op.bbox, |r, &j| r.union(&atoms[j].bbox));
        let node = MathNode::Nary { op: op.ch(), sub: take(&under), sup: take(&over), body: Vec::new() };
        replaced.push((oi, MAtom { text: String::new(), bbox, baseline: op.baseline, size: op.size, upright: true, bold: false, node: Some(node) }));
    }
    for (oi, a) in replaced {
        atoms[oi] = a;
    }
    let atoms: Vec<MAtom> = atoms.into_iter().enumerate().filter(|(j, _)| !consumed[*j]).map(|(_, a)| a).collect();

    // 4. Linear structure with scripts.
    attach_nary_bodies(parse_linear(atoms))
}

fn main_baseline(atoms: &[MAtom]) -> Option<f32> {
    let max = atoms.iter().filter(|a| a.node.is_none()).map(|a| a.size).fold(0.0, f32::max);
    let mut v: Vec<f32> = atoms.iter().filter(|a| a.node.is_none() && a.size >= 0.9 * max && !is_big_operator(a.ch())).map(|a| a.baseline).collect();
    if v.is_empty() {
        v = atoms.iter().map(|a| a.baseline).collect();
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(v.len() / 2).copied()
}

/// Left-to-right sequence with super/subscripts from baseline offsets.
pub fn parse_linear(mut atoms: Vec<MAtom>) -> Vec<MathNode> {
    if atoms.is_empty() {
        return Vec::new();
    }
    atoms.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
    let base = main_baseline(&atoms).unwrap_or(atoms[0].baseline);
    let base_size = atoms.iter().filter(|a| (a.baseline - base).abs() < 0.5).map(|a| a.size).fold(0.0, f32::max).max(atoms[0].size);
    let tol = 0.18 * base_size;

    enum Elem {
        Base(Vec<MAtom>),
        Struct(MathNode),
    }
    let mut elems: Vec<(Elem, Vec<MAtom>, Vec<MAtom>)> = Vec::new(); // (base, sub, sup)
    for a in atoms {
        // Scripts are smaller than the base text or clearly off its baseline
        // (inline subscripts in TeX are lowered by only ~0.15 em).
        let smaller = a.size < 0.8 * base_size;
        let on_line = a.node.is_some() || is_big_operator(a.ch()) || ((a.baseline - base).abs() <= tol && !smaller);
        if on_line || elems.is_empty() {
            let is_struct = a.node.is_some();
            // Consecutive plain characters form one base unless a script intervened.
            match elems.last_mut() {
                Some((Elem::Base(v), sub, sup)) if !is_struct && sub.is_empty() && sup.is_empty() && !is_big_operator(a.ch()) && !v.last().is_some_and(|l| is_big_operator(l.ch())) => v.push(a),
                _ => {
                    if let Some(node) = a.node.clone() {
                        elems.push((Elem::Struct(node), Vec::new(), Vec::new()));
                    } else {
                        elems.push((Elem::Base(vec![a]), Vec::new(), Vec::new()));
                    }
                }
            }
        } else {
            let last = elems.last_mut().unwrap();
            if a.baseline < base - 0.01 {
                last.2.push(a);
            } else {
                last.1.push(a);
            }
        }
    }
    let mut out = Vec::new();
    for (elem, sub, sup) in elems {
        let base_nodes = match elem {
            Elem::Struct(n) => vec![n],
            Elem::Base(v) => {
                if sub.is_empty() && sup.is_empty() {
                    runs(&v)
                } else {
                    // Scripts attach to the last character of the base.
                    let (head, last) = v.split_at(v.len() - 1);
                    out.extend(runs(head));
                    runs(last)
                }
            }
        };
        if sub.is_empty() && sup.is_empty() {
            out.extend(base_nodes);
            continue;
        }
        let sub_n = (!sub.is_empty()).then(|| parse_linear(sub));
        let sup_n = (!sup.is_empty()).then(|| parse_linear(sup));
        // An operator with side limits (∫ in display style) is an n-ary.
        if let [MathNode::Run { text, .. }] = base_nodes.as_slice() {
            if let Some(op) = text.chars().next().filter(|c| is_big_operator(*c) && text.chars().count() == 1) {
                out.push(MathNode::Nary { op, sub: sub_n, sup: sup_n, body: Vec::new() });
                continue;
            }
        }
        if let [MathNode::Nary { op, sub: None, sup: None, .. }] = base_nodes.as_slice() {
            out.push(MathNode::Nary { op: *op, sub: sub_n, sup: sup_n, body: Vec::new() });
            continue;
        }
        out.push(MathNode::Script { base: base_nodes, sub: sub_n, sup: sup_n });
    }
    merge_runs(out)
}

/// Turns atoms into runs (letters italic unless set upright).
fn runs(atoms: &[MAtom]) -> Vec<MathNode> {
    let mut out: Vec<MathNode> = Vec::new();
    for a in atoms {
        if let Some(n) = &a.node {
            out.push(n.clone());
            continue;
        }
        let upright = a.upright && a.text.chars().any(|c| c.is_alphabetic());
        out.push(MathNode::Run { text: a.text.clone(), upright, bold: a.bold, size: a.size });
    }
    merge_runs(out)
}

fn merge_runs(nodes: Vec<MathNode>) -> Vec<MathNode> {
    let mut out: Vec<MathNode> = Vec::new();
    for n in nodes {
        if let (Some(MathNode::Run { text: t, upright: u, bold: b, .. }), MathNode::Run { text, upright, bold, .. }) = (out.last_mut(), &n) {
            if u == upright && b == bold && !text.chars().any(is_big_operator) && !t.chars().any(is_big_operator) {
                t.push_str(text);
                continue;
            }
        }
        out.push(n);
    }
    out
}

/// The operand of an n-ary operator runs until the next relation symbol.
fn attach_nary_bodies(nodes: Vec<MathNode>) -> Vec<MathNode> {
    let mut out: Vec<MathNode> = Vec::new();
    let mut open: Option<usize> = None;
    for n in nodes {
        let is_rel = matches!(&n, MathNode::Run { text, .. } if text.trim_start().starts_with(is_relation));
        if is_rel {
            open = None;
        }
        if let Some(i) = open {
            if let MathNode::Nary { body, .. } = &mut out[i] {
                // A run that contains a relation is split at it.
                if let MathNode::Run { text, upright, bold, size } = &n {
                    if let Some(pos) = text.find(is_relation) {
                        let (a, b) = text.split_at(pos);
                        if !a.is_empty() {
                            body.push(MathNode::Run { text: a.to_string(), upright: *upright, bold: *bold, size: *size });
                        }
                        out.push(MathNode::Run { text: b.to_string(), upright: *upright, bold: *bold, size: *size });
                        open = None;
                        continue;
                    }
                }
                body.push(n);
                continue;
            }
        }
        let is_nary = matches!(n, MathNode::Nary { .. });
        out.push(n);
        if is_nary {
            open = Some(out.len() - 1);
        }
    }
    out
}

/// A display equation found on the page.
#[derive(Clone, Debug)]
pub struct DisplayEquation {
    pub bbox: Rect,
    pub nodes: Vec<MathNode>,
    pub baseline: f32,
    pub size: f32,
}

fn glyph_is_math(raw: &RawPage, g: &Glyph) -> bool {
    raw.styles[g.style].math || is_math_symbol(g.ch)
}

fn glyph_is_text_letter(raw: &RawPage, g: &Glyph) -> bool {
    g.ch.is_alphabetic() && !glyph_is_math(raw, g)
}

pub fn atom_of(raw: &RawPage, g: &Glyph) -> MAtom {
    let st = &raw.styles[g.style];
    MAtom {
        text: g.ch.to_string(),
        bbox: g.bbox,
        baseline: g.baseline,
        size: st.size,
        // Letters from a text font inside maths are upright (function names, units).
        upright: !st.math && !st.italic,
        bold: st.bold,
        node: None,
    }
}

/// Structures the geometric parser cannot express (matrices, case
/// distinctions): a tall delimiter enclosing several rows of base-size text
/// that are not explained by fraction bars.
fn is_complex(atoms: &[MAtom], bars: &[Rect]) -> bool {
    let base = atoms
        .iter()
        .filter(|a| !is_big_operator(a.ch()) && a.bbox.height() < 2.0 * a.size)
        .map(|a| a.size)
        .fold(0.0, f32::max);
    if base <= 0.0 {
        return false;
    }
    let opening = |c: char| matches!(c, '(' | '[' | '{' | '|' | '⟨');
    let closing = |c: char| matches!(c, ')' | ']' | '}' | '|' | '⟩');
    for d in atoms.iter().filter(|a| opening(a.ch()) && a.bbox.height() > 1.8 * base) {
        let end = atoms
            .iter()
            .filter(|a| closing(a.ch()) && a.bbox.height() > 1.8 * base && a.bbox.x0 > d.bbox.x1)
            .map(|a| a.bbox.x0)
            .fold(f32::MAX, f32::min);
        let mut bases: Vec<f32> = atoms
            .iter()
            .filter(|a| {
                a.bbox.x0 >= d.bbox.x1 - 0.5
                    && a.bbox.x1 <= end + 0.5
                    && a.size >= 0.9 * base
                    && a.bbox.cy() > d.bbox.y0
                    && a.bbox.cy() < d.bbox.y1
                    && !bars.iter().any(|b| a.bbox.cx() >= b.x0 && a.bbox.cx() <= b.x1)
            })
            .map(|a| a.baseline)
            .collect();
        bases.sort_by(|a, b| a.total_cmp(b));
        let rows = bases.windows(2).filter(|w| w[1] - w[0] > 0.6 * base).count() + 1;
        if !bases.is_empty() && rows >= 2 {
            return true;
        }
    }
    false
}

/// Finds display equations, removing their glyphs and bars from the page.
/// Also returns regions of equations too complex to rebuild (to be kept as pictures).
pub fn extract_display_equations(raw: &mut RawPage) -> (Vec<DisplayEquation>, Vec<Rect>) {
    let n = raw.glyphs.len();
    if n == 0 || !raw.glyphs.iter().any(|g| raw.styles[g.style].math) {
        return (Vec::new(), Vec::new());
    }
    // Horizontal runs of glyphs (word-ish groups) in stream order.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        let g = &raw.glyphs[i];
        let size = raw.styles[g.style].size;
        let joins = groups.last().is_some_and(|grp| {
            let p = &raw.glyphs[*grp.last().unwrap()];
            g.bbox.x0 - p.bbox.x1 < 0.5 * size && g.bbox.x0 > p.bbox.x0 - 0.5 * size && g.bbox.v_overlap(&p.bbox) > -0.5 * size && !g.space_before
        });
        if joins {
            groups.last_mut().unwrap().push(i);
        } else {
            groups.push(vec![i]);
        }
    }
    let group_box = |grp: &[usize]| Rect::union_all(grp.iter().map(|&i| &raw.glyphs[i].bbox)).unwrap();
    let is_math_group = |grp: &[usize]| {
        let math = grp.iter().filter(|&&i| glyph_is_math(raw, &raw.glyphs[i])).count();
        let letters = grp.iter().filter(|&&i| glyph_is_text_letter(raw, &raw.glyphs[i])).count();
        // Function names (sin, log) are allowed when maths surrounds them.
        math > 0 && letters <= 1 || (math == 0 && letters == 0)
    };
    let boxes: Vec<Rect> = groups.iter().map(|g| group_box(g)).collect();
    let mathy: Vec<bool> = groups.iter().map(|g| is_math_group(g)).collect();
    // Only real maths fonts seed an equation; symbols such as "−" or "≤" in
    // ordinary text must not turn a table cell into a formula.
    let has_math: Vec<bool> = groups.iter().map(|g| g.iter().any(|&i| raw.styles[raw.glyphs[i].style].math)).collect();

    // Candidate bars: short horizontal rules.
    let bars: Vec<usize> = raw
        .rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.orientation == RuleOrientation::Horizontal && r.bbox.width() < 0.6 * raw.width && r.width < 2.0)
        .map(|(i, _)| i)
        .collect();

    // Connected components of maths groups (+ neutral groups next to them) and bars.
    let m = groups.len();
    let total = m + bars.len();
    let item_box = |k: usize| if k < m { boxes[k] } else { raw.rules[bars[k - m]].bbox };
    let item_size = |k: usize| if k < m { raw.styles[raw.glyphs[groups[k][0]].style].size } else { 10.0 };
    let usable = |k: usize| if k < m { mathy[k] } else { true };
    let mut parent: Vec<usize> = (0..total).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for a in 0..total {
        if !usable(a) {
            continue;
        }
        for b in a + 1..total {
            if !usable(b) {
                continue;
            }
            let s = item_size(a).max(item_size(b));
            let (ba, bb) = (item_box(a), item_box(b));
            let ea = Rect::new(ba.x0 - 1.2 * s, ba.y0 - 0.35 * s, ba.x1 + 1.2 * s, ba.y1 + 0.35 * s);
            if ea.intersects(&bb) {
                let (x, y) = (find(&mut parent, a), find(&mut parent, b));
                if x != y {
                    parent[x] = y;
                }
            }
        }
    }
    let mut comps: std::collections::HashMap<usize, Vec<usize>> = Default::default();
    for k in 0..total {
        if usable(k) {
            let r = find(&mut parent, k);
            comps.entry(r).or_default().push(k);
        }
    }

    let mut out = Vec::new();
    let mut complex = Vec::new();
    let mut remove_glyph = vec![false; n];
    let mut remove_rule = vec![false; raw.rules.len()];
    for (_, members) in comps {
        let gs: Vec<usize> = members.iter().copied().filter(|&k| k < m).collect();
        if gs.is_empty() || !gs.iter().any(|&k| has_math[k]) {
            continue;
        }
        let bbox = members.iter().fold(item_box(members[0]), |r, &k| r.union(&item_box(k)));
        // A display equation has its lines to itself: no text beside it.
        let text_beside = (0..m).any(|k| {
            !members.contains(&k) && boxes[k].v_overlap(&bbox) > 0.3 * boxes[k].height().min(bbox.height()) && !mathy[k]
        });
        let glyph_count: usize = gs.iter().map(|&k| groups[k].len()).sum();
        if text_beside || glyph_count < 2 {
            continue;
        }
        let atoms: Vec<MAtom> = gs.iter().flat_map(|&k| groups[k].iter()).map(|&i| atom_of(raw, &raw.glyphs[i])).collect();
        let bar_rects: Vec<Rect> = members.iter().filter(|&&k| k >= m).map(|&k| raw.rules[bars[k - m]].bbox).collect();
        if is_complex(&atoms, &bar_rects) {
            complex.push(bbox);
            continue;
        }
        let size = atoms.iter().map(|a| a.size).fold(0.0, f32::max);
        let baseline = main_baseline(&atoms).unwrap_or(bbox.y1);
        let nodes = parse(atoms, &bar_rects);
        for &k in &gs {
            for &i in &groups[k] {
                remove_glyph[i] = true;
            }
        }
        for &k in members.iter().filter(|&&k| k >= m) {
            remove_rule[bars[k - m]] = true;
        }
        out.push(DisplayEquation { bbox, nodes, baseline, size });
    }
    let mut i = 0;
    raw.glyphs.retain(|_| {
        let keep = !remove_glyph[i];
        i += 1;
        keep
    });
    let mut i = 0;
    raw.rules.retain(|_| {
        let keep = !remove_rule[i];
        i += 1;
        keep
    });
    out.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
    (out, complex)
}

/// Sets every run to the equation's base size (Office Math scales scripts itself).
pub fn normalize_sizes(nodes: &mut [MathNode], size: f32) {
    for n in nodes {
        match n {
            MathNode::Run { size: s, .. } => *s = size,
            MathNode::Frac { num, den } => {
                normalize_sizes(num, size);
                normalize_sizes(den, size);
            }
            MathNode::Script { base, sub, sup } => {
                normalize_sizes(base, size);
                if let Some(v) = sub {
                    normalize_sizes(v, size);
                }
                if let Some(v) = sup {
                    normalize_sizes(v, size);
                }
            }
            MathNode::Radical { degree, body } => {
                if let Some(v) = degree {
                    normalize_sizes(v, size);
                }
                normalize_sizes(body, size);
            }
            MathNode::Nary { sub, sup, body, .. } => {
                if let Some(v) = sub {
                    normalize_sizes(v, size);
                }
                if let Some(v) = sup {
                    normalize_sizes(v, size);
                }
                normalize_sizes(body, size);
            }
        }
    }
}

/// True if a glyph should start or continue inline maths.
pub fn inline_math_glyph(raw: &RawPage, g: &Glyph) -> bool {
    raw.styles[g.style].math
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(c: &str, x: f32, base: f32, size: f32) -> MAtom {
        MAtom {
            text: c.into(),
            bbox: Rect::new(x, base - 0.75 * size, x + 0.5 * size, base + 0.25 * size),
            baseline: base,
            size,
            upright: false,
            bold: false,
            node: None,
        }
    }

    #[test]
    fn scripts_and_fraction() {
        // E = mc²
        let nodes = parse(vec![at("E", 0.0, 100.0, 10.0), at("=", 7.0, 100.0, 10.0), at("m", 13.0, 100.0, 10.0), at("c", 18.0, 100.0, 10.0), at("2", 23.0, 96.0, 7.0)], &[]);
        assert_eq!(MathNode::plain(&nodes), "E=mc^2");
        assert!(matches!(nodes.last(), Some(MathNode::Script { sup: Some(_), sub: None, .. })));

        // x = a / b with a bar at y=97.5
        let atoms = vec![at("x", 0.0, 100.0, 10.0), at("=", 7.0, 100.0, 10.0), at("a", 15.0, 94.0, 10.0), at("b", 15.0, 106.0, 10.0)];
        let nodes = parse(atoms, &[Rect::new(14.0, 97.2, 21.0, 97.8)]);
        assert_eq!(MathNode::plain(&nodes), "x=(a)/(b)");
    }

    #[test]
    fn radicals_and_operators() {
        // √(b) with bar starting at the radical's right edge.
        let mut rad = at("√", 0.0, 100.0, 10.0);
        rad.bbox = Rect::new(0.0, 88.0, 8.0, 102.0);
        let atoms = vec![rad, at("b", 8.5, 100.0, 10.0)];
        let nodes = parse(atoms, &[Rect::new(8.0, 89.0, 14.0, 89.6)]);
        assert_eq!(MathNode::plain(&nodes), "√(b)");

        // ∑ with limits above/below followed by k = n
        let mut sum = at("∑", 0.0, 100.0, 10.0);
        sum.bbox = Rect::new(0.0, 88.0, 14.0, 104.0);
        let atoms = vec![sum, at("n", 5.0, 85.0, 7.0), at("k", 5.0, 111.0, 7.0), at("k", 16.0, 100.0, 10.0), at("=", 22.0, 100.0, 10.0), at("n", 29.0, 100.0, 10.0)];
        let nodes = parse(atoms, &[]);
        assert_eq!(MathNode::plain(&nodes), "∑_k^nk=n");
        assert!(matches!(&nodes[0], MathNode::Nary { body, .. } if MathNode::plain(body) == "k"));
    }

    #[test]
    fn cmex_mapping() {
        assert_eq!(cmex_char('X'), '∑');
        assert_eq!(cmex_char('Z'), '∫');
        assert!(is_math_font("ABCDEF+CMMI10"));
        assert!(is_math_font("CambriaMath"));
        assert!(!is_math_font("Times-Roman"));
    }
}
