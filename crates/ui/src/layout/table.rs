//! Automatic table layout (CSS 2.1 §17.5.2, as WebKit's AutoTableLayout does it) for the
//! separated-borders model, with colspan and rowspan.

use super::resolve_calc;
use crate::document::Document;
use crate::dom::NodeId;
use crate::style::Display;
use taffy::prelude::*;
use taffy::{
    LayoutInput, LayoutOutput, LayoutPartialTree, MaybeResolve, ResolveOrZero, RunMode, SizingMode,
};

struct CellSlot {
    node: NodeId,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
}

fn span_attr(doc: &Document, id: NodeId, name: &str) -> usize {
    doc.attribute(id, name)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
        .min(1000)
}

impl Document {
    pub(crate) fn compute_table_layout(&mut self, id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let LayoutInput {
            known_dimensions,
            parent_size,
            available_space,
            run_mode,
            sizing_mode,
            ..
        } = inputs;
        let style = self
            .node(id)
            .style
            .clone()
            .unwrap_or_else(|| self.initial.clone());
        let ts = self.node(id).taffy_style.clone();
        let margin = ts.margin.resolve_or_zero(parent_size.width, resolve_calc);
        let padding = ts.padding.resolve_or_zero(parent_size.width, resolve_calc);
        let border = ts.border.resolve_or_zero(parent_size.width, resolve_calc);
        let mut pb = padding + border;
        let (sx, sy) = if style.border_collapse {
            (0.0, 0.0)
        } else {
            style.border_spacing
        };

        // Rows in visual order: header groups, then bodies and bare rows, then footers.
        let mut groups: Vec<(NodeId, Vec<NodeId>)> = Vec::new();
        let mut header = Vec::new();
        let mut body = Vec::new();
        let mut footer = Vec::new();
        for &c in &self.node(id).layout_children.clone() {
            let Some(cs) = self.node(c).style.clone() else {
                continue;
            };
            if self.node(c).taffy_style.position == taffy::Position::Absolute {
                continue;
            }
            match cs.display {
                Display::TableHeaderGroup => header.push(c),
                Display::TableFooterGroup => footer.push(c),
                _ => body.push(c),
            }
        }
        let mut rows: Vec<NodeId> = Vec::new();
        for g in header.into_iter().chain(body).chain(footer) {
            let d = self
                .node(g)
                .style
                .as_ref()
                .map(|s| s.display)
                .unwrap_or(Display::Block);
            if d == Display::TableRow {
                groups.push((g, vec![g]));
                rows.push(g);
            } else if d.is_row_group() {
                let rs: Vec<NodeId> = self
                    .node(g)
                    .layout_children
                    .iter()
                    .copied()
                    .filter(|r| {
                        self.node(*r)
                            .style
                            .as_ref()
                            .is_some_and(|s| s.display == Display::TableRow)
                    })
                    .collect();
                rows.extend(rs.iter().copied());
                groups.push((g, rs));
            }
        }

        // Slot grid.
        let mut cells: Vec<CellSlot> = Vec::new();
        let mut occupied: Vec<Vec<bool>> = vec![Vec::new(); rows.len()];
        let mut ncols = 0usize;
        for (ri, &r) in rows.iter().enumerate() {
            let mut col = 0usize;
            for &c in &self.node(r).layout_children.clone() {
                while occupied[ri].get(col).copied().unwrap_or(false) {
                    col += 1;
                }
                let colspan = span_attr(self, c, "colspan");
                let rowspan = span_attr(self, c, "rowspan").min(rows.len() - ri);
                for row in occupied.iter_mut().skip(ri).take(rowspan) {
                    if row.len() < col + colspan {
                        row.resize(col + colspan, false);
                    }
                    row[col..col + colspan].fill(true);
                }
                cells.push(CellSlot {
                    node: c,
                    row: ri,
                    col,
                    colspan,
                    rowspan,
                });
                col += colspan;
                ncols = ncols.max(col);
            }
        }

        // The collapsing border model (CSS 2.1 §17.6.2): each border between two cells, or between
        // a cell and the table's edge, is the wider of the two; cells and the table each take half
        // of it, and the table's padding is ignored.
        if style.border_collapse {
            let table_b = style.used_border();
            let side = |doc: &Document, c: NodeId, i: usize| -> f32 {
                doc.node(c)
                    .style
                    .as_ref()
                    .map(|s| s.used_border()[i])
                    .unwrap_or(0.0)
            };
            let mut at: std::collections::HashMap<(usize, usize), NodeId> =
                std::collections::HashMap::new();
            for cell in &cells {
                for r in cell.row..cell.row + cell.rowspan {
                    for c in cell.col..cell.col + cell.colspan {
                        at.insert((r, c), cell.node);
                    }
                }
            }
            let mut edge = [0.0f32; 4];
            for cell in &cells {
                let own = [
                    side(self, cell.node, 0),
                    side(self, cell.node, 1),
                    side(self, cell.node, 2),
                    side(self, cell.node, 3),
                ];
                let neighbour = |r: isize, c: isize| -> Option<NodeId> {
                    if r < 0 || c < 0 {
                        return None;
                    }
                    at.get(&(r as usize, c as usize)).copied()
                };
                let (r0, c0) = (cell.row as isize, cell.col as isize);
                let (r1, c1) = (
                    (cell.row + cell.rowspan) as isize,
                    (cell.col + cell.colspan) as isize,
                );
                let top = match neighbour(r0 - 1, c0) {
                    Some(n) => own[0].max(side(self, n, 2)),
                    None => own[0].max(table_b[0]),
                };
                let right = match neighbour(r0, c1) {
                    Some(n) => own[1].max(side(self, n, 3)),
                    None => own[1].max(table_b[1]),
                };
                let bottom = match neighbour(r1, c0) {
                    Some(n) => own[2].max(side(self, n, 0)),
                    None => own[2].max(table_b[2]),
                };
                let left = match neighbour(r0, c0 - 1) {
                    Some(n) => own[3].max(side(self, n, 1)),
                    None => own[3].max(table_b[3]),
                };
                if r0 == 0 {
                    edge[0] = edge[0].max(top);
                }
                if c1 as usize == ncols {
                    edge[1] = edge[1].max(right);
                }
                if r1 as usize == rows.len() {
                    edge[2] = edge[2].max(bottom);
                }
                if c0 == 0 {
                    edge[3] = edge[3].max(left);
                }
                let half = taffy::Rect {
                    top: taffy::LengthPercentage::length(top / 2.0),
                    right: taffy::LengthPercentage::length(right / 2.0),
                    bottom: taffy::LengthPercentage::length(bottom / 2.0),
                    left: taffy::LengthPercentage::length(left / 2.0),
                };
                let n = self.node_mut(cell.node);
                if n.taffy_style.border != half {
                    n.taffy_style.border = half;
                    n.cache.clear();
                }
            }
            pb = taffy::Rect {
                top: edge[0] / 2.0,
                right: edge[1] / 2.0,
                bottom: edge[2] / 2.0,
                left: edge[3] / 2.0,
            };
        } else {
            // Cells keep their own borders (undo a previous collapse).
            for cell in &cells {
                let b = self
                    .node(cell.node)
                    .style
                    .as_ref()
                    .map(|s| s.used_border())
                    .unwrap_or([0.0; 4]);
                let own = taffy::Rect {
                    top: taffy::LengthPercentage::length(b[0]),
                    right: taffy::LengthPercentage::length(b[1]),
                    bottom: taffy::LengthPercentage::length(b[2]),
                    left: taffy::LengthPercentage::length(b[3]),
                };
                let n = self.node_mut(cell.node);
                if n.taffy_style.border != own {
                    n.taffy_style.border = own;
                    n.cache.clear();
                }
            }
        }

        // Column min and max widths.
        let mut col_min = vec![0.0f32; ncols];
        let mut col_max = vec![0.0f32; ncols];
        let mut col_fixed = vec![false; ncols];
        let measure = |doc: &mut Document, c: NodeId, avail: AvailableSpace| -> f32 {
            doc.compute_child_layout(
                c.taffy(),
                LayoutInput {
                    known_dimensions: Size::NONE,
                    known_dimensions_are_definite: Size {
                        width: true,
                        height: true,
                    },
                    parent_size: Size::NONE,
                    available_space: Size {
                        width: avail,
                        height: AvailableSpace::MaxContent,
                    },
                    sizing_mode: SizingMode::ContentSize,
                    axis: taffy::RequestedAxis::Horizontal,
                    run_mode: RunMode::ComputeSize,
                    vertical_margins_are_collapsible: taffy::Line::FALSE,
                },
            )
            .size
            .width
        };
        let mut cell_mm: Vec<(f32, f32, Option<f32>)> = Vec::with_capacity(cells.len());
        for cell in &cells {
            let mut mn = measure(self, cell.node, AvailableSpace::MinContent);
            let mut mx = measure(self, cell.node, AvailableSpace::MaxContent).max(mn);
            let cts = &self.node(cell.node).taffy_style;
            let cpb = cts.padding.resolve_or_zero(None, resolve_calc)
                + cts.border.resolve_or_zero(None, resolve_calc);
            let fixed = cts.size.width.into_option().map(|w| {
                if cts.box_sizing == taffy::BoxSizing::ContentBox {
                    w + cpb.horizontal_axis_sum()
                } else {
                    w
                }
            });
            if let Some(w) = fixed {
                mn = mn.max(w);
                mx = mn.max(w);
            }
            cell_mm.push((mn, mx, fixed));
        }
        for (cell, &(mn, mx, fixed)) in cells.iter().zip(&cell_mm) {
            if cell.colspan == 1 {
                col_min[cell.col] = col_min[cell.col].max(mn);
                col_max[cell.col] = col_max[cell.col].max(mx);
                if fixed.is_some() {
                    col_fixed[cell.col] = true;
                }
            }
        }
        for (cell, &(mn, mx, _)) in cells.iter().zip(&cell_mm) {
            if cell.colspan > 1 {
                let span = cell.col..cell.col + cell.colspan;
                let inner_spacing = sx * (cell.colspan - 1) as f32;
                let cur_min: f32 = col_min[span.clone()].iter().sum::<f32>() + inner_spacing;
                if mn > cur_min {
                    let extra = mn - cur_min;
                    let total: f32 = col_max[span.clone()].iter().sum();
                    for k in span.clone() {
                        let share = if total > 0.0 {
                            col_max[k] / total
                        } else {
                            1.0 / cell.colspan as f32
                        };
                        col_min[k] += extra * share;
                        col_max[k] = col_max[k].max(col_min[k]);
                    }
                }
                let cur_max: f32 = col_max[span.clone()].iter().sum::<f32>() + inner_spacing;
                if mx > cur_max {
                    let extra = mx - cur_max;
                    let total: f32 = col_max[span.clone()].iter().sum();
                    for k in span.clone() {
                        let share = if total > 0.0 {
                            col_max[k] / total
                        } else {
                            1.0 / cell.colspan as f32
                        };
                        col_max[k] += extra * share;
                    }
                }
            }
        }

        let spacing_w = if ncols > 0 {
            sx * (ncols + 1) as f32
        } else {
            0.0
        };
        let sum_min: f32 = col_min.iter().sum();
        let sum_max: f32 = col_max.iter().sum();
        let extra_w = spacing_w + pb.horizontal_axis_sum();
        // The table's specified width is a border-box width in the UA sheet (box-sizing).
        let style_w = if sizing_mode == SizingMode::InherentSize {
            ts.size
                .width
                .maybe_resolve(parent_size.width, resolve_calc)
                .map(|w| {
                    if ts.box_sizing == taffy::BoxSizing::ContentBox {
                        w + pb.horizontal_axis_sum()
                    } else {
                        w
                    }
                })
        } else {
            None
        };
        let table_w = match known_dimensions.width.or(style_w) {
            Some(w) => w.max(sum_min + extra_w),
            None => match available_space.width {
                AvailableSpace::MinContent => sum_min + extra_w,
                AvailableSpace::MaxContent => sum_max + extra_w,
                AvailableSpace::Definite(a) => {
                    let avail = a - margin.horizontal_axis_sum();
                    (sum_max + extra_w).min(avail).max(sum_min + extra_w)
                }
            },
        };
        let avail_cols = (table_w - extra_w).max(0.0);
        let mut widths = col_min.clone();
        if ncols > 0 {
            if avail_cols >= sum_max {
                widths = col_max.clone();
                let extra = avail_cols - sum_max;
                let auto: Vec<usize> = (0..ncols).filter(|k| !col_fixed[*k]).collect();
                let targets: Vec<usize> = if auto.is_empty() {
                    (0..ncols).collect()
                } else {
                    auto
                };
                let total: f32 = targets.iter().map(|k| col_max[*k]).sum();
                for &k in &targets {
                    let share = if total > 0.0 {
                        col_max[k] / total
                    } else {
                        1.0 / targets.len() as f32
                    };
                    widths[k] += extra * share;
                }
            } else if avail_cols > sum_min {
                let t = (avail_cols - sum_min) / (sum_max - sum_min).max(f32::EPSILON);
                for k in 0..ncols {
                    widths[k] = col_min[k] + (col_max[k] - col_min[k]) * t;
                }
            }
        }
        let mut col_x = vec![0.0f32; ncols + 1];
        let mut x = pb.left + sx;
        for k in 0..ncols {
            col_x[k] = x;
            x += widths[k] + sx;
        }
        col_x[ncols] = x;

        // Row heights.
        let nrows = rows.len();
        let mut row_h = vec![0.0f32; nrows];
        for (ri, &r) in rows.iter().enumerate() {
            let rts = &self.node(r).taffy_style;
            if let Some(h) = rts.size.height.into_option() {
                row_h[ri] = h;
            }
        }
        let cell_w = |cell: &CellSlot| -> f32 {
            widths[cell.col..cell.col + cell.colspan]
                .iter()
                .sum::<f32>()
                + sx * (cell.colspan - 1) as f32
        };
        let mut content_h = vec![0.0f32; cells.len()];
        for (i, cell) in cells.iter().enumerate() {
            let w = cell_w(cell);
            let out = self.compute_child_layout(
                cell.node.taffy(),
                LayoutInput {
                    known_dimensions: Size {
                        width: Some(w),
                        height: None,
                    },
                    known_dimensions_are_definite: Size {
                        width: true,
                        height: true,
                    },
                    parent_size: Size {
                        width: Some(w),
                        height: None,
                    },
                    available_space: Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::MaxContent,
                    },
                    sizing_mode: SizingMode::InherentSize,
                    axis: taffy::RequestedAxis::Vertical,
                    run_mode: RunMode::ComputeSize,
                    vertical_margins_are_collapsible: taffy::Line::FALSE,
                },
            );
            content_h[i] = out.size.height;
            if cell.rowspan == 1 {
                row_h[cell.row] = row_h[cell.row].max(out.size.height);
            }
        }
        for (i, cell) in cells.iter().enumerate() {
            if cell.rowspan > 1 {
                let span = cell.row..cell.row + cell.rowspan;
                let cur: f32 =
                    row_h[span.clone()].iter().sum::<f32>() + sy * (cell.rowspan - 1) as f32;
                if content_h[i] > cur {
                    row_h[span.end - 1] += content_h[i] - cur;
                }
            }
        }
        let spacing_h = if nrows > 0 {
            sy * (nrows + 1) as f32
        } else {
            0.0
        };
        let mut table_h = row_h.iter().sum::<f32>() + spacing_h + pb.vertical_axis_sum();
        let style_h = ts
            .size
            .height
            .maybe_resolve(parent_size.height, resolve_calc)
            .map(|h| {
                if ts.box_sizing == taffy::BoxSizing::ContentBox {
                    h + pb.vertical_axis_sum()
                } else {
                    h
                }
            });
        if let Some(h) = known_dimensions.height.or(style_h)
            && h > table_h
            && nrows > 0
        {
            let extra = h - table_h;
            let total: f32 = row_h.iter().sum();
            for rh in row_h.iter_mut() {
                *rh += if total > 0.0 {
                    extra * *rh / total
                } else {
                    extra / nrows as f32
                };
            }
            table_h = h;
        }
        let size = Size {
            width: table_w,
            height: table_h,
        };

        if run_mode == RunMode::PerformLayout {
            let mut row_y = vec![0.0f32; nrows];
            let mut y = pb.top + sy;
            for ri in 0..nrows {
                row_y[ri] = y;
                y += row_h[ri] + sy;
            }
            // Sections span the table's content box; the first one also takes the spacing above
            // its rows, and each row is followed by the vertical spacing. Rows span the full width.
            let content_left = pb.left;
            let content_w = (table_w - pb.horizontal_axis_sum()).max(0.0);
            let mut section_top = pb.top;
            for (g, rs) in &groups {
                if rs.is_empty() {
                    continue;
                }
                let first = rows.iter().position(|r| r == &rs[0]).unwrap_or(0);
                let last = rows
                    .iter()
                    .position(|r| r == rs.last().unwrap())
                    .unwrap_or(first);
                let section_bottom = row_y[last] + row_h[last] + sy;
                let is_row = self
                    .node(*g)
                    .style
                    .as_ref()
                    .is_some_and(|s| s.display == Display::TableRow);
                if is_row {
                    self.store_layout(
                        *g,
                        content_left,
                        row_y[first],
                        Size {
                            width: content_w,
                            height: row_h[first],
                        },
                        content_w,
                    );
                } else {
                    self.store_layout(
                        *g,
                        content_left,
                        section_top,
                        Size {
                            width: content_w,
                            height: section_bottom - section_top,
                        },
                        content_w,
                    );
                    for (k, r) in rs.iter().enumerate() {
                        let ri = first + k;
                        self.store_layout(
                            *r,
                            0.0,
                            row_y[ri] - section_top,
                            Size {
                                width: content_w,
                                height: row_h[ri],
                            },
                            content_w,
                        );
                    }
                }
                section_top = section_bottom;
            }
            let inner_left = content_left;
            for (i, cell) in cells.iter().enumerate() {
                let w = cell_w(cell);
                let h: f32 = row_h[cell.row..cell.row + cell.rowspan].iter().sum::<f32>()
                    + sy * (cell.rowspan - 1) as f32;
                // Vertical alignment of the cell's content: extra space goes above it.
                let valign = self
                    .node(cell.node)
                    .style
                    .as_ref()
                    .map(|s| s.vertical_align.clone());
                let free = (h - content_h[i]).max(0.0);
                let offset = match valign {
                    Some(crate::style::VerticalAlign::Middle) => (free / 2.0).floor(),
                    Some(crate::style::VerticalAlign::Bottom) => free,
                    _ => 0.0,
                };
                let saved = self.node(cell.node).taffy_style.padding.top;
                if offset > 0.0 {
                    let cur = self
                        .node(cell.node)
                        .taffy_style
                        .padding
                        .top
                        .resolve_or_zero(Some(w), resolve_calc);
                    self.node_mut(cell.node).taffy_style.padding.top =
                        taffy::LengthPercentage::length(cur + offset);
                    self.node_mut(cell.node).cache.clear();
                }
                let out = self.compute_child_layout(
                    cell.node.taffy(),
                    LayoutInput {
                        known_dimensions: Size {
                            width: Some(w),
                            height: Some(h),
                        },
                        known_dimensions_are_definite: Size {
                            width: true,
                            height: true,
                        },
                        parent_size: Size {
                            width: Some(w),
                            height: Some(h),
                        },
                        available_space: Size {
                            width: AvailableSpace::Definite(w),
                            height: AvailableSpace::Definite(h),
                        },
                        sizing_mode: SizingMode::InherentSize,
                        axis: taffy::RequestedAxis::Both,
                        run_mode: RunMode::PerformLayout,
                        vertical_margins_are_collapsible: taffy::Line::FALSE,
                    },
                );
                if offset > 0.0 {
                    self.node_mut(cell.node).taffy_style.padding.top = saved;
                    self.node_mut(cell.node).cache.clear();
                }
                // Cells are positioned relative to their row.
                let row = rows[cell.row];
                let row_origin_y = row_y[cell.row];
                self.store_layout(cell.node, col_x[cell.col] - inner_left, 0.0, out.size, w);
                let _ = row_origin_y;
                self.node_mut(cell.node).layout_parent = Some(row);
            }
            for &c in &self.node(id).layout_children.clone() {
                if self.node(c).taffy_style.position == taffy::Position::Absolute {
                    super::layout_abspos(self, id, c, size, border);
                }
            }
            let n = self.node_mut(id);
            n.layout.scrollable_overflow_rect = taffy::Rect {
                left: 0.0,
                top: 0.0,
                right: size.width,
                bottom: size.height,
            };
        }
        let first_baseline = None;
        LayoutOutput {
            size,
            scrollable_overflow_rect: taffy::Rect {
                left: 0.0,
                top: 0.0,
                right: size.width,
                bottom: size.height,
            },
            baselines: taffy::Baselines {
                first: first_baseline,
                last: None,
            },
            top_margin: taffy::CollapsibleMarginSet::ZERO,
            bottom_margin: taffy::CollapsibleMarginSet::ZERO,
            margins_can_collapse_through: false,
        }
    }
}
