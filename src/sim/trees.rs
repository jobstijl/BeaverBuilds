use bevy::prelude::*;

use super::map::Map;

/// A tree on a tile. Grows from 0.0 to 1.0; choppable when mature.
#[derive(Component)]
pub struct Tree {
    pub tile: UVec2,
    pub growth: f32,
}

impl Tree {
    pub const MATURE: f32 = 1.0;

    pub fn is_mature(&self) -> bool {
        self.growth >= Self::MATURE
    }
}

/// Seconds for a tree to fully grow on irrigated land (slower on dry land).
const GROWTH_TIME: f32 = 70.0;

pub struct TreesPlugin;

impl Plugin for TreesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, scatter_initial_trees)
            .add_systems(FixedUpdate, grow_trees);
    }
}

pub fn spawn_tree(commands: &mut Commands, map: &mut Map, tile: UVec2, growth: f32) {
    let entity = commands.spawn(Tree { tile, growth }).id();
    let i = map.idx(tile.x, tile.y);
    map.tree[i] = Some(entity);
}

fn scatter_initial_trees(mut commands: Commands, mut map: ResMut<Map>) {
    // Deterministic scatter on free land, denser near the river.
    let (w, h) = (map.width, map.height);
    for y in 0..h {
        for x in 0..w {
            let hash = ((x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503)) >> 3) % 100;
            let i = map.idx(x, y);
            let near_water = map.irrigated[i] || map.ground[i] <= 1;
            let chance = if near_water { 18 } else { 7 };
            if hash < chance && map.is_free_land(x, y) {
                let growth = 0.4 + (hash as f32 % 7.0) / 10.0;
                spawn_tree(&mut commands, &mut map, UVec2::new(x, y), growth);
            }
        }
    }
}

fn grow_trees(time: Res<Time>, map: Res<Map>, mut trees: Query<&mut Tree>) {
    let dt = time.delta_secs();
    for mut tree in &mut trees {
        if tree.growth >= Tree::MATURE {
            continue;
        }
        let irrigated = map.irrigated[map.idx(tree.tile.x, tree.tile.y)];
        let rate = if irrigated { 1.0 } else { 0.35 } / GROWTH_TIME;
        tree.growth = (tree.growth + rate * dt).min(Tree::MATURE);
    }
}
