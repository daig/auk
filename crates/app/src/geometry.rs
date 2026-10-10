//! Where a tab's panes go: the split tree (§4.3) laid out in a window's content area, zoom, the
//! dividers between panes, and which pane lies in a direction from another.

use auk_mirror::PaneId;
use auk_mirror::layout::{Node, SplitDir, Tab};

/// A rectangle in points, from the window's top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A tab's layout in `area`: each visible pane's rectangle and the dividers between them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabLayout {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Rect>,
}

impl TabLayout {
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }

    /// The pane under a point.
    pub fn pane_at(&self, x: f64, y: f64) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(p, _)| *p)
    }
}

/// Lays out `tab` in `area`, with `divider` points between split children. A zoomed tab shows
/// only its zoomed pane, over the whole area.
pub fn layout_tab(tab: &Tab, area: Rect, divider: f64) -> TabLayout {
    let mut out = TabLayout::default();
    match tab.zoom.filter(|z| tab.root.contains(*z)) {
        Some(zoomed) => out.panes.push((zoomed, area)),
        None => layout_node(&tab.root, area, divider, &mut out),
    }
    out
}

/// A split's first child area, its divider and its second child area, honouring its ratio.
fn split_rects(dir: SplitDir, ratio: u16, area: Rect, divider: f64) -> (Rect, Rect, Rect) {
    let share = f64::from(ratio.clamp(1, 999)) / 1000.0;
    match dir {
        SplitDir::Right => {
            let avail = (area.w - divider).max(0.0);
            let w1 = (avail * share).round();
            let a = Rect::new(area.x, area.y, w1, area.h);
            let d = Rect::new(area.x + w1, area.y, divider, area.h);
            let b = Rect::new(d.right(), area.y, (avail - w1).max(0.0), area.h);
            (a, d, b)
        }
        SplitDir::Down => {
            let avail = (area.h - divider).max(0.0);
            let h1 = (avail * share).round();
            let a = Rect::new(area.x, area.y, area.w, h1);
            let d = Rect::new(area.x, area.y + h1, area.w, divider);
            let b = Rect::new(area.x, d.bottom(), area.w, (avail - h1).max(0.0));
            (a, d, b)
        }
    }
}

fn layout_node(node: &Node, area: Rect, divider: f64, out: &mut TabLayout) {
    match node {
        Node::Pane { pane } => out.panes.push((*pane, area)),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let (a, d, b) = split_rects(*dir, *ratio, area, divider);
            layout_node(first, a, divider, out);
            out.dividers.push(d);
            layout_node(second, b, divider, out);
        }
    }
}

/// The area of the node at `path` (as [`Node::divider`] gives it) of a tree laid out in `area`.
pub fn node_area(root: &Node, area: Rect, divider: f64, path: &[bool]) -> Option<Rect> {
    match (path.split_first(), root) {
        (None, _) => Some(area),
        (
            Some((&second_side, rest)),
            Node::Split {
                dir,
                ratio,
                first,
                second,
            },
        ) => {
            let (a, _, b) = split_rects(*dir, *ratio, area, divider);
            if second_side {
                node_area(second, b, divider, rest)
            } else {
                node_area(first, a, divider, rest)
            }
        }
        (Some(_), Node::Pane { .. }) => None,
    }
}

/// A direction for moving focus between panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The pane nearest `from` in `dir`: among panes wholly on that side, the one whose span along
/// the other axis overlaps most, then the closest.
pub fn neighbor(layout: &TabLayout, from: PaneId, dir: Direction) -> Option<PaneId> {
    let a = layout.rect(from)?;
    const EPS: f64 = 0.5;
    let overlap = |lo1: f64, hi1: f64, lo2: f64, hi2: f64| (hi1.min(hi2) - lo1.max(lo2)).max(0.0);
    layout
        .panes
        .iter()
        .filter(|(p, _)| *p != from)
        .filter_map(|(p, b)| {
            let (beside, distance, shared) = match dir {
                Direction::Left => (
                    b.right() <= a.x + EPS,
                    a.x - b.right(),
                    overlap(a.y, a.bottom(), b.y, b.bottom()),
                ),
                Direction::Right => (
                    b.x >= a.right() - EPS,
                    b.x - a.right(),
                    overlap(a.y, a.bottom(), b.y, b.bottom()),
                ),
                Direction::Up => (
                    b.bottom() <= a.y + EPS,
                    a.y - b.bottom(),
                    overlap(a.x, a.right(), b.x, b.right()),
                ),
                Direction::Down => (
                    b.y >= a.bottom() - EPS,
                    b.y - a.bottom(),
                    overlap(a.x, a.right(), b.x, b.right()),
                ),
            };
            (beside && shared > 0.0).then_some((*p, distance, shared))
        })
        .min_by(|x, y| {
            x.1.partial_cmp(&y.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(y.2.partial_cmp(&x.2).unwrap_or(std::cmp::Ordering::Equal))
        })
        .map(|(p, _, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(root: Node, zoom: Option<PaneId>) -> Tab {
        Tab {
            id: 1,
            title: None,
            root,
            zoom,
        }
    }

    fn split(dir: SplitDir, a: Node, b: Node) -> Node {
        Node::Split {
            dir,
            ratio: 500,
            first: Box::new(a),
            second: Box::new(b),
        }
    }

    fn pane(id: PaneId) -> Node {
        Node::Pane { pane: id }
    }

    #[test]
    fn splits_share_the_area_around_dividers() {
        let t = tab(
            split(
                SplitDir::Right,
                pane(1),
                split(SplitDir::Down, pane(2), pane(3)),
            ),
            None,
        );
        let l = layout_tab(&t, Rect::new(0.0, 30.0, 801.0, 401.0), 1.0);
        assert_eq!(l.rect(1), Some(Rect::new(0.0, 30.0, 400.0, 401.0)));
        assert_eq!(l.rect(2), Some(Rect::new(401.0, 30.0, 400.0, 200.0)));
        assert_eq!(l.rect(3), Some(Rect::new(401.0, 231.0, 400.0, 200.0)));
        assert_eq!(l.dividers.len(), 2);
        assert_eq!(l.pane_at(500.0, 300.0), Some(3));
        assert_eq!(l.pane_at(400.5, 300.0), None);
    }

    #[test]
    fn ratios_size_the_sides_and_node_areas_follow_paths() {
        let root = Node::Split {
            dir: SplitDir::Right,
            ratio: 250,
            first: Box::new(pane(1)),
            second: Box::new(Node::Split {
                dir: SplitDir::Down,
                ratio: 750,
                first: Box::new(pane(2)),
                second: Box::new(pane(3)),
            }),
        };
        let area = Rect::new(0.0, 0.0, 801.0, 401.0);
        let l = layout_tab(&tab(root.clone(), None), area, 1.0);
        assert_eq!(l.rect(1), Some(Rect::new(0.0, 0.0, 200.0, 401.0)));
        assert_eq!(l.rect(2), Some(Rect::new(201.0, 0.0, 600.0, 300.0)));
        assert_eq!(l.rect(3), Some(Rect::new(201.0, 301.0, 600.0, 100.0)));
        assert_eq!(node_area(&root, area, 1.0, &[]), Some(area));
        assert_eq!(
            node_area(&root, area, 1.0, &[true]),
            Some(Rect::new(201.0, 0.0, 600.0, 401.0))
        );
        assert_eq!(node_area(&root, area, 1.0, &[true, true]), l.rect(3));
        assert_eq!(node_area(&root, area, 1.0, &[false, true]), None);
    }

    #[test]
    fn zoom_shows_one_pane() {
        let t = tab(split(SplitDir::Right, pane(1), pane(2)), Some(2));
        let area = Rect::new(0.0, 0.0, 100.0, 100.0);
        let l = layout_tab(&t, area, 1.0);
        assert_eq!(l.panes, vec![(2, area)]);
        assert!(l.dividers.is_empty());
        // A stale zoom (the pane left the tab) is ignored.
        let t = tab(split(SplitDir::Right, pane(1), pane(2)), Some(9));
        assert_eq!(layout_tab(&t, area, 1.0).panes.len(), 2);
    }

    #[test]
    fn neighbors_by_direction() {
        let t = tab(
            split(
                SplitDir::Right,
                pane(1),
                split(SplitDir::Down, pane(2), pane(3)),
            ),
            None,
        );
        let l = layout_tab(&t, Rect::new(0.0, 0.0, 801.0, 401.0), 1.0);
        assert_eq!(neighbor(&l, 1, Direction::Right), Some(2));
        assert_eq!(neighbor(&l, 3, Direction::Left), Some(1));
        assert_eq!(neighbor(&l, 2, Direction::Down), Some(3));
        assert_eq!(neighbor(&l, 3, Direction::Up), Some(2));
        assert_eq!(neighbor(&l, 1, Direction::Left), None);
        assert_eq!(neighbor(&l, 2, Direction::Right), None);
    }
}
