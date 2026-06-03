//! inf3d_monitor — comprehensive per-run telemetry recorder.
//!
//! A read-only "flight recorder" that writes a dense log of the whole game state
//! every run, so a developer (or an AI assistant reading the file) can reconstruct
//! almost exactly what happened — frame timing, entity/mesh/chunk/foliage counts,
//! asset counts, player physics, camera/zoom, quality settings, water, and
//! pathfinding — without attaching a debugger.
//!
//! It writes to **`inf3d-monitor.log`** in the process working directory
//! (the repo root when launched via `cargo run -p inf3d_app`), overwriting it
//! each run so the file always reflects the latest session. Disable by setting
//! the env var `INF3D_NO_MONITOR=1`.
//!
//! ## What it captures
//! - **Summary line** every [`SUMMARY_INTERVAL`] seconds: the full snapshot.
//! - **SPIKE line** the instant a frame hitches (frame time over an absolute or
//!   relative threshold), tagged with the frame-over-frame **deltas** of every
//!   count and the number of fixed physics ticks that frame — so each hitch is
//!   correlated with its likely cause (foliage spawn burst vs chunk remesh vs
//!   physics catch-up). This is the line that explains stutters.
//! - **EVENT lines** on discrete state changes: movement start/stop, quality
//!   preset change.
//!
//! The recorder only *reads* ECS state (queries + resources) and frame-over-frame
//! count deltas — it never instruments other crates, so it adds no coupling to the
//! systems it watches (it's a pure downstream sink, like `inf3d_ui`).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};

use bevy::camera::{Projection, ScalingMode};
use bevy::prelude::*;
use bevy_voxel_world::prelude::Chunk;
use bevy_water::WaterSettings;

use inf3d_camera::IsoCamera;
use inf3d_core::{QualityPreset, QualitySettings, Rock, Tree};
use inf3d_gameplay::{MovePath, Player};
use inf3d_physics::{CharacterController, DesiredMove, InteractionTarget};
use inf3d_pathfinding::PathTiming;
use inf3d_world::MainWorld;

/// Log file name (created in the process working directory, overwritten per run).
const LOG_PATH: &str = "inf3d-monitor.log";
/// Seconds between full summary lines.
const SUMMARY_INTERVAL: f32 = 0.5;
/// A frame slower than this (ms) is always logged as a spike.
const SPIKE_ABS_MS: f32 = 24.0;
/// ...or a frame slower than this multiple of the rolling median is a spike.
const SPIKE_MULT: f32 = 1.8;
/// Rolling frame-time window length (samples) for median / p95.
const WINDOW: usize = 120;
/// Minimum samples before the *relative* (median-based) spike test engages, so
/// the first chaotic startup frames don't each read as a spike.
const SPIKE_WARMUP: usize = 30;

/// Frame-over-frame countable quantities. Deltas of these on a spiked frame point
/// straight at the cause (e.g. `meshes +40` == a foliage spawn burst).
#[derive(Default, Clone, Copy)]
struct Counts {
    entities: i64,
    meshes: i64,
    chunks: i64,
    foliage_tiles: i64,
    trees: i64,
    rocks: i64,
}

impl Counts {
    fn delta(self, prev: Counts) -> Counts {
        Counts {
            entities: self.entities - prev.entities,
            meshes: self.meshes - prev.meshes,
            chunks: self.chunks - prev.chunks,
            foliage_tiles: self.foliage_tiles - prev.foliage_tiles,
            trees: self.trees - prev.trees,
            rocks: self.rocks - prev.rocks,
        }
    }
}

/// Counts the number of fixed-schedule ticks that elapse between rendered frames.
/// `>= 2` on a spiked frame means the physics loop ran extra catch-up steps (a
/// classic fixed-timestep hitch amplifier).
#[derive(Resource, Default)]
struct FixedTickCounter(u32);

/// The recorder's own state + the open log writer.
#[derive(Resource)]
struct Monitor {
    writer: Option<BufWriter<File>>,
    enabled: bool,
    frame: u64,
    times_ms: VecDeque<f32>,
    last_summary_t: f32,
    prev: Counts,
    /// Previous-frame movement / preset, for edge-triggered EVENT lines.
    prev_moving: bool,
    prev_preset: Option<QualityPreset>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            writer: None,
            enabled: std::env::var("INF3D_NO_MONITOR").is_err(),
            frame: 0,
            times_ms: VecDeque::with_capacity(WINDOW),
            last_summary_t: 0.0,
            prev: Counts::default(),
            prev_moving: false,
            prev_preset: None,
        }
    }
}

pub struct MonitorPlugin;

impl Plugin for MonitorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Monitor>()
            .init_resource::<FixedTickCounter>()
            .add_systems(Startup, open_log)
            // Count fixed ticks in the fixed schedule; read+reset per render frame.
            .add_systems(FixedUpdate, count_fixed_tick)
            // Record in `Last` so end-of-frame spawns/despawns are already applied.
            .add_systems(Last, record_frame);
    }
}

fn count_fixed_tick(mut ticks: ResMut<FixedTickCounter>) {
    ticks.0 += 1;
}

fn open_log(mut mon: ResMut<Monitor>, quality: Res<QualitySettings>) {
    if !mon.enabled {
        info!("inf3d_monitor: disabled via INF3D_NO_MONITOR");
        return;
    }
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    match File::create(LOG_PATH) {
        Ok(f) => {
            let mut w = BufWriter::new(f);
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(
                w,
                "# inf3d monitor | run_epoch={epoch} | cwd={cwd}\n\
                 # preset={} render_dist={} grass_radius={} foliage_ring_max={} \
                 ssao={} motion_blur={} dof={} bloom={} water={} water_amp={}\n\
                 # SUMMARY every {SUMMARY_INTERVAL}s; SPIKE on frame>{SPIKE_ABS_MS}ms or >{SPIKE_MULT}x median; deltas (d:) are vs previous frame",
                quality.preset.name(),
                quality.render_distance_chunks,
                quality.grass_radius_world,
                quality.foliage_ring_max,
                quality.ssao_enabled,
                quality.motion_blur_enabled,
                quality.dof_enabled,
                quality.bloom_enabled,
                quality.water_enabled,
                quality.water_amplitude,
            );
            let _ = w.flush();
            mon.writer = Some(w);
            info!("inf3d_monitor: recording to {cwd}/{LOG_PATH}");
        }
        Err(e) => {
            warn!("inf3d_monitor: could not create {LOG_PATH}: {e}");
            mon.enabled = false;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn record_frame(
    time: Res<Time>,
    mut mon: ResMut<Monitor>,
    mut ticks: ResMut<FixedTickCounter>,
    quality: Res<QualitySettings>,
    // Grouped into tuples to stay within Bevy's 16-system-param limit. All of
    // these are read-only, so the bundled queries never conflict with each other.
    opt_res: (
        Option<Res<WaterSettings>>,
        Option<Res<PathTiming>>,
        Option<Res<InteractionTarget>>,
    ),
    assets: (
        Res<Assets<Mesh>>,
        Res<Assets<StandardMaterial>>,
        Res<Assets<Image>>,
    ),
    count_q: (
        Query<Entity>,
        Query<(), With<Mesh3d>>,
        Query<(), With<Chunk<MainWorld>>>,
        Query<(), With<Tree>>,
        Query<(), With<Rock>>,
        Query<&Name>,
    ),
    q_player: Query<(&Transform, &Player, &CharacterController, &DesiredMove, &MovePath)>,
    q_cam: Query<(&Projection, &GlobalTransform), With<IsoCamera>>,
) {
    if !mon.enabled || mon.writer.is_none() {
        return;
    }
    let (water, path_timing, interaction) = opt_res;
    let (meshes, materials, images) = assets;
    let (q_all, q_mesh, q_chunk, q_tree, q_rock, q_names) = count_q;

    let dt_ms = time.delta_secs() * 1000.0;
    let elapsed = time.elapsed_secs();
    mon.frame += 1;
    let frame = mon.frame;
    if mon.times_ms.len() == WINDOW {
        mon.times_ms.pop_front();
    }
    mon.times_ms.push_back(dt_ms);
    let fixed_ticks = ticks.0;
    ticks.0 = 0;

    // --- rolling frame-time stats ---
    let mut sorted: Vec<f32> = mon.times_ms.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let median = sorted[n / 2];
    let p95 = sorted[(n * 95 / 100).min(n - 1)];
    let mean = sorted.iter().sum::<f32>() / n as f32;
    let fps = if mean > 0.0 { 1000.0 / mean } else { 0.0 };

    // --- counts (end-of-frame) + deltas ---
    let counts = Counts {
        entities: q_all.iter().count() as i64,
        meshes: q_mesh.iter().count() as i64,
        chunks: q_chunk.iter().count() as i64,
        foliage_tiles: q_names
            .iter()
            .filter(|name| name.as_str().starts_with("FoliageTile"))
            .count() as i64,
        trees: q_tree.iter().count() as i64,
        rocks: q_rock.iter().count() as i64,
    };
    let d = counts.delta(mon.prev);

    // --- player physics ---
    let (ppos, pcell, pfacing, grounded, vvel, desired, waypoints) = match q_player.single().ok() {
        Some((t, p, cc, dm, mp)) => (
            t.translation,
            p.cell,
            p.facing,
            cc.grounded,
            cc.vertical_velocity,
            dm.velocity,
            mp.waypoints.len(),
        ),
        None => (Vec3::ZERO, IVec2::ZERO, 0.0, false, 0.0, Vec3::ZERO, 0),
    };
    let moving = waypoints > 0;

    // --- camera / zoom ---
    let (zoom, cam_pos) = match q_cam.single() {
        Ok((proj, gt)) => {
            let z = match proj {
                Projection::Orthographic(o) => match o.scaling_mode {
                    ScalingMode::FixedVertical { viewport_height } => viewport_height,
                    _ => -1.0,
                },
                _ => -1.0,
            };
            (z, gt.translation())
        }
        Err(_) => (-1.0, Vec3::ZERO),
    };

    // --- pathfinding / interaction / water ---
    let (path_ms, path_exp) = path_timing
        .map(|p| (p.last_ms, p.last_expansions))
        .unwrap_or((0.0, 0));
    let has_target = interaction.map(|i| i.entity.is_some()).unwrap_or(false);
    let water_amp = water.map(|w| w.amplitude);

    // --- spike test ---
    let is_spike = dt_ms > SPIKE_ABS_MS || (n >= SPIKE_WARMUP && dt_ms > median * SPIKE_MULT);

    // Asset counts (Res params, independent of `mon`).
    let (mesh_assets, mat_assets, img_assets) = (meshes.len(), materials.len(), images.len());

    // Snapshot the edge-trigger state + decide what to write BEFORE borrowing the
    // writer — `mon.writer.as_mut()` goes through `ResMut`'s deref, which locks
    // the whole resource, so we must not read other `mon` fields while it's held.
    let moving_changed = moving != mon.prev_moving;
    let preset_changed = mon.prev_preset != Some(quality.preset);
    let should_summary = elapsed - mon.last_summary_t >= SUMMARY_INTERVAL;

    // All writes happen inside this scope; the writer borrow ends with it, so the
    // `mon.*` field updates afterward are free of a conflicting borrow.
    if let Some(writer) = mon.writer.as_mut() {
        if is_spike {
            let cause = if fixed_ticks >= 2 {
                "physics catch-up (fixed_ticks>=2)"
            } else if d.chunks.abs() >= 2 {
                "chunk (re)mesh"
            } else if d.meshes >= 16 || (d.trees + d.rocks) >= 8 {
                "foliage spawn burst"
            } else if d.entities <= -16 {
                "despawn burst"
            } else {
                "unknown (GPU/asset upload?)"
            };
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] *** SPIKE dt={dt_ms:6.1}ms (median {median:.1}, p95 {p95:.1}) fixed_ticks={fixed_ticks} *** \
                 cause={cause} | d: ent{:+} mesh{:+} chunk{:+} tile{:+} tree{:+} rock{:+} | \
                 moving={moving} wps={waypoints} player=({:.1},{:.1},{:.1}) cell=({},{}) zoom={zoom:.0}",
                d.entities, d.meshes, d.chunks, d.foliage_tiles, d.trees, d.rocks,
                ppos.x, ppos.y, ppos.z, pcell.x, pcell.y,
            );
        }

        if moving_changed {
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] EVENT move={} wps={waypoints} pathfind_last={path_ms:.2}ms exp={path_exp}",
                if moving { "START" } else { "STOP" },
            );
        }

        if preset_changed {
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] EVENT preset={} render_dist={} grass_radius={} ssao={} mb={} dof={} bloom={} water={}",
                quality.preset.name(),
                quality.render_distance_chunks,
                quality.grass_radius_world,
                quality.ssao_enabled,
                quality.motion_blur_enabled,
                quality.dof_enabled,
                quality.bloom_enabled,
                quality.water_enabled,
            );
        }

        if should_summary {
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] fps={fps:5.1} dt={dt_ms:5.1}ms p95={p95:5.1}ms fixed_ticks={fixed_ticks} | \
                 ent={} mesh={} chunk={} tile={} tree={} rock={} | assets: mesh={mesh_assets} mat={mat_assets} img={img_assets} | \
                 player=({:.1},{:.1},{:.1}) cell=({},{}) facing={pfacing:.2} grounded={grounded} vy={vvel:+.2} desired=({:.1},{:.1},{:.1}) wps={waypoints} | \
                 cam=({:.1},{:.1},{:.1}) zoom={zoom:.1} | path_last={path_ms:.2}ms exp={path_exp} target={has_target} | \
                 water_amp={} | preset={} ssao={} mb={} dof={} bloom={}",
                counts.entities, counts.meshes, counts.chunks, counts.foliage_tiles, counts.trees, counts.rocks,
                ppos.x, ppos.y, ppos.z, pcell.x, pcell.y, desired.x, desired.y, desired.z,
                cam_pos.x, cam_pos.y, cam_pos.z,
                water_amp.map(|a| format!("{a:.2}")).unwrap_or_else(|| "off".to_string()),
                quality.preset.name(), quality.ssao_enabled, quality.motion_blur_enabled,
                quality.dof_enabled, quality.bloom_enabled,
            );
        }

        if is_spike || moving_changed || preset_changed || should_summary {
            let _ = writer.flush();
        }
    }

    // Borrow of `mon.writer` is released; safe to mutate `mon` fields now.
    mon.prev = counts;
    mon.prev_moving = moving;
    mon.prev_preset = Some(quality.preset);
    if should_summary {
        mon.last_summary_t = elapsed;
    }
}
