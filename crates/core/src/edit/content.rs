//! Content streams: lexing, following the text position, writing operators.

use std::collections::HashSet;

use anyhow::bail;

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Arg {
    Num(f64),
    Str(Vec<u8>),
    /// A name, as written (still `#xx`-escaped).
    Name(Vec<u8>),
    Array(Vec<Arg>),
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct Op {
    /// Byte range of the operator together with its operands.
    pub start: usize,
    pub end: usize,
    pub name: Vec<u8>,
    pub args: Vec<Arg>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lexer<'a> {
    d: &'a [u8],
    p: usize,
}

impl Lexer<'_> {
    fn skip_ws(&mut self) {
        loop {
            while self.p < self.d.len() && is_ws(self.d[self.p]) {
                self.p += 1;
            }
            if self.p < self.d.len() && self.d[self.p] == b'%' {
                while self.p < self.d.len() && self.d[self.p] != b'\n' && self.d[self.p] != b'\r' {
                    self.p += 1;
                }
                continue;
            }
            break;
        }
    }

    fn regular(&mut self) -> &[u8] {
        let s = self.p;
        while self.p < self.d.len() && !is_ws(self.d[self.p]) && !is_delim(self.d[self.p]) {
            self.p += 1;
        }
        &self.d[s..self.p]
    }

    fn literal(&mut self) -> Vec<u8> {
        let d = self.d;
        self.p += 1;
        let mut depth = 1;
        let mut out = Vec::new();
        while self.p < d.len() {
            let c = d[self.p];
            self.p += 1;
            match c {
                b'\\' => {
                    let Some(&e) = d.get(self.p) else { break };
                    self.p += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match d.get(self.p) {
                                    Some(&o @ b'0'..=b'7') => {
                                        v = v * 8 + (o - b'0') as u32;
                                        self.p += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        b'\r' => {
                            if d.get(self.p) == Some(&b'\n') {
                                self.p += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\r' => {
                    if d.get(self.p) == Some(&b'\n') {
                        self.p += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn hex(&mut self) -> Vec<u8> {
        self.p += 1;
        let mut digits = Vec::new();
        while self.p < self.d.len() && self.d[self.p] != b'>' {
            if let Some(v) = (self.d[self.p] as char).to_digit(16) {
                digits.push(v as u8);
            }
            self.p += 1;
        }
        self.p += 1;
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        digits.chunks(2).map(|c| c[0] * 16 + c[1]).collect()
    }

    fn object(&mut self, depth: usize) -> Option<Arg> {
        self.skip_ws();
        let d = self.d;
        let &b = d.get(self.p)?;
        if depth > 64 {
            self.p += 1;
            return Some(Arg::Other);
        }
        Some(match b {
            b'(' => Arg::Str(self.literal()),
            b'<' if d.get(self.p + 1) == Some(&b'<') => {
                self.p += 2;
                loop {
                    self.skip_ws();
                    if self.p >= d.len() {
                        break;
                    }
                    if d[self.p] == b'>' {
                        self.p += if d.get(self.p + 1) == Some(&b'>') { 2 } else { 1 };
                        break;
                    }
                    if self.object(depth + 1).is_none() {
                        break;
                    }
                }
                Arg::Other
            }
            b'<' => Arg::Str(self.hex()),
            b'[' => {
                self.p += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.p >= d.len() {
                        break;
                    }
                    if d[self.p] == b']' {
                        self.p += 1;
                        break;
                    }
                    match self.object(depth + 1) {
                        Some(a) => items.push(a),
                        None => break,
                    }
                }
                Arg::Array(items)
            }
            b'/' => {
                self.p += 1;
                Arg::Name(self.regular().to_vec())
            }
            b'{' | b'}' | b')' | b'>' | b']' => {
                self.p += 1;
                Arg::Other
            }
            _ => {
                let t = self.regular();
                if t.is_empty() {
                    self.p += 1;
                    return Some(Arg::Other);
                }
                if t[0].is_ascii_digit() || matches!(t[0], b'+' | b'-' | b'.') {
                    Arg::Num(parse_number(t))
                } else {
                    Arg::Other
                }
            }
        })
    }
}

pub(crate) fn parse_number(t: &[u8]) -> f64 {
    let s = String::from_utf8_lossy(t);
    if let Ok(v) = s.parse::<f64>() {
        return v;
    }
    // Lenient: "--1", "1.2.3", "-.5-" …
    let neg = s.starts_with('-');
    let body: String = s.trim_start_matches(['+', '-']).chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = body.splitn(3, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    let v = format!("{}.{}", if int.is_empty() { "0" } else { int }, if frac.is_empty() { "0" } else { frac }).parse::<f64>().unwrap_or(0.0);
    if neg {
        -v
    } else {
        v
    }
}

/// Position just after the `EI` of an inline image whose `BI` ends at `from`.
fn skip_inline_image(d: &[u8], from: usize) -> anyhow::Result<usize> {
    let mut i = from;
    loop {
        if i + 2 > d.len() {
            bail!("inline image without data");
        }
        if &d[i..i + 2] == b"ID" && (is_ws(d[i - 1]) || is_delim(d[i - 1])) && (i + 2 == d.len() || is_ws(d[i + 2])) {
            i += 3;
            break;
        }
        i += 1;
    }
    while i + 2 <= d.len() {
        if &d[i..i + 2] == b"EI" && is_ws(d[i - 1]) && (i + 2 == d.len() || is_ws(d[i + 2]) || is_delim(d[i + 2])) {
            return Ok(i + 2);
        }
        i += 1;
    }
    bail!("unterminated inline image")
}

pub(crate) fn parse_content(d: &[u8]) -> anyhow::Result<Vec<Op>> {
    let mut lx = Lexer { d, p: 0 };
    let mut ops = Vec::new();
    let mut args = Vec::new();
    let mut start: Option<usize> = None;
    loop {
        lx.skip_ws();
        if lx.p >= d.len() {
            break;
        }
        let b = d[lx.p];
        if matches!(b, b'(' | b'<' | b'[' | b'/' | b'{' | b'}' | b')' | b'>' | b']') || b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.') {
            start.get_or_insert(lx.p);
            if let Some(a) = lx.object(0) {
                args.push(a);
            }
            continue;
        }
        let s = lx.p;
        let word = lx.regular().to_vec();
        if word.is_empty() {
            lx.p += 1;
            continue;
        }
        if matches!(word.as_slice(), b"true" | b"false" | b"null") {
            start.get_or_insert(s);
            args.push(Arg::Other);
            continue;
        }
        let op_start = start.take().unwrap_or(s);
        if word == b"BI" {
            let end = skip_inline_image(d, lx.p)?;
            lx.p = end;
            args.clear();
            ops.push(Op { start: op_start, end, name: word, args: Vec::new() });
            continue;
        }
        ops.push(Op { start: op_start, end: lx.p, name: word, args: std::mem::take(&mut args) });
    }
    Ok(ops)
}

/// Decodes `#xx` escapes of a name as written in a content stream.
pub(crate) fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            if let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(&raw[i + 1..i + 3]), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Matrices
// ---------------------------------------------------------------------------

pub(crate) type Mat = [f64; 6];
pub(crate) const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m` followed by `n`.
pub(crate) fn mul(m: &Mat, n: &Mat) -> Mat {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

pub(crate) fn apply(m: &Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// A vector (no translation).
pub(crate) fn apply_vec(m: &Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y, m[1] * x + m[3] * y)
}

pub(crate) fn translate(x: f64, y: f64) -> Mat {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

pub(crate) fn invert(m: &Mat) -> Option<Mat> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)])
}

/// Length of the x axis of `m` (scale along the baseline).
pub(crate) fn x_scale(m: &Mat) -> f64 {
    (m[0] * m[0] + m[1] * m[1]).sqrt()
}

// ---------------------------------------------------------------------------
// Following the text position
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TextState {
    font: Vec<u8>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
}

#[derive(Clone)]
struct GState {
    ctm: Mat,
    ts: TextState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ShowKind {
    Tj,
    TJ,
    Quote,
    DQuote,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Code(Vec<u8>),
    /// A TJ adjustment in thousandths of a text space unit.
    Kern(f64),
    /// Baseline offset for the following glyphs, in em (0 = the run's own baseline).
    Rise(f64),
}

/// A text-showing operator that draws glyphs.
#[derive(Clone, Debug)]
pub(crate) struct Show {
    pub op: usize,
    pub kind: ShowKind,
    /// Index of the matching PDFium object within its container.
    pub object: usize,
    /// Font resource name as written.
    pub font: Vec<u8>,
    pub size: f64,
    pub tc: f64,
    pub tw: f64,
    pub th: f64,
    pub rise: f64,
    pub ctm: Mat,
    pub tlm: Mat,
    /// Offset of the first glyph from the line start (text space).
    pub s: f64,
    /// Strings and adjustments in order.
    pub parts: Vec<Arg>,
    /// Advance from the first glyph to the end of the operator (text space),
    /// when a following operator continues from there.
    pub advance: Option<f64>,
}

impl Show {
    pub fn unit(&self) -> f64 {
        self.size * self.th / 1000.0
    }

    /// Text matrix at the first glyph.
    pub fn tm(&self) -> Mat {
        mul(&translate(self.s, 0.0), &self.tlm)
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for p in &self.parts {
            if let Arg::Str(b) = p {
                v.extend_from_slice(b);
            }
        }
        v
    }

    /// Adjustments before the first glyph (thousandths).
    pub fn init(&self) -> f64 {
        self.parts
            .iter()
            .map_while(|p| match p {
                Arg::Num(n) => Some(*n),
                Arg::Str(b) if b.is_empty() => Some(0.0),
                _ => None,
            })
            .sum()
    }

    /// The operator's content from its first glyph on, as tokens (strings whole).
    pub fn raw_tokens(&self) -> Vec<Tok> {
        let mut toks = Vec::new();
        let mut started = false;
        for p in &self.parts {
            match p {
                Arg::Str(b) if !b.is_empty() => {
                    started = true;
                    toks.push(Tok::Code(b.clone()));
                }
                Arg::Num(n) if started => toks.push(Tok::Kern(*n)),
                _ => {}
            }
        }
        toks
    }
}

pub(crate) struct Scan {
    pub ops: Vec<Op>,
    pub shows: Vec<Show>,
    /// Operator indices of `Do` operators that paint form XObjects, in order.
    pub form_dos: Vec<usize>,
    /// Operators that paint images (`Do` of image XObjects and inline images), in order.
    pub image_ops: Vec<usize>,
    /// Lowest q/Q nesting reached (negative with stray Q operators) and the final nesting.
    pub min_depth: i64,
    pub end_depth: i64,
}

impl Scan {
    /// Only the operators (when the text cannot be followed).
    pub fn ops_only(content: &[u8], forms: &HashSet<Vec<u8>>, images: &HashSet<Vec<u8>>) -> Self {
        let ops = parse_content(content).unwrap_or_default();
        let (mut depth, mut min_depth) = (0i64, 0i64);
        let mut form_dos = Vec::new();
        let mut image_ops = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            match op.name.as_slice() {
                b"q" => depth += 1,
                b"Q" => {
                    depth -= 1;
                    min_depth = min_depth.min(depth);
                }
                b"Do" => {
                    if let Some(Arg::Name(n)) = op.args.first() {
                        let n = decode_name(n);
                        if forms.contains(&n) {
                            form_dos.push(i);
                        } else if images.contains(&n) {
                            image_ops.push(i);
                        }
                    }
                }
                b"BI" => image_ops.push(i),
                _ => {}
            }
        }
        Scan { ops, shows: Vec::new(), form_dos, image_ops, min_depth, end_depth: depth }
    }
}

/// A painted path: its subpaths (byte ranges of their construction
/// operators, with bounds in the content's coordinate space).
#[derive(Clone, Debug)]
pub(crate) struct PathPaint {
    pub subpaths: Vec<(usize, usize, [f64; 4])>,
    /// Byte range of the painting operator.
    pub paint: (usize, usize),
    /// The path also sets the clipping path.
    pub clip: bool,
}

/// The painted paths of a content stream.
pub(crate) fn scan_paths(ops: &[Op], ctm0: Mat) -> Vec<PathPaint> {
    let mut ctm = ctm0;
    let mut stack: Vec<Mat> = Vec::new();
    let mut subs: Vec<(usize, usize, [f64; 4])> = Vec::new();
    let mut clip = false;
    let mut out = Vec::new();
    let mut in_text = false;
    let grow = |b: &mut [f64; 4], p: (f64, f64)| {
        b[0] = b[0].min(p.0);
        b[1] = b[1].min(p.1);
        b[2] = b[2].max(p.0);
        b[3] = b[3].max(p.1);
    };
    let empty = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for op in ops {
        let a = &op.args;
        let num = |k: usize| -> f64 {
            match a.get(k) {
                Some(Arg::Num(v)) => *v,
                _ => 0.0,
            }
        };
        match op.name.as_slice() {
            b"q" => stack.push(ctm),
            b"Q" => {
                if let Some(m) = stack.pop() {
                    ctm = m;
                }
            }
            b"cm" if a.len() >= 6 => ctm = mul(&[num(0), num(1), num(2), num(3), num(4), num(5)], &ctm),
            b"BT" => in_text = true,
            b"ET" => in_text = false,
            b"m" if !in_text => {
                let mut b = empty;
                grow(&mut b, apply(&ctm, num(0), num(1)));
                subs.push((op.start, op.end, b));
            }
            b"re" if !in_text => {
                let mut b = empty;
                let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                for p in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                    grow(&mut b, apply(&ctm, p.0, p.1));
                }
                subs.push((op.start, op.end, b));
            }
            b"l" | b"c" | b"v" | b"y" | b"h" if !in_text => {
                if let Some(last) = subs.last_mut() {
                    last.1 = op.end;
                    let mut k = 0;
                    while k + 1 < a.len() {
                        grow(&mut last.2, apply(&ctm, num(k), num(k + 1)));
                        k += 2;
                    }
                }
            }
            b"W" | b"W*" => clip = true,
            b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                if !subs.is_empty() {
                    out.push(PathPaint { subpaths: std::mem::take(&mut subs), paint: (op.start, op.end), clip: clip || op.name == b"n" });
                }
                clip = false;
            }
            _ => {}
        }
    }
    out
}

/// Follows the text position through `content` and pairs every glyph-showing
/// operator with PDFium's text object (`objs`: object index and origin, in
/// order, in the same coordinate space as `ctm0`).
pub(crate) fn scan(content: &[u8], objs: &[(usize, (f64, f64))], forms: &HashSet<Vec<u8>>, images: &HashSet<Vec<u8>>, ctm0: Mat) -> anyhow::Result<Scan> {
    let ops = parse_content(content)?;
    let mut st = GState { ctm: ctm0, ts: TextState { font: Vec::new(), size: 0.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0 } };
    let mut stack: Vec<GState> = Vec::new();
    let (mut depth, mut min_depth) = (0i64, 0i64);
    let mut tlm = IDENTITY;
    let mut s = 0.0f64;
    let mut known = true;
    let mut pending: Option<usize> = None;
    let mut extra = 0.0f64;
    let mut cursor = 0usize;
    let mut shows: Vec<Show> = Vec::new();
    let mut form_dos = Vec::new();
    let mut image_ops = Vec::new();
    let tol = |p: (f64, f64)| 0.6 + 1e-4 * (p.0.abs() + p.1.abs());

    for (i, op) in ops.iter().enumerate() {
        let a = &op.args;
        let num = |k: usize| -> f64 {
            match a.get(k) {
                Some(Arg::Num(v)) => *v,
                _ => 0.0,
            }
        };
        let line_move = |tlm: &mut Mat, s: &mut f64, x: f64, y: f64| {
            *tlm = mul(&translate(x, y), tlm);
            *s = 0.0;
        };
        match op.name.as_slice() {
            b"q" => {
                stack.push(st.clone());
                depth += 1;
            }
            b"Q" => {
                if let Some(g) = stack.pop() {
                    st = g;
                }
                depth -= 1;
                min_depth = min_depth.min(depth);
            }
            b"cm" if a.len() >= 6 => st.ctm = mul(&[num(0), num(1), num(2), num(3), num(4), num(5)], &st.ctm),
            b"Do" => {
                if let Some(Arg::Name(n)) = a.first() {
                    let n = decode_name(n);
                    if forms.contains(&n) {
                        form_dos.push(i);
                    } else if images.contains(&n) {
                        image_ops.push(i);
                    }
                }
            }
            b"BI" => image_ops.push(i),
            b"BT" => {
                tlm = IDENTITY;
                s = 0.0;
                known = true;
                pending = None;
            }
            b"ET" => {
                known = true;
                pending = None;
            }
            b"Tm" if a.len() >= 6 => {
                tlm = [num(0), num(1), num(2), num(3), num(4), num(5)];
                s = 0.0;
                known = true;
                pending = None;
            }
            b"Td" => {
                line_move(&mut tlm, &mut s, num(0), num(1));
                known = true;
                pending = None;
            }
            b"TD" => {
                st.ts.tl = -num(1);
                line_move(&mut tlm, &mut s, num(0), num(1));
                known = true;
                pending = None;
            }
            b"T*" => {
                line_move(&mut tlm, &mut s, 0.0, -st.ts.tl);
                known = true;
                pending = None;
            }
            b"Tc" => st.ts.tc = num(0),
            b"Tw" => st.ts.tw = num(0),
            b"Tz" => st.ts.th = num(0) / 100.0,
            b"TL" => st.ts.tl = num(0),
            b"Ts" => st.ts.rise = num(0),
            b"Tf" => {
                if let Some(Arg::Name(n)) = a.first() {
                    st.ts.font = n.clone();
                }
                st.ts.size = num(1);
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                let kind = match op.name.as_slice() {
                    b"Tj" => ShowKind::Tj,
                    b"TJ" => ShowKind::TJ,
                    b"'" => ShowKind::Quote,
                    _ => ShowKind::DQuote,
                };
                if kind == ShowKind::DQuote {
                    st.ts.tw = num(0);
                    st.ts.tc = num(1);
                }
                if matches!(kind, ShowKind::Quote | ShowKind::DQuote) {
                    line_move(&mut tlm, &mut s, 0.0, -st.ts.tl);
                    known = true;
                    pending = None;
                }
                let parts: Vec<Arg> = match kind {
                    ShowKind::TJ => match a.first() {
                        Some(Arg::Array(v)) => v.iter().filter(|x| matches!(x, Arg::Num(_) | Arg::Str(_))).cloned().collect(),
                        _ => Vec::new(),
                    },
                    _ => a.iter().rev().find(|x| matches!(x, Arg::Str(_))).cloned().into_iter().collect(),
                };
                let glyphs = parts.iter().any(|p| matches!(p, Arg::Str(b) if !b.is_empty()));
                let unit = st.ts.size * st.ts.th / 1000.0;
                let mut init = 0.0;
                for p in &parts {
                    match p {
                        Arg::Num(n) => init += n,
                        Arg::Str(b) if !b.is_empty() => break,
                        _ => {}
                    }
                }
                if !glyphs {
                    let dk = -init * unit;
                    if known {
                        s += dk;
                    } else {
                        extra += dk;
                    }
                    continue;
                }
                let init_disp = -init * unit;
                let Some(&(object, origin)) = objs.get(cursor) else {
                    bail!("the content shows more text than PDFium reports");
                };
                cursor += 1;
                if known {
                    s += init_disp;
                    let m = mul(&mul(&translate(s, 0.0), &tlm), &st.ctm);
                    let o = apply(&m, 0.0, st.ts.rise);
                    if (o.0 - origin.0).hypot(o.1 - origin.1) > tol(o) {
                        bail!("could not locate text object {object} in the content");
                    }
                } else {
                    // Continues where the previous operator ended: PDFium knows where that is.
                    let m = mul(&mul(&translate(s, 0.0), &tlm), &st.ctm);
                    let base = apply(&m, 0.0, st.ts.rise);
                    let uu = m[0] * m[0] + m[1] * m[1];
                    if uu < 1e-12 {
                        bail!("degenerate text matrix");
                    }
                    let dx = ((origin.0 - base.0) * m[0] + (origin.1 - base.1) * m[1]) / uu;
                    let o = apply(&m, dx, st.ts.rise);
                    if (o.0 - origin.0).hypot(o.1 - origin.1) > tol(o) {
                        bail!("could not follow the text position to object {object}");
                    }
                    if let Some(p) = pending {
                        shows[p].advance = Some(dx - extra - init_disp);
                    }
                    s += dx;
                }
                shows.push(Show {
                    op: i,
                    kind,
                    object,
                    font: st.ts.font.clone(),
                    size: st.ts.size,
                    tc: st.ts.tc,
                    tw: st.ts.tw,
                    th: st.ts.th,
                    rise: st.ts.rise,
                    ctm: st.ctm,
                    tlm,
                    s,
                    parts,
                    advance: None,
                });
                pending = Some(shows.len() - 1);
                extra = 0.0;
                known = false;
            }
            _ => {}
        }
    }
    if cursor != objs.len() {
        bail!("PDFium reports text that is not in this content stream");
    }
    Ok(Scan { ops, shows, form_dos, image_ops, min_depth, end_depth: depth })
}

// ---------------------------------------------------------------------------
// Writing operators
// ---------------------------------------------------------------------------

pub(crate) fn fmt(v: f64) -> String {
    let v = if v.abs() < 1e-7 { 0.0 } else { v };
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

pub(crate) fn fmt_mat(m: &Mat) -> String {
    m.iter().map(|v| fmt(*v)).collect::<Vec<_>>().join(" ")
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::from("<");
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s.push('>');
    s
}

/// TJ operators for `toks`; rises become `Ts` changes around the glyphs
/// they apply to (`size` and `base_rise` in text space units).
pub(crate) fn tj_ops(toks: &[Tok], size: f64, base_rise: f64) -> String {
    let mut out = String::new();
    let mut arr = String::new();
    let mut run: Vec<u8> = Vec::new();
    let mut rise = 0.0f64;
    let flush_run = |arr: &mut String, run: &mut Vec<u8>| {
        if !run.is_empty() {
            arr.push_str(&hex(run));
            run.clear();
        }
    };
    let flush_arr = |out: &mut String, arr: &mut String| {
        if !arr.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&format!("[{arr}] TJ"));
            arr.clear();
        }
    };
    for t in toks {
        match t {
            Tok::Code(c) => run.extend_from_slice(c),
            Tok::Kern(n) => {
                flush_run(&mut arr, &mut run);
                arr.push_str(&format!(" {} ", fmt(*n)));
            }
            Tok::Rise(r) => {
                if (r - rise).abs() > 1e-6 {
                    flush_run(&mut arr, &mut run);
                    flush_arr(&mut out, &mut arr);
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&format!("{} Ts", fmt(base_rise + r * size)));
                    rise = *r;
                }
            }
        }
    }
    flush_run(&mut arr, &mut run);
    flush_arr(&mut out, &mut arr);
    if rise.abs() > 1e-6 {
        out.push_str(&format!(" {} Ts", fmt(base_rise)));
    }
    if out.is_empty() {
        out.push_str("[] TJ");
    }
    out
}

pub(crate) fn write_name(raw: &[u8]) -> String {
    format!("/{}", String::from_utf8_lossy(raw))
}

/// Operators that set what `'` and `"` set before showing text.
pub(crate) fn quote_prefix(sh: &Show, with_line_move: bool) -> String {
    match sh.kind {
        ShowKind::Quote if with_line_move => "T* ".into(),
        ShowKind::DQuote if with_line_move => format!("{} Tw {} Tc T* ", fmt(sh.tw), fmt(sh.tc)),
        ShowKind::DQuote => format!("{} Tw {} Tc ", fmt(sh.tw), fmt(sh.tc)),
        _ => String::new(),
    }
}

/// Replacement for an operator whose glyphs are removed: it still moves the
/// text position as far as the original did, when something continues from there.
pub(crate) fn neutralize(sh: &Show) -> anyhow::Result<String> {
    let mut out = quote_prefix(sh, true);
    if let Some(a) = sh.advance {
        let unit = sh.unit();
        if unit.abs() < 1e-9 {
            if a.abs() > 1e-6 {
                bail!("cannot remove text drawn with a zero font size");
            }
        } else {
            out.push_str(&format!("[{}] TJ", fmt(sh.init() - a / unit)));
        }
    }
    Ok(out)
}

/// What ends a replacement drawn in its own text object: back to the line
/// the operator was on, at the position where it ended.
pub(crate) fn resume(sh: &Show) -> String {
    let mut s = format!("BT {} Tm", fmt_mat(&sh.tlm));
    if let Some(a) = sh.advance {
        let unit = sh.unit();
        if unit.abs() > 1e-9 {
            s.push_str(&format!(" [{}] TJ", fmt(-(sh.s + a) / unit)));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_operators_and_operands() {
        let d = b"q 1 0 0 1 72 700 cm BT /F1 12 Tf [(Hel\\)lo) -250 <0041 42>] TJ (a\\053b) Tj ET\nBI /W 2 /H 1 ID \x00\xff\nEI Q % done\n";
        let ops = parse_content(d).unwrap();
        let names: Vec<&[u8]> = ops.iter().map(|o| o.name.as_slice()).collect();
        assert_eq!(names, vec![&b"q"[..], b"cm", b"BT", b"Tf", b"TJ", b"Tj", b"ET", b"BI", b"Q"]);
        assert_eq!(ops[4].args, vec![Arg::Array(vec![Arg::Str(b"Hel)lo".to_vec()), Arg::Num(-250.0), Arg::Str(vec![0x00, 0x41, 0x42])])]);
        assert_eq!(ops[5].args, vec![Arg::Str(b"a+b".to_vec())]);
        assert_eq!(&d[ops[4].start..ops[4].end], b"[(Hel\\)lo) -250 <0041 42>] TJ");
    }

    #[test]
    fn follows_text_position_and_measures_advances() {
        let d = b"BT /F1 10 Tf 100 700 Td (ab) Tj [-100 (cd)] TJ 0 -12 Td (ef) Tj ET";
        let objs = vec![(0, (100.0, 700.0)), (1, (100.0 + 11.0 + 1.0, 700.0)), (2, (100.0, 688.0))];
        let sc = scan(d, &objs, &HashSet::new(), &HashSet::new(), IDENTITY).unwrap();
        assert_eq!(sc.shows.len(), 3);
        assert!((sc.shows[0].advance.unwrap() - 11.0).abs() < 1e-6);
        assert!(sc.shows[1].advance.is_none());
        assert_eq!(neutralize(&sc.shows[0]).unwrap(), "[-1100] TJ");
        assert_eq!(resume(&sc.shows[0]), "BT 1 0 0 1 100 700 Tm [-1100] TJ");
        let bad = vec![(0, (100.0, 700.0)), (1, (112.0, 700.0)), (2, (100.0, 600.0))];
        assert!(scan(d, &bad, &HashSet::new(), &HashSet::new(), IDENTITY).is_err());
    }

    #[test]
    fn forms_and_matrices() {
        let d = b"q 2 0 0 2 10 20 cm /X1 Do /Im1 Do Q";
        let forms: HashSet<Vec<u8>> = [b"X1".to_vec()].into_iter().collect();
        let images: HashSet<Vec<u8>> = [b"Im1".to_vec()].into_iter().collect();
        let sc = scan(d, &[], &forms, &images, IDENTITY).unwrap();
        assert_eq!(sc.form_dos, vec![2]);
        assert_eq!(sc.image_ops, vec![3]);
        let m = [2.0, 0.0, 0.0, 2.0, 10.0, 20.0];
        let i = invert(&m).unwrap();
        let p = apply(&mul(&m, &i), 3.0, 4.0);
        assert!((p.0 - 3.0).abs() < 1e-9 && (p.1 - 4.0).abs() < 1e-9);
    }

    #[test]
    fn finds_path_subpaths() {
        let d = b"q 2 0 0 2 0 0 cm 1 1 m 2 1 l 2 2 l h 5 5 m 6 6 l f Q 0 0 10 10 re W n";
        let ops = parse_content(d).unwrap();
        let paths = scan_paths(&ops, IDENTITY);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].subpaths.len(), 2);
        assert_eq!(paths[0].subpaths[0].2, [2.0, 2.0, 4.0, 4.0]);
        assert_eq!(&d[paths[0].subpaths[1].0..paths[0].subpaths[1].1], b"5 5 m 6 6 l");
        assert!(!paths[0].clip && paths[1].clip);
    }

    #[test]
    fn writes_names_numbers_and_rises() {
        assert_eq!(decode_name(b"F#201"), b"F 1".to_vec());
        assert_eq!(parse_number(b"--1.5"), -1.5);
        assert_eq!(fmt(-0.00001), "0");
        assert_eq!(tj_ops(&[Tok::Code(vec![65]), Tok::Kern(-250.0), Tok::Code(vec![66, 67])], 10.0, 0.0), "[<41> -250 <4243>] TJ");
        assert_eq!(
            tj_ops(&[Tok::Code(vec![65]), Tok::Rise(0.5), Tok::Code(vec![66]), Tok::Rise(0.0), Tok::Code(vec![67])], 10.0, 1.0),
            "[<41>] TJ 6 Ts [<42>] TJ 1 Ts [<43>] TJ"
        );
    }
}
