//! Pane layout tree: up to four panes arranged by recursive horizontal / vertical splits.

use eframe::egui::{pos2, Rect};
use serde::{Deserialize, Serialize};

pub const MAX_PANES: usize = 4;

#[derive(Clone, Serialize, Deserialize)]
pub enum Node {
    Leaf(usize), // pane id
    /// `horiz`: a | b side by side, otherwise a above b.
    Split { horiz: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

pub struct Divider {
    pub path: Vec<bool>, // route to the split node (false = a, true = b)
    pub rect: Rect,      // hit area
    pub horiz: bool,
    pub parent: Rect,    // area the split divides (to turn pointer position into a ratio)
}

impl Node {
    pub fn layout(&self, rect: Rect, path: &mut Vec<bool>, panes: &mut Vec<(usize, Rect)>, divs: &mut Vec<Divider>) {
        match self {
            Node::Leaf(id) => panes.push((*id, rect)),
            Node::Split { horiz, ratio, a, b } => {
                let (ra, rb, hit) = if *horiz {
                    let x = rect.left() + (rect.width() * ratio).round();
                    (
                        Rect::from_min_max(rect.min, pos2(x - 2.0, rect.bottom())),
                        Rect::from_min_max(pos2(x + 3.0, rect.top()), rect.max),
                        Rect::from_min_max(pos2(x - 3.0, rect.top()), pos2(x + 3.0, rect.bottom())),
                    )
                } else {
                    let y = rect.top() + (rect.height() * ratio).round();
                    (
                        Rect::from_min_max(rect.min, pos2(rect.right(), y - 2.0)),
                        Rect::from_min_max(pos2(rect.left(), y + 3.0), rect.max),
                        Rect::from_min_max(pos2(rect.left(), y - 3.0), pos2(rect.right(), y + 3.0)),
                    )
                };
                divs.push(Divider { path: path.clone(), rect: hit, horiz: *horiz, parent: rect });
                path.push(false);
                a.layout(ra, path, panes, divs);
                path.pop();
                path.push(true);
                b.layout(rb, path, panes, divs);
                path.pop();
            }
        }
    }

    pub fn node_mut(&mut self, path: &[bool]) -> &mut Node {
        match (self, path.split_first()) {
            (n, None) => n,
            (Node::Split { a, b, .. }, Some((&first, rest))) => {
                if !first {
                    a.node_mut(rest)
                } else {
                    b.node_mut(rest)
                }
            }
            (n, _) => n,
        }
    }

    pub fn leaves(&self, out: &mut Vec<usize>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    /// Replaces leaf `target` with a split holding `target` and the new pane.
    pub fn split_leaf(&mut self, target: usize, new_id: usize, horiz: bool, new_first: bool) -> bool {
        match self {
            Node::Leaf(id) => {
                let id = *id;
                if id != target {
                    return false;
                }
                let (old, new) = (Node::Leaf(id), Node::Leaf(new_id));
                let (a, b) = if new_first { (new, old) } else { (old, new) };
                *self = Node::Split { horiz, ratio: 0.5, a: Box::new(a), b: Box::new(b) };
                true
            }
            Node::Split { a, b, .. } => a.split_leaf(target, new_id, horiz, new_first) || b.split_leaf(target, new_id, horiz, new_first),
        }
    }

    /// Removes leaf `target`; its sibling takes the parent's place.
    pub fn remove_leaf(&mut self, target: usize) -> bool {
        if let Node::Split { a, b, .. } = self {
            if matches!(**a, Node::Leaf(i) if i == target) {
                let keep = std::mem::replace(&mut **b, Node::Leaf(0));
                *self = keep;
                return true;
            }
            if matches!(**b, Node::Leaf(i) if i == target) {
                let keep = std::mem::replace(&mut **a, Node::Leaf(0));
                *self = keep;
                return true;
            }
            return a.remove_leaf(target) || b.remove_leaf(target);
        }
        false
    }
}
