//! Tables: cells found by the converter's layout analysis, and editing the
//! text of a cell inside its box.

use super::*;

/// A table on a page.
#[derive(Clone, Debug)]
pub struct TableInfo {
    /// Box as fractions of the displayed page (0..1, origin top-left).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub rows: usize,
    pub cols: usize,
    /// Cells in reading order (row by row).
    pub cells: Vec<TableCell>,
}

/// A cell of a table.
#[derive(Clone, Debug)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub row_span: usize,
    pub col_span: usize,
    /// Box as fractions of the displayed page.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Indices into [`PageText::items`].
    pub items: Vec<usize>,
    /// The cell's text (its paragraphs joined by `\n`).
    pub text: String,
}

/// The runs of a cell as one paragraph (its paragraphs joined by `\n`),
/// with the lines that the paragraph refers to.
pub(super) fn cell_para(a: &Analysis, runs: &[usize]) -> Option<(Vec<Line>, Para)> {
    let mut runs = runs.to_vec();
    runs.sort_unstable();
    runs.dedup();
    runs.retain(|r| !a.texts[*r].trim().is_empty());
    if runs.is_empty() {
        return None;
    }
    let containers: Vec<Vec<usize>> = a.runs.iter().map(|g| a.po.objs[g[0]].container().to_vec()).collect();
    let lines = group_lines_of(&runs, &a.texts, &a.geoms, &containers);
    let mut paras = group_paragraphs(&lines, &a.geoms, &containers, &a.texts);
    paras.sort_by(|x, y| x.top.total_cmp(&y.top));
    let mut it = paras.into_iter();
    let mut p = it.next()?;
    for q in it {
        let off = p.text.chars().count() + 1;
        p.text.push('\n');
        p.src.push(Src::Join);
        p.text.push_str(&q.text);
        p.src.extend(q.src);
        p.hyphens.extend(q.hyphens.into_iter().map(|(pos, r, i)| (pos + off, r, i)));
        p.lines.extend(q.lines);
        p.left = p.left.min(q.left);
        p.right = p.right.max(q.right);
        p.top = p.top.min(q.top);
        p.bottom = p.bottom.max(q.bottom);
    }
    Some((lines, p))
}

/// Data tables on a page, with the runs in each cell.
pub(super) fn find_tables(doc: &PdfDocument, page: &PdfPage, index: usize, a: &Analysis, pt: &PageText) -> Vec<TableInfo> {
    // The converter works in the orientation that shows the text upright.
    if PageTransform::for_text(page).rotation_degrees() != a.tf.rotation_degrees() {
        return Vec::new();
    }
    let opts = crate::extract::ExtractOptions { extract_images: false, rasterize_vector_graphics: false, vector_shapes: false, ..Default::default() };
    let Ok((raw, _)) = crate::extract::extract_page(doc, page, index, &opts) else { return Vec::new() };
    let layout = crate::layout::analyse_page(&raw, &Default::default());
    let mut tables = Vec::new();
    fn walk<'t>(blocks: &'t [crate::model::Block], out: &mut Vec<&'t crate::model::Table>) {
        for b in blocks {
            if let crate::model::Block::Table(t) = b {
                if t.layout_only {
                    for row in &t.rows {
                        for c in &row.cells {
                            walk(&c.blocks, out);
                        }
                    }
                } else {
                    out.push(t);
                }
            }
        }
    }
    walk(&layout.blocks, &mut tables);
    let (pw, ph) = (a.tf.width().max(1.0), a.tf.height().max(1.0));
    let mut out = Vec::new();
    for t in tables {
        let b = t.bbox;
        let total_w: f32 = t.col_widths.iter().sum();
        let total_h: f32 = t.rows.iter().map(|r| r.height).sum();
        if total_w <= 0.0 || t.rows.is_empty() {
            continue;
        }
        let sx = b.width() / total_w;
        let sy = if total_h > 0.0 { b.height() / total_h } else { 1.0 };
        let mut xs = vec![0.0f32];
        for w in &t.col_widths {
            xs.push(xs.last().unwrap() + w * sx);
        }
        let col_x = |c: usize| if t.bidi { (b.x1 - xs[c + 1], b.x1 - xs[c]) } else { (b.x0 + xs[c], b.x0 + xs[c + 1]) };
        let mut ys = vec![b.y0];
        for r in &t.rows {
            ys.push(ys.last().unwrap() + if total_h > 0.0 { r.height * sy } else { b.height() / t.rows.len() as f32 });
        }
        let mut cells: Vec<TableCell> = Vec::new();
        // Cell started by a vertical merge, per grid column.
        let mut merge_from: HashMap<usize, usize> = HashMap::new();
        for (ri, row) in t.rows.iter().enumerate() {
            let mut col = 0;
            for c in &row.cells {
                let span = c.col_span.max(1);
                let last = (col + span - 1).min(t.col_widths.len() - 1);
                let (xa, _) = col_x(col);
                let (_, xb) = col_x(last);
                let (x0, x1) = (xa.min(col_x(last).0), xb.max(col_x(col).1));
                if c.v_merge_continue {
                    if let Some(&k) = merge_from.get(&col) {
                        cells[k].row_span += 1;
                        cells[k].h = (ys[ri + 1] - cells[k].y * ph) / ph;
                    }
                } else {
                    if c.v_merge_start {
                        merge_from.insert(col, cells.len());
                    } else {
                        merge_from.remove(&col);
                    }
                    cells.push(TableCell { row: ri, col, row_span: 1, col_span: span, x: x0 / pw, y: ys[ri] / ph, w: (x1 - x0) / pw, h: (ys[ri + 1] - ys[ri]) / ph, items: Vec::new(), text: String::new() });
                }
                col += span;
            }
        }
        // Runs into cells by their centre.
        let mut runs_of: Vec<Vec<usize>> = vec![Vec::new(); cells.len()];
        for (i, item) in pt.items.iter().enumerate() {
            if item.outlined {
                continue;
            }
            let (cx, cy) = (item.x + item.w / 2.0, item.y + item.h / 2.0);
            if let Some(k) = cells.iter().position(|c| cx >= c.x && cx <= c.x + c.w && cy >= c.y && cy <= c.y + c.h) {
                cells[k].items.push(i);
                if let Some(r) = a.run_of(&item.objects[0]) {
                    runs_of[k].push(r);
                }
            }
        }
        if cells.iter().filter(|c| !c.items.is_empty()).count() < 2 {
            continue;
        }
        for (k, c) in cells.iter_mut().enumerate() {
            if let Some((_, p)) = cell_para(a, &runs_of[k]) {
                c.text = p.text;
            }
        }
        out.push(TableInfo { x: b.x0 / pw, y: b.y0 / ph, w: b.width() / pw, h: b.height() / ph, rows: t.rows.len(), cols: t.col_widths.len(), cells });
    }
    out
}

impl Session<'_, '_> {
    /// Rewrites the text of a table cell (box in display points) inside the cell.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn cell(&mut self, plan: &mut PagePlan, a: &Analysis, cell: [f64; 4], objects: &[ObjPath], text: &str, fit: bool, touched: &mut HashSet<ObjPath>) -> anyhow::Result<()> {
        let runs: Vec<usize> = objects.iter().filter_map(|o| a.run_of(o)).collect();
        let Some((lines, mut para)) = cell_para(a, &runs) else {
            return self.empty_cell(plan, a, cell, text);
        };
        for r in para.lines.iter().flat_map(|l| lines[*l].runs.iter()) {
            touched.extend(a.run_paths(*r));
        }
        if text.trim().is_empty() {
            let paths: Vec<ObjPath> = para.lines.iter().flat_map(|l| lines[*l].runs.iter()).flat_map(|r| a.run_paths(*r)).collect();
            return self.delete(plan, a, &paths);
        }
        // Alignment and padding inside the cell.
        let cw = (cell[2] - cell[0]).max(1.0);
        let (gl, gr) = (para.left - cell[0], cell[2] - para.right);
        let rtl = objects::first_strong_rtl(&para.text);
        let align = if para.lines.len() > 1 && matches!(para.align, Align::Justify | Align::Center | Align::Right) {
            para.align
        } else if (gl - gr).abs() <= 0.12 * cw && gl > 0.08 * cw {
            Align::Center
        } else if rtl || (gr < gl && gr < 0.1 * cw && gl > 0.2 * cw) {
            Align::Right
        } else {
            Align::Left
        };
        para.align = align;
        let pad = match align {
            Align::Left | Align::Justify if !rtl => gl,
            Align::Center => gl.min(gr),
            _ => gr,
        }
        .clamp(1.0, 0.25 * cw);
        let frame = [cell[0] + pad, cell[1], cell[2] - pad, cell[3] - pad.min(2.0)];
        let w = self.layout_paragraph(plan, a, &lines, &para, text, &Wrap { fit, frame: Some(frame), ..Default::default() })?;
        if w.overfull {
            self.ctx.note(format!("Page {}: the text is taller than its table cell.", plan.page + 1));
        }
        self.place_wrapped(plan, &w)
    }

    /// Text for an empty cell, in the style of the nearest text.
    fn empty_cell(&mut self, plan: &mut PagePlan, a: &Analysis, cell: [f64; 4], text: &str) -> anyhow::Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let (cx, cy) = ((cell[0] + cell[2]) / 2.0, (cell[1] + cell[3]) / 2.0);
        let near = (0..a.runs.len())
            .filter(|r| !a.texts[*r].trim().is_empty() && a.geoms[*r].horizontal && !a.po.objs[a.runs[*r][0]].invisible)
            .min_by(|x, y| {
                let d = |r: usize| {
                    let g = &a.geoms[r];
                    let (gx, gy) = ((g.left + g.right) / 2.0, (g.top + g.bottom) / 2.0);
                    // Same row or column first.
                    let k = if (g.top..=g.bottom).contains(&cy) || (g.left..=g.right).contains(&cx) { 0.3 } else { 1.0 };
                    k * ((gx - cx).powi(2) + (gy - cy).powi(2)).sqrt()
                };
                d(*x).total_cmp(&d(*y))
            });
        let lines: Vec<&str> = text.trim_matches('\n').split('\n').collect();
        let Some(r) = near else {
            let size = ((cell[3] - cell[1]) * 0.6).clamp(6.0, 11.0);
            let style = NewTextStyle::default();
            for (i, l) in lines.iter().enumerate() {
                let base = cell[1] + 2.0 + 0.8 * size + i as f64 * 1.2 * size;
                self.draw_text(plan, a, cell[0] + 2.0, base, l, size, [0, 0, 0], &style, Some(cell[2] - cell[0] - 4.0))?;
            }
            return Ok(());
        };
        let o = &a.po.objs[a.runs[r][0]];
        let g = &a.geoms[r];
        let size = g.size.max(1.0);
        let pad = (0.3 * size).min(0.2 * (cell[2] - cell[0]));
        // Text lines centred vertically in the cell.
        let block = lines.len() as f64 * 1.2 * size;
        let first_base = ((cell[1] + cell[3]) / 2.0 - block / 2.0 + 0.85 * size).max(cell[1] + 0.9 * size);
        if !o.container().is_empty() {
            // Text inside a form: write with an installed font like it.
            let base = fonts::strip_subset(&o.font).to_string();
            let family = if base.is_empty() { crate::fonts::analyse_font_name(&o.font).family } else { crate::fonts::analyse_font_name(&base).family };
            let style = NewTextStyle { family, bold: o.bold_flag || o.fill_stroke, italic: o.italic_flag || o.skewed() };
            for (i, l) in lines.iter().enumerate() {
                self.draw_text(plan, a, cell[0] + pad, first_base + i as f64 * 1.2 * size, l, size, o.color, &style, Some(cell[2] - cell[0] - 2.0 * pad))?;
            }
            return Ok(());
        }
        let c = self.container(plan, &a.po, &[])?;
        let k = plan.containers[c].show_of(o.index()).ok_or_else(|| anyhow!("text object not in content"))?;
        let sh = plan.containers[c].scan.shows[k].clone();
        for (i, l) in lines.iter().enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            let (fb, pieces) = self.pieces_for(plan.page, plan, c, &sh, o, l)?;
            let width = pieces_advance(&pieces, size, 0.0, 0.0, 1.0);
            let base = first_base + i as f64 * 1.2 * size;
            let room = cell[2] - cell[0] - 2.0 * pad;
            let x = if a.geoms[r].left > cell[0] + 0.5 * (cell[2] - cell[0]) { cell[2] - pad - width.min(room) } else { cell[0] + pad };
            let m = flow_line_matrix(&a.tf, x, base);
            let font = match fb {
                Some(f) => RunFont::Fallback(f),
                None => RunFont::Res(sh.font.clone()),
            };
            let tz = (width > room && room > 0.0).then(|| (room / width).max(0.5));
            let run = SegRun { font: Some(font), size, color: Some(o.color), render: Some(0), tz, tm: Some(m), items: pieces.into_iter().map(Item::Piece).collect(), ..Default::default() };
            plan.containers[c].appended.push(Out::Block(Segment { prefix: String::new(), reset: true, runs: vec![run], resume: String::new() }));
        }
        Ok(())
    }
}

/// Text space with its origin at a baseline point of the displayed page
/// (1 unit = 1 display point, y up) → page user space.
fn flow_line_matrix(tf: &PageTransform, x: f64, y: f64) -> Mat {
    let o = tf.to_user(x as f32, y as f32);
    let r = tf.to_user(x as f32 + 1.0, y as f32);
    let u = tf.to_user(x as f32, y as f32 - 1.0);
    [(r.0 - o.0) as f64, (r.1 - o.1) as f64, (u.0 - o.0) as f64, (u.1 - o.1) as f64, o.0 as f64, o.1 as f64]
}
