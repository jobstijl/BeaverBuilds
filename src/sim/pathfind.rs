//! Grid pathfinding for beavers, run on the async task pool. Pure functions
//! over a snapshot of the map so the search can leave the main thread.

use bevy::prelude::*;
use std::collections::BinaryHeap;

/// Per-tile traversal cost snapshot. `INFINITY` is impassable.
#[derive(Clone)]
pub struct WalkGrid {
    pub width: u32,
    pub height: u32,
    pub cost: Vec<f32>,
}

/// Cost of stepping onto a stone path tile: noticeably cheaper than grass,
/// so routes bend toward built paths.
pub const PATH_COST: f32 = 0.45;
pub const GRASS_COST: f32 = 1.0;

impl WalkGrid {
    #[inline]
    fn idx(&self, t: UVec2) -> usize {
        (t.y * self.width + t.x) as usize
    }
}

#[derive(PartialEq)]
struct Open {
    score: f32,
    tile: UVec2,
}

impl Eq for Open {}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap on score.
        other.score.total_cmp(&self.score)
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A* over 4-neighbors. Returns the tile sequence from `start` (exclusive)
/// to `goal` (inclusive), or `None` if unreachable. The goal tile is always
/// enterable (it is the work site, even when occupied by the building).
pub fn find_path(grid: &WalkGrid, start: UVec2, goal: UVec2) -> Option<Vec<UVec2>> {
    if start == goal {
        return Some(Vec::new());
    }
    let n = (grid.width * grid.height) as usize;
    let mut best = vec![f32::INFINITY; n];
    let mut from: Vec<u32> = vec![u32::MAX; n];
    let mut open = BinaryHeap::new();
    let h = |t: UVec2| (t.x.abs_diff(goal.x) + t.y.abs_diff(goal.y)) as f32 * PATH_COST;
    best[grid.idx(start)] = 0.0;
    open.push(Open {
        score: h(start),
        tile: start,
    });
    while let Some(Open { tile, .. }) = open.pop() {
        if tile == goal {
            // Walk parents back to the start.
            let mut path = vec![goal];
            let mut at = grid.idx(goal);
            while from[at] != u32::MAX {
                at = from[at] as usize;
                let t = UVec2::new(at as u32 % grid.width, at as u32 / grid.width);
                if t == start {
                    break;
                }
                path.push(t);
            }
            path.reverse();
            return Some(path);
        }
        let g = best[grid.idx(tile)];
        for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
            let (nx, ny) = (tile.x as i32 + dx, tile.y as i32 + dy);
            if nx < 0 || ny < 0 || nx as u32 >= grid.width || ny as u32 >= grid.height {
                continue;
            }
            let next = UVec2::new(nx as u32, ny as u32);
            let step = if next == goal {
                GRASS_COST
            } else {
                grid.cost[grid.idx(next)]
            };
            if !step.is_finite() {
                continue;
            }
            let tentative = g + step;
            let i = grid.idx(next);
            if tentative < best[i] {
                best[i] = tentative;
                from[i] = grid.idx(tile) as u32;
                open.push(Open {
                    score: tentative + h(next),
                    tile: next,
                });
            }
        }
    }
    None
}
