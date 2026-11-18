//! Where the flame view puts each node: an icicle with the plan's root across the full width and every node below it
//! as wide as its share of the cost, or of the time after ANALYZE, laid left to right under its parent.

use super::Plan;

/// One bar, its left edge and width as fractions of the full width, and its row from the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub node: usize,
    pub row: usize,
    pub x: f64,
    pub w: f64,
}

/// Whether the plan has times to weigh the bars by.
pub fn timed(p: &Plan) -> bool {
    p.analyzed && p.nodes.iter().any(|n| n.self_ms.is_some())
}

/// The bars of a plan and what their widths stand for.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub bars: Vec<Bar>,
    /// Each node's cost, or time, with the nodes below it, and the sum over the roots.
    pub weights: Vec<f64>,
    pub total: f64,
}

/// The bars of `p`, weighed by time when `time` and by cost otherwise. Nodes that weigh nothing get no bar; every other
/// bar is at least `min` wide, as a fraction of the full width, so a node too small to see still has a bar to point at
/// and click. A bar never grows past the room its parent has left, so it can still come out narrower at the end of a
/// crowded row.
pub fn layout(p: &Plan, time: bool, min: f64) -> Layout {
    let weights = super::tree::inclusive(p, time);
    let roots: Vec<usize> = (0..p.nodes.len()).filter(|&i| p.nodes[i].parent.is_none()).collect();
    let total: f64 = roots.iter().map(|&i| weights[i]).sum();
    let mut bars = Vec::new();
    if total > 0.0 && total.is_finite() {
        let mut x = 0.0;
        for &r in &roots {
            x += place(p, &weights, total, min, r, 0, x, 1.0 - x, &mut bars);
        }
    }
    Layout { bars, weights, total }
}

/// Places node `i` at `x` in at most `room`, then its children under it; returns the width it took.
#[allow(clippy::too_many_arguments)]
fn place(p: &Plan, weight: &[f64], total: f64, min: f64, i: usize, row: usize, x: f64, room: f64, out: &mut Vec<Bar>) -> f64 {
    // A child is never wider than the room its parent left, whatever rounding or clamping did to the sums.
    let w = if weight[i] > 0.0 { (weight[i] / total).max(min) } else { 0.0 }.min(room).max(0.0);
    if w <= 0.0 {
        return 0.0;
    }
    out.push(Bar { node: i, row, x, w });
    let mut cx = x;
    for &k in &p.nodes[i].kids {
        cx += place(p, weight, total, min, k, row + 1, cx, x + w - cx, out);
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::plan::parse;
    use crate::db::sql::Dialect;

    fn check(p: &Plan, bars: &[Bar]) {
        for b in bars {
            assert!(b.w > 0.0 && b.x >= 0.0 && b.x + b.w <= 1.0 + 1e-9, "{b:?} leaves the chart");
            let Some(j) = p.nodes[b.node].parent else { continue };
            let parent = bars.iter().find(|x| x.node == j).expect("a bar's parent has a bar");
            assert_eq!(b.row, parent.row + 1);
            assert!(b.x >= parent.x - 1e-9 && b.x + b.w <= parent.x + parent.w + 1e-9, "{b:?} is wider than its parent {parent:?}");
        }
        // Siblings never overlap.
        for a in bars {
            for b in bars.iter().filter(|b| b.node != a.node && b.row == a.row) {
                assert!(a.x + a.w <= b.x + 1e-9 || b.x + b.w <= a.x + 1e-9, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn the_root_spans_the_width_and_children_stay_inside_their_parent() {
        for (d, text) in [
            (Dialect::Postgres, include_str!("fixtures/pg18-join.json")),
            (Dialect::Postgres, include_str!("fixtures/pg18-fix-before.json")),
            (Dialect::Postgres, include_str!("fixtures/pg18-cte-analyze.json")),
            (Dialect::Postgres, include_str!("fixtures/pg18-parallel-analyze.json")),
            (Dialect::MySql, include_str!("fixtures/mysql-join.json")),
            (Dialect::MySql, include_str!("fixtures/mysql-join-analyze.txt")),
            (Dialect::MariaDb, include_str!("fixtures/mariadb-join-analyze.json")),
        ] {
            let p = parse(d, text).unwrap();
            for time in [false, true] {
                if time && !timed(&p) {
                    continue;
                }
                let bars = layout(&p, time, 0.0).bars;
                assert!(!bars.is_empty(), "no bars for {d:?} (time {time})");
                check(&p, &bars);
                if p.nodes.iter().filter(|n| n.parent.is_none()).count() == 1 {
                    assert!((bars[0].w - 1.0).abs() < 1e-9 && bars[0].x == 0.0, "the root does not span the width: {:?}", bars[0]);
                }
            }
        }
    }

    #[test]
    fn a_limit_keeps_the_scan_below_it_inside_its_own_width() {
        // Limit 9.27 over an Index Scan that would cost 29,661 to finish: the scan stops early, so it fits under the Limit.
        let p = parse(Dialect::Postgres, include_str!("fixtures/pg18-limit-analyze.json")).unwrap();
        assert!(p.nodes[1].cost.unwrap() > p.nodes[0].cost.unwrap() * 100.0);
        for time in [false, true] {
            let bars = layout(&p, time, 0.0).bars;
            check(&p, &bars);
            assert_eq!(bars[0].node, 0);
            assert!((bars[0].w - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_plan_that_weighs_nothing_has_no_bars() {
        let mut p = parse(Dialect::Postgres, include_str!("fixtures/pg18-scan.json")).unwrap();
        for n in &mut p.nodes {
            n.self_cost = None;
        }
        assert!(layout(&p, false, 0.0).bars.is_empty());
        assert!(layout(&p, true, 0.0).bars.is_empty(), "a plan that never ran has no times to weigh");
    }
}
