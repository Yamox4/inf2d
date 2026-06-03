//! Self-contained dust particle puff system.
//!
//! Send a [`DustBurst`] message to spawn a short-lived cloud of small cubes
//! that fly outward and upward, fall under gravity, and fade as they expire.

use bevy::prelude::*;

/// Request a puff of `amount` dust particles at `pos`, flung outward at `speed`.
#[derive(Message)]
pub struct DustBurst {
    /// World-space origin of the puff.
    pub pos: Vec3,
    /// Number of particles to spawn.
    pub amount: u32,
    /// Overall velocity multiplier for how forcefully particles scatter.
    pub speed: f32,
}

/// Plugin wiring up the dust assets and emit/update systems.
pub struct DustPlugin;

impl Plugin for DustPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<DustBurst>()
            .add_systems(Startup, init_dust_assets)
            .add_systems(Update, (emit_dust, update_dust));
    }
}

/// Shared mesh + material handles so every particle reuses one allocation.
#[derive(Resource)]
struct DustAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Per-particle motion state and lifetime.
#[derive(Component)]
struct Dust {
    vel: Vec3,
    age: f32,
    life: f32,
}

/// Build the shared cube mesh and a dirt-tan, unlit, translucent material once.
fn init_dust_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(Cuboid::from_length(0.16));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.66, 0.55, 0.40, 0.85),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    commands.insert_resource(DustAssets { mesh, material });
}

/// Spawn particles for each incoming [`DustBurst`].
fn emit_dust(
    mut commands: Commands,
    assets: Res<DustAssets>,
    mut bursts: MessageReader<DustBurst>,
) {
    use rand::Rng;
    let mut rng = rand::rng();

    for b in bursts.read() {
        // Clamp the burst size so a stray/huge `amount` can't spawn an
        // unbounded number of entities synchronously in one frame.
        const MAX_BURST: u32 = 256;
        let amount = b.amount.min(MAX_BURST);
        for _ in 0..amount {
            // Scatter on a random horizontal heading, biased upward.
            let angle = rng.random_range(0.0..std::f32::consts::TAU);
            let radial = rng.random_range(0.2..1.0);
            let up = rng.random_range(0.6..1.4);
            let vel = Vec3::new(angle.cos() * radial, up, angle.sin() * radial) * b.speed;

            // Jitter the spawn point so particles don't all start co-located.
            let offset = Vec3::new(
                rng.random_range(-0.15..0.15),
                0.05,
                rng.random_range(-0.15..0.15),
            );

            commands.spawn((
                Mesh3d(assets.mesh.clone()),
                MeshMaterial3d(assets.material.clone()),
                // Start at a tiny non-zero scale; update_dust pops it in.
                // A literal zero scale gives a degenerate model matrix/AABB on
                // the first frame, so begin slightly above zero instead.
                Transform::from_translation(b.pos + offset).with_scale(Vec3::splat(0.01)),
                Dust {
                    vel,
                    age: 0.0,
                    life: rng.random_range(0.35..0.6),
                },
            ));
        }
    }
}

/// Advance, decelerate, scale, and retire dust particles.
fn update_dust(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut Dust)>,
) {
    let dt = time.delta_secs();

    for (e, mut t, mut d) in &mut q {
        d.age += dt;
        if d.age >= d.life {
            commands.entity(e).despawn();
            continue;
        }

        // Gravity plus air drag give the puff a soft settling arc.
        d.vel.y -= 6.0 * dt;
        d.vel *= 1.0 - (2.0 * dt).min(1.0);
        t.translation += d.vel * dt;

        // Quick pop-in over the first 20% of life, then taper to nothing.
        let f = d.age / d.life;
        let s = if f < 0.2 {
            f / 0.2
        } else {
            1.0 - (f - 0.2) / 0.8
        };
        t.scale = Vec3::splat(s.clamp(0.0, 1.0));
    }
}
