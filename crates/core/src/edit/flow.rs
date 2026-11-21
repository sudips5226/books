//! Text flow: when a rewritten paragraph no longer fits in its column, the
//! lines at the bottom of the column move to the top of the next column, and
//! from the last column to the first column of the next page (pages are
//! added at the end when needed).

use super::*;

/// Header and footer bands (fraction of the page height) are not part of columns.
const BAND: f64 = 0.06;

/// Where a line in a column comes from.
#[derive(Clone, Debug)]
enum LineSrc {
    /// Existing runs of this page.
    Runs(Vec<usize>),
    /// Line of the rewritten paragraph.
    Wrapped(usize),
    /// Line carried from the page before.
    Carried(usize),
}

#[derive(Clone, Debug)]
struct FLine {
    src: LineSrc,
    /// Box now (display points).
    left: f64,
    top: f64,
    bottom: f64,
    /// Where it was (display points: left, top).
    from: (f64, f64),
}

/// Lines that continue on the next page.
#[derive(Clone, Debug, Default)]
pub(super) struct Carry {
    pub lines: Vec<CarryLine>,
    /// The column they left (display points).
    pub col_left: f64,
    pub col_top: f64,
    pub col_width: f64,
    /// Bottom of the body text on the page they left.
    pub body_bottom: f64,
    /// Space between lines.
    pub gap: f64,
}

/// A carried line: runs whose text matrices are in line space (display
/// points from the line's top-left corner, y up).
#[derive(Clone, Debug)]
pub(super) struct CarryLine {
    /// Offset from the column's left edge and from the first carried line's top.
    pub dx: f64,
    pub dy: f64,
    pub height: f64,
    pub runs: Vec<SegRun>,
}

/// Line space at (`x`, `y`) on the displayed page → page user space.
fn line_matrix(tf: &PageTransform, x: f64, y: f64) -> Mat {
    let o = tf.to_user(x as f32, y as f32);
    let r = tf.to_user(x as f32 + 1.0, y as f32);
    let u = tf.to_user(x as f32, y as f32 - 1.0);
    [(r.0 - o.0) as f64, (r.1 - o.1) as f64, (u.0 - o.0) as f64, (u.1 - o.1) as f64, o.0 as f64, o.1 as f64]
}

/// Box of a line (display points: left, top, right, bottom).
fn line_box(a: &Analysis, l: usize) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for r in &a.lines[l].runs {
        let g = &a.geoms[*r];
        b = [b[0].min(g.left), b[1].min(g.top), b[2].max(g.right), b[3].max(g.bottom)];
    }
    b
}

/// Lines of the body (not headers or footers), horizontal, with text.
fn body_lines(a: &Analysis) -> Vec<usize> {
    let h = a.tf.height() as f64;
    (0..a.lines.len())
        .filter(|l| {
            let b = line_box(a, *l);
            !a.lines[*l].text.trim().is_empty() && a.lines[*l].runs.iter().all(|r| a.geoms[*r].horizontal) && b[1] >= BAND * h && b[3] <= (1.0 - BAND) * h
        })
        .collect()
}

/// Lines of `body` in the column starting at `left`, top to bottom.
fn column_lines(a: &Analysis, body: &[usize], left: f64, width: f64) -> Vec<usize> {
    let mut v: Vec<usize> = body
        .iter()
        .copied()
        .filter(|l| {
            let b = line_box(a, *l);
            let overlap = b[2].min(left + width) - b[0].max(left);
            overlap > 0.5 * (b[2] - b[0]).max(1.0)
        })
        .collect();
    v.sort_by(|x, y| line_box(a, *x)[1].total_cmp(&line_box(a, *y)[1]));
    v
}

/// Left edge of the next column to the right of `right`.
fn next_column(a: &Analysis, body: &[usize], right: f64, width: f64) -> Option<f64> {
    body.iter().map(|l| line_box(a, *l)).filter(|b| b[0] >= right - 1.0 && b[2] - b[0] >= 0.25 * width).map(|b| b[0]).reduce(f64::min)
}

impl Session<'_, '_> {
    fn untouched(a: &Analysis, l: usize, touched: &HashSet<ObjPath>) -> bool {
        a.lines[l].runs.iter().all(|r| a.run_paths(*r).iter().all(|p| !touched.contains(p)))
    }

    /// The paragraph grew: the text below it moves down, and what no longer
    /// fits in the column moves on to the next column or page.
    pub(super) fn flow(&mut self, plan: &mut PagePlan, a: &Analysis, para: &Para, w: &mut Wrapped, touched: &mut HashSet<ObjPath>) -> anyhow::Result<()> {
        let body = body_lines(a);
        let body_bottom = body.iter().map(|l| line_box(a, *l)[3]).fold(para.bottom, f64::max);
        let (col_left, col_width) = (para.left, (para.right - para.left).max(1.0));
        let step = Self::wrapped_shift(a, w, -w.pitch).1.abs();
        let b0 = line_box(a, para.lines[0]);
        let h0 = b0[3] - b0[1];
        let mut start: Vec<FLine> = (0..w.lines.len())
            .map(|i| {
                let top = b0[1] + i as f64 * step;
                FLine { src: LineSrc::Wrapped(i), left: col_left, top, bottom: top + h0, from: (col_left, top) }
            })
            .collect();
        let grown = (w.lines.len() as f64 - w.old_lines as f64) * step;
        let own: HashSet<usize> = para.lines.iter().copied().collect();
        for l in column_lines(a, &body, col_left, col_width) {
            let b = line_box(a, l);
            if own.contains(&l) || b[1] < para.bottom - 1.0 || !Self::untouched(a, l, touched) {
                continue;
            }
            start.push(FLine { src: LineSrc::Runs(a.lines[l].runs.clone()), left: b[0], top: b[1] + grown, bottom: b[3] + grown, from: (b[0], b[1]) });
        }
        let col_top = column_lines(a, &body, col_left, col_width).first().map(|l| line_box(a, *l)[1]).unwrap_or(b0[1]);
        let gap = (step - h0).max(0.0);
        let mut carry = Carry { col_left, col_top, col_width, body_bottom, gap, lines: Vec::new() };
        self.run_flow(plan, a, &body, start, &mut carry, touched, Some(w))?;
        self.add_carry(carry, plan.page);
        Ok(())
    }

    fn add_carry(&mut self, carry: Carry, page: usize) {
        if carry.lines.is_empty() {
            return;
        }
        self.ctx.note(format!("Page {}: text that no longer fits moves on to the next page.", page + 1));
        match &mut self.carry {
            Some(c) => {
                let base = c.lines.last().map(|l| l.dy + l.height + c.gap).unwrap_or(0.0);
                c.lines.extend(carry.lines.into_iter().map(|mut l| {
                    l.dy += base;
                    l
                }));
            }
            None => self.carry = Some(carry),
        }
    }

    /// Lines carried from the page before go to the top of the first column.
    pub(super) fn flow_in(&mut self, plan: &mut PagePlan, a: &Analysis, carry: Carry, touched: &mut HashSet<ObjPath>) -> anyhow::Result<()> {
        let body = body_lines(a);
        let left = body.iter().map(|l| line_box(a, *l)).filter(|b| b[2] - b[0] >= 0.25 * carry.col_width).map(|b| b[0]).reduce(f64::min).unwrap_or(carry.col_left);
        let col = column_lines(a, &body, left, carry.col_width);
        let top = col.first().map(|l| line_box(a, *l)[1]).unwrap_or(carry.col_top).min(carry.col_top.max(BAND * a.tf.height() as f64));
        let body_bottom = body.iter().map(|l| line_box(a, *l)[3]).fold(carry.body_bottom, f64::max);
        let mut start: Vec<FLine> = carry
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| FLine { src: LineSrc::Carried(i), left: left + l.dx, top: top + l.dy, bottom: top + l.dy + l.height, from: (left + l.dx, top + l.dy) })
            .collect();
        let shift = start.last().map(|l| l.bottom + carry.gap - top).unwrap_or(0.0).max(0.0);
        for l in col {
            if !Self::untouched(a, l, touched) {
                continue;
            }
            let b = line_box(a, l);
            start.push(FLine { src: LineSrc::Runs(a.lines[l].runs.clone()), left: b[0], top: b[1] + shift, bottom: b[3] + shift, from: (b[0], b[1]) });
        }
        let mut next = Carry { col_left: left, col_top: top, col_width: carry.col_width, body_bottom, gap: carry.gap, lines: Vec::new() };
        let incoming = carry.lines;
        self.run_flow_with(plan, a, &body, start, &mut next, touched, None, &incoming)?;
        self.add_carry(next, plan.page);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn run_flow(&mut self, plan: &mut PagePlan, a: &Analysis, body: &[usize], start: Vec<FLine>, carry: &mut Carry, touched: &mut HashSet<ObjPath>, w: Option<&mut Wrapped>) -> anyhow::Result<()> {
        self.run_flow_with(plan, a, body, start, carry, touched, w, &[])
    }

    /// Places column lines, moving what passes the bottom of the body text
    /// into the next column; what is left over goes into `carry` (whose
    /// column fields describe the starting column).
    #[allow(clippy::too_many_arguments)]
    fn run_flow_with(
        &mut self,
        plan: &mut PagePlan,
        a: &Analysis,
        body: &[usize],
        mut pending: Vec<FLine>,
        carry: &mut Carry,
        touched: &mut HashSet<ObjPath>,
        mut w: Option<&mut Wrapped>,
        incoming: &[CarryLine],
    ) -> anyhow::Result<()> {
        let mut col_left = carry.col_left;
        let width = carry.col_width;
        let body_bottom = carry.body_bottom;
        let mut placed: Vec<FLine> = Vec::new();
        let mut used: HashSet<usize> = pending.iter().filter_map(|l| if let LineSrc::Runs(r) = &l.src { r.first().copied() } else { None }).collect();
        let mut fresh = true;
        loop {
            let mut cut = pending.iter().position(|l| l.bottom > body_bottom + 0.5).unwrap_or(pending.len());
            if cut == 0 && fresh {
                // A line taller than the column still has to go somewhere.
                cut = 1.min(pending.len());
            }
            placed.extend(pending.drain(..cut));
            if pending.is_empty() {
                break;
            }
            let Some(next) = next_column(a, body, col_left + width, width) else { break };
            let lines: Vec<usize> = column_lines(a, body, next, width)
                .into_iter()
                .filter(|l| Self::untouched(a, *l, touched) && !a.lines[*l].runs.first().is_some_and(|r| used.contains(r)))
                .collect();
            let top = lines.first().map(|l| line_box(a, *l)[1]).unwrap_or(carry.col_top);
            let (dx, dy) = (next - col_left, top - pending[0].top);
            for l in &mut pending {
                l.left += dx;
                l.top += dy;
                l.bottom += dy;
            }
            let shift = pending.last().map(|l| l.bottom + carry.gap - top).unwrap_or(0.0).max(0.0);
            for l in lines {
                let b = line_box(a, l);
                used.extend(a.lines[l].runs.first().copied());
                pending.push(FLine { src: LineSrc::Runs(a.lines[l].runs.clone()), left: b[0], top: b[1] + shift, bottom: b[3] + shift, from: (b[0], b[1]) });
            }
            col_left = next;
            fresh = false;
        }

        // Move the placed lines.
        let c0 = self.container(plan, &a.po, &[])?;
        for l in &placed {
            let d = (l.left - l.from.0, l.top - l.from.1);
            match &l.src {
                LineSrc::Runs(runs) => {
                    if d.0.abs() < 1e-6 && d.1.abs() < 1e-6 {
                        continue;
                    }
                    let v = display_to_page_vec(&a.tf, d.0, d.1);
                    for r in runs {
                        let paths = a.run_paths(*r);
                        if paths.iter().any(|p| touched.contains(p)) {
                            continue;
                        }
                        touched.extend(paths.iter().cloned());
                        self.move_objects(plan, a, &paths, v)?;
                    }
                }
                LineSrc::Wrapped(i) => {
                    if let Some(w) = w.as_deref_mut() {
                        let v = display_to_page_vec(&a.tf, d.0, d.1);
                        shift_runs(w, *i, v)?;
                    }
                }
                LineSrc::Carried(i) => {
                    let m = line_matrix(&a.tf, l.left, l.top);
                    let runs: Vec<SegRun> = incoming[*i]
                        .runs
                        .iter()
                        .cloned()
                        .map(|mut r| {
                            r.tm = r.tm.map(|t| mul(&t, &m));
                            r
                        })
                        .collect();
                    plan.containers[c0].appended.push(Out::Block(Segment { prefix: String::new(), reset: true, runs, resume: String::new() }));
                }
            }
        }

        // The rest continues on the next page.
        let first_top = pending.first().map(|l| l.top).unwrap_or(0.0);
        let mut out = Vec::new();
        for l in &pending {
            let runs = match &l.src {
                LineSrc::Runs(runs) => {
                    let mut v = Vec::new();
                    for r in runs {
                        let paths = a.run_paths(*r);
                        if paths.iter().any(|p| touched.contains(p)) {
                            continue;
                        }
                        touched.extend(paths.iter().cloned());
                        v.extend(self.lift_run(plan, a, &paths, l.from)?);
                    }
                    v
                }
                LineSrc::Wrapped(i) => match w.as_deref_mut() {
                    Some(w) => {
                        let runs = std::mem::take(&mut w.lines[*i]);
                        let full = mul(&w.anchor.ctm, &w.to_page);
                        let inv = invert(&line_matrix(&a.tf, l.from.0, l.from.1)).ok_or_else(|| anyhow!("degenerate page"))?;
                        let mut v = Vec::new();
                        for mut r in runs {
                            r.font = match r.font.take() {
                                Some(RunFont::Res(name)) => Some(RunFont::Obj(self.font_object(&plan.containers[w.c].resources, &name)?)),
                                f => f,
                            };
                            r.tm = r.tm.map(|t| mul(&mul(&t, &full), &inv));
                            v.push(r);
                        }
                        v
                    }
                    None => Vec::new(),
                },
                LineSrc::Carried(i) => incoming[*i].runs.clone(),
            };
            out.push(CarryLine { dx: l.left - col_left, dy: l.top - first_top, height: l.bottom - l.top, runs });
        }
        carry.lines = out;
        carry.col_left = col_left;
        Ok(())
    }

    /// Removes a run from the page and returns it as runs in line space at `origin`.
    fn lift_run(&mut self, plan: &mut PagePlan, a: &Analysis, paths: &[ObjPath], origin: (f64, f64)) -> anyhow::Result<Vec<SegRun>> {
        let (c, shows) = self.shows_of(plan, a, paths)?;
        let to_page = a.po.container(&plan.containers[c].path).map(|i| i.to_page).unwrap_or(IDENTITY);
        let inv = invert(&line_matrix(&a.tf, origin.0, origin.1)).ok_or_else(|| anyhow!("degenerate page"))?;
        let mut out = Vec::new();
        for k in shows {
            let sh = plan.containers[c].scan.shows[k].clone();
            let o = a.po.objs.iter().find(|o| o.container() == plan.containers[c].path.as_slice() && o.index() == sh.object);
            let font = self.font_object(&plan.containers[c].resources, &sh.font)?;
            let tm = mul(&mul(&sh.tm(), &sh.ctm), &mul(&to_page, &inv));
            let size = sh.size;
            let stroke = o.is_some_and(|o| o.fill_stroke);
            out.push(SegRun {
                font: Some(RunFont::Obj(font)),
                size,
                color: Some(o.map(|o| o.color).unwrap_or([0, 0, 0])),
                stroke: stroke.then(|| 0.03 * size * x_scale(&tm)),
                render: if stroke { None } else { Some(if o.is_some_and(|o| o.invisible) { 3 } else { 0 }) },
                tz: Some(sh.th),
                tc: Some(sh.tc),
                tw: Some(sh.tw),
                tm: Some(tm),
                items: sh.raw_tokens().into_iter().map(Item::Tok).collect(),
                base_rise: sh.rise,
            });
            let op = &plan.containers[c].scan.ops[sh.op];
            let (s, e) = (op.start, op.end);
            let text = neutralize(&sh)?;
            plan.containers[c].splices.push((s, e, Out::Parts(vec![Part::Raw(text)])));
        }
        Ok(out)
    }

    /// The font object of a resource name (as written), made indirect if needed.
    fn font_object(&mut self, resources: &Dictionary, name: &[u8]) -> anyhow::Result<ObjectId> {
        let decoded = content::decode_name(name);
        let fonts = resources.get(b"Font").ok().map(|o| deref(&self.doc, o)).and_then(|o| o.as_dict().ok()).ok_or_else(|| anyhow!("no fonts in the resources"))?;
        match fonts.get(&decoded) {
            Ok(Object::Reference(id)) => Ok(*id),
            Ok(Object::Dictionary(d)) => {
                let d = d.clone();
                Ok(self.doc.add_object(d))
            }
            _ => bail!("font {} not found", String::from_utf8_lossy(&decoded)),
        }
    }

    /// Adds a blank page after the last one and returns its index.
    pub(super) fn add_page(&mut self) -> anyhow::Result<usize> {
        let last = *self.page_ids.last().ok_or_else(|| anyhow!("no pages"))?;
        let mut media = None;
        let mut id = last;
        for _ in 0..64 {
            let d = self.doc.get_dictionary(id)?;
            if let Ok(m) = d.get(b"MediaBox") {
                media = Some(deref(&self.doc, m).clone());
                break;
            }
            match d.get(b"Parent").and_then(|o| o.as_reference()) {
                Ok(p) => id = p,
                Err(_) => break,
            }
        }
        let parent = self.doc.get_dictionary(last)?.get(b"Parent")?.as_reference()?;
        let content = self.doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Page".to_vec()));
        d.set("Parent", parent);
        d.set("MediaBox", media.unwrap_or_else(|| Object::Array(vec![0.into(), 0.into(), 595.into(), 842.into()])));
        d.set("Resources", Dictionary::new());
        d.set("Contents", content);
        let new = self.doc.add_object(d);
        let kids = self.doc.get_dictionary_mut(parent)?.get_mut(b"Kids")?.as_array_mut()?;
        let at = kids.iter().position(|k| matches!(k, Object::Reference(r) if *r == last)).map(|i| i + 1).unwrap_or(kids.len());
        kids.insert(at, Object::Reference(new));
        let mut node = Some(parent);
        for _ in 0..64 {
            let Some(n) = node else { break };
            let d = self.doc.get_dictionary_mut(n)?;
            let count = d.get(b"Count").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0);
            d.set("Count", count + 1);
            node = d.get(b"Parent").and_then(|o| o.as_reference()).ok();
        }
        self.page_ids.push(new);
        Ok(self.page_ids.len() - 1)
    }

    /// Lines carried past the last page go on new pages.
    pub(super) fn flow_new_pages(&mut self) -> anyhow::Result<()> {
        let mut guard = 0;
        while let Some(carry) = self.carry.take() {
            guard += 1;
            if guard > 200 {
                bail!("the text does not fit on the pages");
            }
            let page = self.add_page()?;
            let id = self.page_ids[page];
            let media: Vec<f64> = self.doc.get_dictionary(id)?.get(b"MediaBox")?.as_array()?.iter().map(|o| fonts::obj_num(&self.doc, o).unwrap_or(0.0)).collect();
            let (x0, y0, x1, y1) = (media[0], media[1], media[2], media[3]);
            let tf = PageTransform::plain(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32);
            let po = PageObjects { objs: Vec::new(), containers: vec![objects::ContainerInfo { path: Vec::new(), to_page: IDENTITY, forms: Vec::new(), images: Vec::new() }], obstacles: Vec::new(), glyph_paths: Vec::new(), images: Vec::new() };
            let a = Analysis { po, runs: Vec::new(), texts: Vec::new(), geoms: Vec::new(), lines: Vec::new(), paras: Vec::new(), tf };
            let mut plan = PagePlan { page, page_id: id, containers: Vec::new() };
            let mut touched = HashSet::new();
            self.flow_in(&mut plan, &a, carry, &mut touched)?;
            self.plans.push(plan);
        }
        Ok(())
    }
}

/// Moves line `i` of a laid-out paragraph by `v` (page space).
fn shift_runs(w: &mut Wrapped, i: usize, v: (f64, f64)) -> anyhow::Result<()> {
    let inv_page = invert(&w.to_page).ok_or_else(|| anyhow!("degenerate form matrix"))?;
    let vc = apply_vec(&inv_page, v.0, v.1);
    let inv_ctm = invert(&w.anchor.ctm).ok_or_else(|| anyhow!("degenerate matrix"))?;
    let vt = apply_vec(&inv_ctm, vc.0, vc.1);
    for r in &mut w.lines[i] {
        r.tm = r.tm.map(|t| mul(&t, &translate(vt.0, vt.1)));
    }
    Ok(())
}
