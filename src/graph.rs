//! Lays out a commit list as a coloured branch graph ("tree view").
//!
//! Commits arrive newest first in topological order. Every row gets the column
//! of its dot plus the line segments that cross it, so the UI only has to paint
//! each visible row on its own.

use git2::Oid;

use crate::git::CommitInfo;

/// A line segment inside one row. Columns are lane indices; `color` is a palette index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub from_col: usize,
    pub to_col: usize,
    pub color: usize,
}

#[derive(Clone, Debug, Default)]
pub struct GraphRow {
    /// Column of this commit's dot.
    pub col: usize,
    /// Palette index of this commit's dot.
    pub color: usize,
    /// Lanes that go straight through the row (top edge to bottom edge).
    pub pass: Vec<Segment>,
    /// Lines from the top edge into the dot (children above this commit).
    pub incoming: Vec<Segment>,
    /// Lines from the dot to the bottom edge (towards the parents).
    pub outgoing: Vec<Segment>,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub rows: Vec<GraphRow>,
    /// Widest the graph gets, in lanes.
    pub width: usize,
}

pub fn layout(commits: &[CommitInfo]) -> Graph {
    // Each lane remembers which commit it is waiting for and its colour.
    let mut lanes: Vec<Option<(Oid, usize)>> = Vec::new();
    let mut next_color = 0usize;
    let mut graph = Graph::default();
    let mut new_color = || {
        let c = next_color;
        next_color += 1;
        c
    };

    for commit in commits {
        let mut row = GraphRow::default();

        // Lanes that were waiting for this commit.
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, l)| l.map(|(id, _)| id) == Some(commit.id))
            .map(|(i, _)| i)
            .collect();

        let (col, color) = match waiting.first() {
            Some(&i) => (i, lanes[i].unwrap().1),
            None => {
                // A branch tip nobody pointed at yet: take the first free lane.
                let i = free_lane(&mut lanes);
                (i, new_color())
            }
        };
        row.col = col;
        row.color = color;

        for &i in &waiting {
            let c = lanes[i].unwrap().1;
            row.incoming.push(Segment { from_col: i, to_col: col, color: c });
            lanes[i] = None;
        }

        // Every other active lane passes straight through.
        for (i, lane) in lanes.iter().enumerate() {
            if let Some((_, c)) = lane {
                row.pass.push(Segment { from_col: i, to_col: i, color: *c });
            }
        }

        // Route the parents.
        for (n, parent) in commit.parents.iter().enumerate() {
            if let Some(i) = lanes.iter().position(|l| l.map(|(id, _)| id) == Some(*parent)) {
                // Another lane already leads to this parent: join it.
                let c = lanes[i].unwrap().1;
                row.outgoing.push(Segment { from_col: col, to_col: i, color: if n == 0 { color } else { c } });
                continue;
            }
            let (lane, c) = if n == 0 {
                (col, color)
            } else {
                (free_lane(&mut lanes), new_color())
            };
            lanes[lane] = Some((*parent, c));
            row.outgoing.push(Segment { from_col: col, to_col: lane, color: c });
        }

        while matches!(lanes.last(), Some(None)) {
            lanes.pop();
        }
        let used = row
            .pass
            .iter()
            .chain(&row.incoming)
            .chain(&row.outgoing)
            .map(|s| s.from_col.max(s.to_col) + 1)
            .max()
            .unwrap_or(0)
            .max(col + 1);
        graph.width = graph.width.max(used);
        graph.rows.push(row);
    }
    graph
}

fn free_lane(lanes: &mut Vec<Option<(Oid, usize)>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: u8, parents: &[u8]) -> CommitInfo {
        let oid = |n: u8| Oid::from_bytes(&[n; 20]).unwrap();
        CommitInfo {
            id: oid(id),
            short_id: String::new(),
            summary: String::new(),
            message: String::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
            parents: parents.iter().map(|&p| oid(p)).collect(),
        }
    }

    #[test]
    fn linear_history_uses_one_lane() {
        let g = layout(&[c(3, &[2]), c(2, &[1]), c(1, &[])]);
        assert_eq!(g.width, 1);
        assert!(g.rows.iter().all(|r| r.col == 0));
    }

    #[test]
    fn branch_and_merge() {
        // 5 merges 4 (main) and 3 (feature); both come from 2.
        let g = layout(&[c(5, &[4, 3]), c(4, &[2]), c(3, &[2]), c(2, &[1]), c(1, &[])]);
        assert_eq!(g.width, 2);
        assert_eq!(g.rows[0].outgoing.len(), 2);
        assert_eq!(g.rows[1].col, 0);
        assert_eq!(g.rows[2].col, 1);
        // The feature lane curves back into main right below commit 3.
        assert_eq!(g.rows[2].outgoing[0].to_col, 0);
        assert_eq!(g.rows[3].incoming.len(), 1);
        assert_eq!(g.rows[3].col, 0);
        assert!(g.rows[4].pass.is_empty());
    }

    #[test]
    fn two_tips() {
        // Two branch tips (3 and 2) sharing parent 1.
        let g = layout(&[c(3, &[1]), c(2, &[1]), c(1, &[])]);
        assert_eq!(g.rows[0].col, 0);
        assert_eq!(g.rows[1].col, 1);
        assert_ne!(g.rows[0].color, g.rows[1].color);
        // Tip 2 joins lane 0, which carries on down to commit 1.
        assert_eq!(g.rows[1].outgoing, vec![Segment { from_col: 1, to_col: 0, color: g.rows[1].color }]);
        assert_eq!(g.rows[2].col, 0);
    }
}
