//! Procedural, asset-free character geometry + the spawn helper for the player
//! figure.
//!
//! The player reads as a small, friendly "blob mascot": a smooth teardrop head
//! perched on a curved, belly-bulged body, with two floating hand ellipsoids at
//! the sides and two floating foot ellipsoids out front. **Every mesh here is
//! generated in code** — no `.vox`/glTF assets — so the silhouette is exactly
//! tuned to the orthographic iso camera and shades seamlessly.
//!
//! Two builders do all the work:
//!
//! * [`lathe`] revolves a 2-D `(radius, y)` silhouette around the Y axis into a
//!   surface of revolution. It computes **proper per-vertex normals from the
//!   profile tangent** (rotated into 3-D per segment) so the result shades as a
//!   single smooth body — no faceting, no hand-authored normals. The head and
//!   body are both lathes of hand-picked profiles, so their bases taper to a
//!   single point and overlap into one another, hiding the junction.
//!
//! * [`ellipsoid`] is a UV-sphere whose vertices are scaled at *generation* time
//!   (never via [`Transform::scale`], which would shear the normals and wreck the
//!   lighting). Its normals are the **analytic ellipsoid normal**
//!   `normalize(x/rx², y/ry², z/rz²)`, so an egg-shaped hand or a long forward
//!   foot still lights correctly.
//!
//! All six parts share ONE [`StandardMaterial`] tone family (a small palette of
//! cloned handles) so the figure reads as one cohesive creature rather than a
//! pile of primitives.
//!
//! The spawn helper [`spawn_player`] builds each mesh **once** at startup, wires
//! the logical player entity (kinematic controller + follow marker, untouched by
//! animation), and parents the animated [`CharacterRoot`] tree of six flat
//! children beneath it. The per-part animation state lives on the [`Character`]
//! component the root carries; [`crate::animate_player`] reads it each frame.

use std::f32::consts::TAU;

use avian3d::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};

use inf3d_core::FollowTarget;
use inf3d_physics::{CharacterController, DesiredMove, GameLayer, PLAYER_DIMS};
use inf3d_worldgen::Terrain;

use crate::{
    AnimState, Character, CharacterRoot, MovePath, Part, Player, RestPos, VISUAL_ROOT_OFFSET,
};

// ---------------------------------------------------------------------------
// Procedural mesh builders
// ---------------------------------------------------------------------------

/// Revolve a 2-D silhouette `profile` (each point a `(radius, y)` pair, ordered
/// bottom→top) around the Y axis into a smooth surface of revolution.
///
/// * `segments` is the number of radial slices (≈32 reads as round at iso zoom).
/// * Per-vertex normals come from the **profile tangent**: for each profile
///   point we take the local 2-D tangent `(dr, dy)`, form the silhouette normal
///   `(dy, -dr)` (perpendicular, pointing outward), then rotate that normal
///   around Y for each segment. This is the analytic normal of the revolved
///   surface, so the mesh shades perfectly smoothly with no seams or facets.
/// * UVs wrap `u` around the revolution (0..1) and run `v` up the profile (0..1
///   by index), giving a clean cylindrical unwrap.
/// * The mesh is indexed (a tri-strip-style quad grid stitched into a triangle
///   list); endpoints with radius ~0 collapse to a shared apex naturally because
///   their ring vertices coincide, so a teardrop tip / body base closes cleanly.
///
/// Profiles whose first/last radius is 0 form closed, pointed caps — exactly the
/// teardrop head tip and the body's pointed base used below.
pub fn lathe(profile: &[Vec2], segments: u32) -> Mesh {
    let rings = profile.len();
    let seg = segments.max(3) as usize;
    // One extra column of vertices duplicates the seam so `u` can reach 1.0
    // without wrapping the texture (`seg + 1` columns per ring).
    let cols = seg + 1;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(rings * cols);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(rings * cols);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(rings * cols);

    // Precompute each profile point's 2-D silhouette normal from its tangent.
    // Central differences inside the profile, one-sided at the ends, so the
    // tangent is smooth across the whole curve.
    let mut profile_normals: Vec<Vec2> = Vec::with_capacity(rings);
    for i in 0..rings {
        let prev = profile[i.saturating_sub(1)];
        let next = profile[(i + 1).min(rings - 1)];
        let tangent = next - prev; // (dr, dy)
        // Outward normal of a Y-revolved profile: rotate the tangent -90° in the
        // (r, y) plane → (dy, -dr). `normalize_or_zero` guards a zero tangent.
        let n2 = Vec2::new(tangent.y, -tangent.x).normalize_or_zero();
        profile_normals.push(n2);
    }

    for (ri, point) in profile.iter().enumerate() {
        let r = point.x;
        let y = point.y;
        let n2 = profile_normals[ri];
        let v = ri as f32 / (rings - 1).max(1) as f32;
        for c in 0..cols {
            let theta = c as f32 / seg as f32 * TAU;
            let (st, ct) = theta.sin_cos();
            positions.push([r * ct, y, r * st]);
            // Rotate the 2-D silhouette normal (radial component `n2.x`, axial
            // component `n2.y`) around Y by `theta`.
            let nx = n2.x * ct;
            let nz = n2.x * st;
            let ny = n2.y;
            // At a pole (radius ~0) the radial part collapses; fall back to a
            // pure axial normal so the apex still has a sane, unit normal.
            let n = if r.abs() < 1e-5 {
                Vec3::new(0.0, n2.y.signum().max(0.0).mul_add(2.0, -1.0), 0.0)
            } else {
                Vec3::new(nx, ny, nz).normalize()
            };
            normals.push([n.x, n.y, n.z]);
            uvs.push([c as f32 / seg as f32, v]);
        }
    }

    // Stitch each adjacent ring pair into two triangles per segment.
    let mut indices: Vec<u32> = Vec::with_capacity((rings - 1) * seg * 6);
    for ri in 0..rings - 1 {
        let row0 = (ri * cols) as u32;
        let row1 = ((ri + 1) * cols) as u32;
        for c in 0..seg as u32 {
            let a = row0 + c;
            let b = row0 + c + 1;
            let d = row1 + c;
            let e = row1 + c + 1;
            // CCW winding so outward faces front (positions go bottom→top).
            indices.extend_from_slice(&[a, d, b, b, d, e]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Build a smooth ellipsoid: a UV-sphere whose vertices are scaled to
/// `(rx, ry, rz)` **at generation time**, with exact analytic ellipsoid normals.
///
/// Scaling the positions directly (rather than applying [`Transform::scale`] to a
/// unit sphere) keeps the normals correct: the true outward normal of the
/// surface `x²/rx² + y²/ry² + z²/rz² = 1` is `normalize(x/rx², y/ry², z/rz²)`,
/// which is **not** the position direction once the axes differ. We compute that
/// per vertex so an egg-shaped hand or a long, forward-pointing foot still
/// catches light naturally.
///
/// * `rings` = latitude bands (poles included), `sectors` = longitude slices.
/// * UVs are the standard equirectangular sphere unwrap.
pub fn ellipsoid(rx: f32, ry: f32, rz: f32, rings: u32, sectors: u32) -> Mesh {
    let rings = rings.max(2) as usize;
    let sectors = sectors.max(3) as usize;
    let cols = sectors + 1; // duplicate seam column for clean UVs

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity((rings + 1) * cols);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity((rings + 1) * cols);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity((rings + 1) * cols);

    let inv_rx2 = 1.0 / (rx * rx);
    let inv_ry2 = 1.0 / (ry * ry);
    let inv_rz2 = 1.0 / (rz * rz);

    for ring in 0..=rings {
        // phi: 0 at the +Y pole → PI at the -Y pole.
        let phi = ring as f32 / rings as f32 * std::f32::consts::PI;
        let (sin_phi, cos_phi) = phi.sin_cos();
        for col in 0..cols {
            let theta = col as f32 / sectors as f32 * TAU;
            let (sin_t, cos_t) = theta.sin_cos();
            // Unit-sphere direction, then scale per axis for the position.
            let ux = sin_phi * cos_t;
            let uy = cos_phi;
            let uz = sin_phi * sin_t;
            let x = rx * ux;
            let y = ry * uy;
            let z = rz * uz;
            positions.push([x, y, z]);
            let n = Vec3::new(x * inv_rx2, y * inv_ry2, z * inv_rz2).normalize();
            normals.push([n.x, n.y, n.z]);
            uvs.push([col as f32 / sectors as f32, ring as f32 / rings as f32]);
        }
    }

    let mut indices: Vec<u32> = Vec::with_capacity(rings * sectors * 6);
    for ring in 0..rings {
        let row0 = (ring * cols) as u32;
        let row1 = ((ring + 1) * cols) as u32;
        for col in 0..sectors as u32 {
            let a = row0 + col;
            let b = row0 + col + 1;
            let d = row1 + col;
            let e = row1 + col + 1;
            indices.extend_from_slice(&[a, d, b, b, d, e]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// The radial resolution shared by the lathe parts — round at iso zoom, cheap.
const LATHE_SEGMENTS: u32 = 32;

/// Teardrop **head** profile, ordered bottom→top in `(radius, y)`.
///
/// Starts at a single closed point at the base (radius 0), swells to a rounded
/// belly, then tapers back to a pointed tip at the very top — a smooth egg/
/// teardrop. The base point overlaps **down into** the body top so the two
/// lathes fuse with no visible seam.
fn head_profile() -> Vec<Vec2> {
    vec![
        Vec2::new(0.00, -0.30), // closed base point (sinks into the body)
        Vec2::new(0.14, -0.22),
        Vec2::new(0.24, -0.10),
        Vec2::new(0.31, 0.04),
        Vec2::new(0.34, 0.18), // widest belly
        Vec2::new(0.33, 0.30),
        Vec2::new(0.27, 0.42),
        Vec2::new(0.17, 0.52),
        Vec2::new(0.07, 0.60),
        Vec2::new(0.00, 0.66), // pointed top tip
    ]
}

/// Curved-trapezoid **body** profile, ordered bottom→top in `(radius, y)`.
///
/// A `ConicalFrustum`-like shape but with curved sides and a soft belly bulge:
/// narrow at the top (~0.40) where the head sits, bulging out at a low belly
/// (~0.70), then rounding in to a single closed point at the base so the figure
/// stands on a soft rounded bottom rather than a flat disc.
fn body_profile() -> Vec<Vec2> {
    vec![
        Vec2::new(0.00, -0.62), // closed base point (rounded bottom)
        Vec2::new(0.30, -0.56),
        Vec2::new(0.52, -0.44),
        Vec2::new(0.66, -0.28),
        Vec2::new(0.70, -0.10), // widest belly
        Vec2::new(0.67, 0.08),
        Vec2::new(0.60, 0.24),
        Vec2::new(0.52, 0.38),
        Vec2::new(0.45, 0.50),
        Vec2::new(0.40, 0.60), // narrow top shoulders (head overlaps here)
    ]
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Spawn the player at the nearest land column to (0, 0): a logical parent entity
/// carrying the gameplay transform + kinematic controller, with the animated
/// procedural character figure as its child tree.
///
/// The logical entity is **identical** to before (same `Player`, `MovePath`,
/// `FollowTarget`, `CharacterController`, `DesiredMove`, capsule + layers,
/// `TransformInterpolation`) so movement, camera-follow, physics, and pathfinding
/// are untouched. Only the visual child tree changed: it is now six procedural
/// meshes parented flat under a [`CharacterRoot`] that also carries the
/// [`Character`] animation-state component (the child `Entity` handles + phase /
/// state / jump timer / breathe clock the animator drives).
///
/// All meshes are built **once** here and the material handles are shared (cloned)
/// across parts, per the "meshes built once at spawn" invariant.
pub fn spawn_player(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    terrain: Res<Terrain>,
) {
    // Spawn on the nearest land so the player never starts submerged in water.
    let spawn = terrain.nearest_land(IVec2::ZERO);
    let center = terrain.stand_pos(spawn.x, spawn.y) + Vec3::Y * VISUAL_ROOT_OFFSET;

    // --- Procedural meshes (built once) -----------------------------------
    let body_mesh = meshes.add(lathe(&body_profile(), LATHE_SEGMENTS));
    let head_mesh = meshes.add(lathe(&head_profile(), LATHE_SEGMENTS));
    // Hands: gently egg-shaped, a touch longer front-to-back than tall.
    let hand_mesh = meshes.add(ellipsoid(0.16, 0.14, 0.18, 12, 18));
    // Feet: longer on +Z (forward) so they read as feet pointing ahead.
    let foot_mesh = meshes.add(ellipsoid(0.20, 0.13, 0.32, 12, 18));

    // --- Shared material family -------------------------------------------
    // One cohesive look: a warm body, a lighter "skin" head, dark extremities.
    // A single tuned roughness/metallic across all so light reads as one figure.
    let body_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.24, 0.22),
        perceptual_roughness: 0.62,
        metallic: 0.0,
        ..default()
    });
    let head_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.96, 0.86, 0.78),
        perceptual_roughness: 0.58,
        metallic: 0.0,
        ..default()
    });
    let limb_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.20, 0.20, 0.26),
        perceptual_roughness: 0.55,
        metallic: 0.0,
        ..default()
    });

    // --- Rest poses (neutral local offsets each part eases toward) ---------
    // The body sits centered; the head rides its narrow top, sunk slightly so the
    // teardrop base overlaps the body shoulders and the join disappears. Hands
    // float at belly height to the sides; feet float just below, out front.
    let body_rest = Vec3::new(0.0, 0.05, 0.0);
    let head_rest = Vec3::new(0.0, 0.74, 0.0);
    let hand_l_rest = Vec3::new(-0.66, -0.02, 0.04);
    let hand_r_rest = Vec3::new(0.66, -0.02, 0.04);
    let foot_l_rest = Vec3::new(-0.26, -0.78, 0.14);
    let foot_r_rest = Vec3::new(0.26, -0.78, 0.14);

    // Spawn the six children first so we can record their `Entity` ids on the
    // `Character` component (flat parenting: all are direct children of the root).
    commands
        .spawn((
            Transform::from_translation(center),
            Visibility::default(),
            Player {
                speed: 8.0,
                cell: spawn,
                facing: 0.0,
            },
            MovePath::default(),
            FollowTarget,
            // Kinematic character controller: avian moves it only via our
            // `move_and_slide` in `inf3d_physics`, never auto-integrated. The
            // capsule is built from the single `PLAYER_DIMS` source of truth. The
            // Player layer collides with Solid props only — the ground is derived
            // analytically from the Terrain heightfield, so there is no terrain
            // collider/layer to hit.
            RigidBody::Kinematic,
            Collider::capsule(PLAYER_DIMS.radius, PLAYER_DIMS.half_height * 2.0),
            CollisionLayers::new(GameLayer::Player, [GameLayer::Solid]),
            CharacterController::default(),
            DesiredMove::default(),
            // The controller writes this `Transform` in `FixedPostUpdate`;
            // avian's `TransformInterpolation` eases it between fixed ticks (right
            // after `FixedMain`, before `Update`) so the rendered figure — and
            // the camera following it — stay smooth at any frame rate / zoom.
            TransformInterpolation,
        ))
        .with_children(|parent| {
            // The visual root sits at local Y = -VISUAL_ROOT_OFFSET so the
            // figure's feet land on the capsule's feet (derived from PLAYER_DIMS).
            // Spawn the six parts as its children (flat parenting); the
            // `with_children` spawner auto-attaches a `ChildOf(root)` relationship,
            // so each part is parented to the root without a manual `ChildOf`.
            let mut root = parent.spawn((
                CharacterRoot,
                Transform::from_xyz(0.0, -VISUAL_ROOT_OFFSET, 0.0),
                Visibility::default(),
            ));

            // Capture each child id out of the `with_children` closure so we can
            // wire them onto the root's `Character` state afterward.
            let (mut body, mut head) = (Entity::PLACEHOLDER, Entity::PLACEHOLDER);
            let (mut hand_l, mut hand_r) = (Entity::PLACEHOLDER, Entity::PLACEHOLDER);
            let (mut foot_l, mut foot_r) = (Entity::PLACEHOLDER, Entity::PLACEHOLDER);

            root.with_children(|root| {
                // Spawn a part as a child of the root and return its id. Each
                // carries its `Part` tag + `RestPos` for the animator.
                let mut spawn_part = |mesh: Handle<Mesh>,
                                      mat: Handle<StandardMaterial>,
                                      part: Part,
                                      rest: Vec3|
                 -> Entity {
                    root.spawn((
                        Mesh3d(mesh),
                        MeshMaterial3d(mat),
                        Transform::from_translation(rest),
                        part,
                        RestPos(rest),
                    ))
                    .id()
                };

                body = spawn_part(body_mesh, body_mat, Part::Body, body_rest);
                head = spawn_part(head_mesh, head_mat, Part::Head, head_rest);
                hand_l = spawn_part(hand_mesh.clone(), limb_mat.clone(), Part::HandL, hand_l_rest);
                hand_r = spawn_part(hand_mesh, limb_mat.clone(), Part::HandR, hand_r_rest);
                foot_l = spawn_part(foot_mesh.clone(), limb_mat.clone(), Part::FootL, foot_l_rest);
                foot_r = spawn_part(foot_mesh, limb_mat, Part::FootR, foot_r_rest);
            });

            // Attach the animation-state component to the root, wiring the child
            // ids the animator mutates each frame.
            root.insert(Character {
                head,
                body,
                hand_l,
                hand_r,
                foot_l,
                foot_r,
                state: AnimState::Idle,
                phase: 0.0,
                jump_t: 0.0,
                breathe: 0.0,
            });
        });
}
