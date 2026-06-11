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

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: u32, height: u32, cost: Vec<f32>) -> WalkGrid {
        assert_eq!(cost.len(), (width * height) as usize);
        WalkGrid {
            width,
            height,
            cost,
        }
    }

    #[test]
    fn routes_around_walls() {
        // 5x5, a vertical wall at x=2 with a gap at y=4.
        let mut cost = vec![GRASS_COST; 25];
        for y in 0..4 {
            cost[(y * 5 + 2) as usize] = f32::INFINITY;
        }
        let g = grid(5, 5, cost);
        let path = find_path(&g, UVec2::new(0, 0), UVec2::new(4, 0)).expect("reachable");
        assert!(path.contains(&UVec2::new(2, 4)), "must use the gap");
        assert!(
            path.iter().all(|t| g.cost[g.idx(*t)].is_finite()),
            "never steps on blocked tiles"
        );
        assert_eq!(*path.last().unwrap(), UVec2::new(4, 0));
    }

    #[test]
    fn prefers_stone_paths_over_shorter_grass() {
        // 7x3: direct grass row at y=1; a paved detour along y=0.
        let mut cost = vec![GRASS_COST; 21];
        for x in 0..7 {
            cost[x as usize] = PATH_COST; // row y=0
        }
        let g = grid(7, 3, cost);
        let path = find_path(&g, UVec2::new(0, 1), UVec2::new(6, 1)).expect("reachable");
        assert!(
            path.iter().filter(|t| t.y == 0).count() >= 5,
            "the cheap paved row should carry the route: {path:?}"
        );
    }

    #[test]
    fn unreachable_is_none() {
        let mut cost = vec![GRASS_COST; 25];
        for y in 0..5 {
            cost[(y * 5 + 2) as usize] = f32::INFINITY;
        }
        let g = grid(5, 5, cost);
        assert!(find_path(&g, UVec2::new(0, 0), UVec2::new(4, 0)).is_none());
    }

    #[test]
    fn trivial_cases() {
        let g = grid(3, 3, vec![GRASS_COST; 9]);
        assert_eq!(
            find_path(&g, UVec2::new(1, 1), UVec2::new(1, 1)),
            Some(Vec::new())
        );
        let one = find_path(&g, UVec2::new(0, 0), UVec2::new(1, 0)).unwrap();
        assert_eq!(one, vec![UVec2::new(1, 0)]);
    }
}
