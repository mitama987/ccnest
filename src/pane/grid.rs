use crate::pane::PaneId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal, // stacked top/bottom
    Vertical,   // side-by-side left/right
}

#[derive(Debug, Clone)]
pub enum Layout {
    Leaf(PaneId),
    Split {
        dir: SplitDir,
        ratio: f32,
        a: Box<Layout>,
        b: Box<Layout>,
    },
}

impl Layout {
    pub fn split(self, target: PaneId, dir: Direction, new_pane: PaneId) -> Layout {
        let split_dir = match dir {
            Direction::Left | Direction::Right => SplitDir::Vertical,
            Direction::Up | Direction::Down => SplitDir::Horizontal,
        };
        self.transform_leaf(target, |_leaf| Layout::Split {
            dir: split_dir,
            ratio: 0.5,
            a: Box::new(Layout::Leaf(target)),
            b: Box::new(Layout::Leaf(new_pane)),
        })
    }

    fn transform_leaf<F>(self, target: PaneId, f: F) -> Layout
    where
        F: FnOnce(PaneId) -> Layout + Copy,
    {
        match self {
            Layout::Leaf(id) if id == target => f(id),
            Layout::Leaf(id) => Layout::Leaf(id),
            Layout::Split { dir, ratio, a, b } => Layout::Split {
                dir,
                ratio,
                a: Box::new(a.transform_leaf(target, f)),
                b: Box::new(b.transform_leaf(target, f)),
            },
        }
    }

    pub fn close(self, target: PaneId) -> Option<Layout> {
        match self {
            Layout::Leaf(id) if id == target => None,
            leaf @ Layout::Leaf(_) => Some(leaf),
            Layout::Split { dir, ratio, a, b } => {
                let a2 = a.close(target);
                let b2 = b.close(target);
                match (a2, b2) {
                    (None, None) => None,
                    (Some(x), None) | (None, Some(x)) => Some(x),
                    (Some(a), Some(b)) => Some(Layout::Split {
                        dir,
                        ratio,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                }
            }
        }
    }

    pub fn first_leaf(&self) -> Option<PaneId> {
        match self {
            Layout::Leaf(id) => Some(*id),
            Layout::Split { a, .. } => a.first_leaf(),
        }
    }

    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.walk(&mut |id| out.push(id));
        out
    }

    pub fn walk<F: FnMut(PaneId)>(&self, f: &mut F) {
        match self {
            Layout::Leaf(id) => f(*id),
            Layout::Split { a, b, .. } => {
                a.walk(f);
                b.walk(f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_replaces_leaf() {
        let l = Layout::Leaf(1);
        let l = l.split(1, Direction::Right, 2);
        match l {
            Layout::Split { dir, a, b, .. } => {
                assert_eq!(dir, SplitDir::Vertical);
                assert_eq!(a.first_leaf(), Some(1));
                assert_eq!(b.first_leaf(), Some(2));
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn close_collapses_split() {
        let l = Layout::Leaf(1).split(1, Direction::Down, 2);
        let l = l.close(2).expect("still present");
        assert!(matches!(l, Layout::Leaf(1)));
    }

    #[test]
    fn close_last_leaf_returns_none() {
        let l = Layout::Leaf(1);
        assert!(l.close(1).is_none());
    }
}
