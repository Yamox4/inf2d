#define_import_path inf3d::terrain_xray

// See-through ("x-ray") cutaway — the single shared decision used by BOTH the terrain
// forward shader and its custom prepass, so they cut the EXACT same voxels (the test
// is fully deterministic per voxel — no dither — so the two passes can never disagree
// and punch holes to the background).
//
// Unlike a screen-space circle, this works in WORLD space and is BLOCK-BASED: it cuts
// only the voxels actually sitting on the line of sight between the camera and the
// player's body, snapped to whole blocks. Bystander blocks merely near the player on
// screen (the old failure) are left solid.

struct XrayParams {
    // xyz = player body center (world); w = enabled (> 0.5).
    player: vec4<f32>,
    // xyz = camera forward (unit, INTO the scene); w = cut radius (world units).
    view: vec4<f32>,
    // x = player half-height (world); yzw reserved.
    extra: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(102)
var<uniform> xray: XrayParams;

// First material index that is a player BUILD — keep in sync with
// `inf3d_world::BUILT_MATERIAL_BASE`. Every terrain/city material is below it, so this
// alone classifies a fragment as build-vs-terrain.
const BUILT_MATERIAL_BASE: u32 = 10u;

// Whether this fragment's VOXEL should be cut for the cutaway: a player build that
// sits BETWEEN the camera and the player's body. Snaps to the voxel center so the
// whole block decides together (blocky), and measures distance to the player's whole
// vertical extent so blocks occluding the head/feet are cut too, not just the chest.
fn xray_should_discard(world_position: vec3<f32>, world_normal: vec3<f32>, material: u32) -> bool {
    if xray.player.w <= 0.5 || material < BUILT_MATERIAL_BASE {
        return false;
    }
    let p = xray.player.xyz;
    let v = xray.view.xyz;       // camera forward (into the scene)
    let radius = xray.view.w;
    let half_h = xray.extra.x;

    // One decision per voxel: test the block's CENTER so every fragment of it agrees.
    // Faces sit ON voxel boundaries, so step half a unit back along the (outward)
    // normal first — otherwise floor() would snap a face to its air neighbor, not the
    // solid block it belongs to.
    let center = floor(world_position - world_normal * 0.5) + vec3<f32>(0.5, 0.5, 0.5);
    let dc = center - p;

    // Distance along the view direction. A voxel toward the camera (occluding the
    // player) lies in the -v direction → dot < 0. Require it to be clearly in front,
    // so the player's own cell and everything behind the player stay solid.
    let along = dot(dc, v);
    if along > -0.1 {
        return false;
    }

    // Perpendicular (screen-plane) offset of the voxel from the player, then distance
    // to the player's vertical segment projected into that plane.
    let dc_perp = dc - along * v;
    let up = vec3<f32>(0.0, 1.0, 0.0);
    let up_perp = up - dot(up, v) * v;
    let up_len2 = max(dot(up_perp, up_perp), 1e-4);
    let s = clamp(dot(dc_perp, up_perp) / up_len2, -half_h, half_h);
    let dist = length(dc_perp - s * up_perp);

    return dist < radius;
}
