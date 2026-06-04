//! inf3d_monitor — comprehensive per-run telemetry recorder ("flight recorder").
//!
//! A read-only sink (like the HUD) that writes a dense log of the WHOLE engine
//! state every run, so a developer — or an AI assistant reading the file — can
//! reconstruct almost exactly what happened without attaching a debugger. It only
//! *reads* ECS state (queries + resources); it never instruments other crates.
//!
//! Writes to **`inf3d-monitor.log`** in the process working directory (the repo
//! root under `cargo run`), OVERWRITTEN each run so the file always reflects the
//! latest session. Disable with `INF3D_NO_MONITOR=1`.
//!
//! ## Line types
//! - **FRAME** — every [`SUMMARY_INTERVAL`]s: the fast-changing per-frame numbers
//!   (fps / dt / p95 / interval min-max / spike-count / fixed-ticks) plus world
//!   counts (chunks / meshes / entities + deltas), player, zoom, dynamic render
//!   distance.
//! - **STATE** — every [`STATE_INTERVAL`]s (+ frame 1): the slow-changing full
//!   pipeline state across several labelled sub-lines — CAMERA, GFX (post-FX +
//!   prepass + MSAA + HDR), LIGHT (sun + shadows + cascades + ambient + clear),
//!   QUALITY (fixed graphics fields), WATER, ASSETS. This is the section that makes
//!   graphics / shader / lighting regressions visible (e.g. `shadows=false`).
//! - **SPIKE** — the instant a frame hitches: frame-over-frame deltas of every
//!   count + the real likely cause (streaming burst / foliage / despawn / high
//!   zoom / physics). The cause heuristic checks the actual work deltas FIRST —
//!   high `fixed_ticks` is a *symptom* of an already-slow frame, not the cause, so
//!   it's the last resort, not the first guess.
//! - **EVENT** — discrete changes: movement start/stop.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};

use bevy::camera::{Projection, ScalingMode};
use bevy::core_pipeline::prepass::{DepthPrepass, MotionVectorPrepass, NormalPrepass};
use bevy::light::{CascadeShadowConfig, DirectionalLightShadowMap};
use bevy::pbr::ScreenSpaceAmbientOcclusion;
use bevy::post_process::bloom::Bloom;
use bevy::post_process::dof::DepthOfField;
use bevy::post_process::motion_blur::MotionBlur;
use bevy::prelude::*;
use bevy::render::view::{Hdr, Msaa};
use bevy_voxel_world::prelude::Chunk;
use bevy_water::WaterSettings;

use inf3d_camera::IsoCamera;
use inf3d_core::{QualitySettings, Rock, Tree};
use inf3d_gameplay::{MovePath, Player};
use inf3d_pathfinding::PathTiming;
use inf3d_physics::{CharacterController, DesiredMove, InteractionTarget};
use inf3d_render::FoliageTile;
use inf3d_world::MainWorld;

/// Log file name (created in the process working directory, overwritten per run).
const LOG_PATH: &str = "inf3d-monitor.log";
/// Seconds between FRAME lines (fast per-frame numbers).
const SUMMARY_INTERVAL: f32 = 0.5;
/// Seconds between full STATE dumps (slow-changing camera/gfx/light/quality
/// state). Less frequent than FRAME so the log stays readable while still
/// snapshotting the whole pipeline many times per run.
const STATE_INTERVAL: f32 = 2.0;
/// A frame slower than this (ms) is always logged as a spike.
const SPIKE_ABS_MS: f32 = 24.0;
/// ...or a frame slower than this multiple of the rolling median is a spike.
const SPIKE_MULT: f32 = 1.8;
/// Rolling frame-time window length (samples) for median / p95.
const WINDOW: usize = 120;
/// Minimum samples before the *relative* (median-based) spike test engages, so
/// the chaotic startup frames don't each read as a spike.
const SPIKE_WARMUP: usize = 30;

/// Frame-over-frame countable quantities. Deltas of these on a spiked frame point
/// straight at the cause (e.g. `meshes +40` == a foliage/chunk spawn burst).
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
/// `>= 2` on a spiked frame means the physics loop ran extra catch-up steps — but
/// that is an *amplifier* of an already-slow frame, not usually the root cause.
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
    last_state_t: f32,
    prev: Counts,
    /// Previous-frame movement, for edge-triggered EVENT lines.
    prev_moving: bool,
    /// Min / max dt and spike count accumulated since the last FRAME line, so each
    /// FRAME reports the worst case in its interval (not just the instant sample).
    interval_min_ms: f32,
    interval_max_ms: f32,
    interval_spikes: u32,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            writer: None,
            enabled: std::env::var("INF3D_NO_MONITOR").is_err(),
            frame: 0,
            times_ms: VecDeque::with_capacity(WINDOW),
            last_summary_t: 0.0,
            last_state_t: 0.0,
            prev: Counts::default(),
            prev_moving: false,
            interval_min_ms: f32::INFINITY,
            interval_max_ms: 0.0,
            interval_spikes: 0,
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
                 # graphics=fixed-high render_dist={} grass_radius={} foliage_ring_max={}\n\
                 # FRAME every {SUMMARY_INTERVAL}s; STATE every {STATE_INTERVAL}s; \
                 SPIKE on frame>{SPIKE_ABS_MS}ms or >{SPIKE_MULT}x median; d:=delta vs prev frame",
                quality.render_distance_chunks,
                quality.grass_radius_world,
                quality.foliage_ring_max,
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

/// Format a `Color` as `r,g,b` (sRGB, 2 decimals) for compact logging.
fn rgb(c: Color) -> String {
    let s = c.to_srgba();
    format!("{:.2},{:.2},{:.2}", s.red, s.green, s.blue)
}

#[allow(clippy::too_many_arguments)]
fn record_frame(
    time: Res<Time>,
    mut mon: ResMut<Monitor>,
    mut ticks: ResMut<FixedTickCounter>,
    quality: Res<QualitySettings>,
    // Grouped into tuples to stay within Bevy's 16-system-param limit. All of
    // these are read-only, so the bundled queries never conflict with each other.
    res: (
        Option<Res<WaterSettings>>,
        Option<Res<PathTiming>>,
        Option<Res<InteractionTarget>>,
        Res<MainWorld>,
        Res<ClearColor>,
        Option<Res<DirectionalLightShadowMap>>,
        Option<Res<GlobalAmbientLight>>,
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
        Query<(), With<FoliageTile>>,
    ),
    q_player: Query<(
        &Transform,
        &Player,
        &CharacterController,
        &DesiredMove,
        &MovePath,
    )>,
    q_cam: Query<
        (
            &Projection,
            &GlobalTransform,
            Option<&Bloom>,
            Option<&DepthOfField>,
            Option<&ScreenSpaceAmbientOcclusion>,
            Option<&MotionBlur>,
            Option<&DepthPrepass>,
            Option<&NormalPrepass>,
            Option<&MotionVectorPrepass>,
            Option<&Hdr>,
            Option<&Msaa>,
        ),
        With<IsoCamera>,
    >,
    q_light: Query<(&DirectionalLight, &Transform, Option<&CascadeShadowConfig>)>,
) {
    if !mon.enabled || mon.writer.is_none() {
        return;
    }
    let (water, path_timing, interaction, main_world, clear, shadow_map, ambient) = res;
    let (meshes, materials, images) = assets;
    let (q_all, q_mesh, q_chunk, q_tree, q_rock, q_tiles) = count_q;

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

    // --- rolling frame-time stats (select_nth, no full sort) ---
    let mut samples: Vec<f32> = mon.times_ms.iter().copied().collect();
    let n = samples.len();
    let cmp = |a: &f32, b: &f32| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
    let med_idx = n / 2;
    let p95_idx = (n * 95 / 100).min(n - 1);
    samples.select_nth_unstable_by(p95_idx, cmp);
    let p95 = samples[p95_idx];
    samples.select_nth_unstable_by(med_idx, cmp);
    let median = samples[med_idx];
    let mean = mon.times_ms.iter().sum::<f32>() / n as f32;
    let fps = if mean > 0.0 { 1000.0 / mean } else { 0.0 };

    // --- spike test + per-interval min/max/spike accumulation ---
    let is_spike = dt_ms > SPIKE_ABS_MS || (n >= SPIKE_WARMUP && dt_ms > median * SPIKE_MULT);
    mon.interval_min_ms = mon.interval_min_ms.min(dt_ms);
    mon.interval_max_ms = mon.interval_max_ms.max(dt_ms);
    if is_spike {
        mon.interval_spikes += 1;
    }

    // --- counts (end-of-frame) + deltas ---
    let counts = Counts {
        entities: q_all.iter().count() as i64,
        meshes: q_mesh.iter().count() as i64,
        chunks: q_chunk.iter().count() as i64,
        foliage_tiles: q_tiles.iter().count() as i64,
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

    // --- camera + full graphics state ---
    let (
        zoom,
        near,
        far,
        cam_pos,
        hdr_on,
        bloom_s,
        dof_s,
        ssao_s,
        mb_s,
        depth_pp,
        normal_pp,
        motion_pp,
        msaa,
    ) = match q_cam.single() {
        Ok((proj, gt, bloom, dof, ssao, mb, dpp, npp, mpp, hdr, msaa)) => {
            let (zoom, near, far) = match proj {
                Projection::Orthographic(o) => {
                    let z = match o.scaling_mode {
                        ScalingMode::FixedVertical { viewport_height } => viewport_height,
                        _ => -1.0,
                    };
                    (z, o.near, o.far)
                }
                _ => (-1.0, 0.0, 0.0),
            };
            let msaa_samples = match msaa {
                Some(Msaa::Off) => 1u32,
                Some(Msaa::Sample2) => 2,
                Some(Msaa::Sample4) => 4,
                Some(Msaa::Sample8) => 8,
                None => 0,
            };
            (
                zoom,
                near,
                far,
                gt.translation(),
                hdr.is_some(),
                bloom
                    .map(|b| format!("on(int={:.2})", b.intensity))
                    .unwrap_or_else(|| "off".to_string()),
                dof.map(|d| format!("on(fd={:.0},f{:.0})", d.focal_distance, d.aperture_f_stops))
                    .unwrap_or_else(|| "off".to_string()),
                ssao
                    .map(|s| format!("on({:?})", s.quality_level))
                    .unwrap_or_else(|| "off".to_string()),
                mb.map(|m| format!("on(sa={:.2},n={})", m.shutter_angle, m.samples))
                    .unwrap_or_else(|| "off".to_string()),
                dpp.is_some(),
                npp.is_some(),
                mpp.is_some(),
                msaa_samples,
            )
        }
        Err(_) => (
            -1.0,
            0.0,
            0.0,
            Vec3::ZERO,
            false,
            "?".to_string(),
            "?".to_string(),
            "?".to_string(),
            "?".to_string(),
            false,
            false,
            false,
            0,
        ),
    };

    // --- directional light + shadows (the section that exposes shadow regressions) ---
    let light_str = match q_light.single() {
        Ok((dl, tf, cascade)) => {
            let dir = *tf.forward();
            let (ncasc, max_b) = cascade
                .map(|c| (c.bounds.len(), c.bounds.last().copied().unwrap_or(0.0)))
                .unwrap_or((0, 0.0));
            format!(
                "dir=({:.2},{:.2},{:.2}) illum={:.0} color=({}) shadows={} cascades={} max_dist={:.0}",
                dir.x, dir.y, dir.z, dl.illuminance, rgb(dl.color), dl.shadows_enabled, ncasc, max_b,
            )
        }
        Err(_) => "NONE (no single DirectionalLight!)".to_string(),
    };
    let shadow_map_size = shadow_map.as_ref().map(|s| s.size).unwrap_or(0);
    let ambient_str = ambient
        .as_ref()
        .map(|a| format!("({},bri={:.0})", rgb(a.color), a.brightness))
        .unwrap_or_else(|| "none".to_string());

    // --- pathfinding / interaction / water / streaming ---
    let (path_ms, path_exp) = path_timing
        .map(|p| (p.last_ms, p.last_expansions))
        .unwrap_or((0.0, 0));
    let has_target = interaction.map(|i| i.entity.is_some()).unwrap_or(false);
    let water_amp = water.as_ref().map(|w| w.amplitude);
    let render_dist_dyn = main_world.render_distance_chunks;
    let (mesh_assets, mat_assets, img_assets) = (meshes.len(), materials.len(), images.len());

    // Snapshot mon-owned fields BEFORE borrowing the writer — `mon.writer.as_mut()`
    // locks the whole resource through `ResMut`'s deref, so we must not read other
    // `mon` fields while it's held.
    let moving_changed = moving != mon.prev_moving;
    let should_summary = elapsed - mon.last_summary_t >= SUMMARY_INTERVAL;
    let should_state = frame == 1 || elapsed - mon.last_state_t >= STATE_INTERVAL;
    let interval_min = mon.interval_min_ms;
    let interval_max = mon.interval_max_ms;
    let interval_spikes = mon.interval_spikes;

    if let Some(writer) = mon.writer.as_mut() {
        if is_spike {
            // Inspect the ACTUAL work deltas first; high `fixed_ticks` is a symptom
            // of an already-slow frame, so it's the last resort, not the first guess.
            let cause = if d.chunks.abs() >= 8 || d.meshes.abs() >= 30 {
                "chunk/mesh streaming burst"
            } else if d.trees + d.rocks >= 8 || d.meshes >= 12 {
                "foliage spawn burst"
            } else if d.entities <= -16 {
                "despawn burst"
            } else if zoom >= 70.0 {
                "high-zoom render load"
            } else if fixed_ticks >= 4 {
                "physics catch-up (amplifier)"
            } else {
                "unknown (GPU/asset upload?)"
            };
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] *** SPIKE dt={dt_ms:6.1}ms (median {median:.1}, p95 {p95:.1}) fixed_ticks={fixed_ticks} *** \
                 cause={cause} | d: ent{:+} mesh{:+} chunk{:+} tile{:+} tree{:+} rock{:+} | \
                 moving={moving} wps={waypoints} player=({:.1},{:.1},{:.1}) cell=({},{}) zoom={zoom:.0} rd={render_dist_dyn}",
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

        if should_summary {
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] FRAME fps={fps:5.1} dt={dt_ms:5.1} p95={p95:5.1} min={interval_min:5.1} max={interval_max:6.1} spikes/intvl={interval_spikes} fixed_ticks={fixed_ticks} | \
                 chunk={}(d{:+}) mesh={}(d{:+}) ent={}(d{:+}) | tile={} tree={} rock={} | \
                 player=({:.1},{:.1},{:.1}) cell=({},{}) grounded={grounded} vy={vvel:+.2} wps={waypoints} | zoom={zoom:.0} rd_dyn={render_dist_dyn} | path={path_ms:.2}ms exp={path_exp} target={has_target}",
                counts.chunks, d.chunks, counts.meshes, d.meshes, counts.entities, d.entities,
                counts.foliage_tiles, counts.trees, counts.rocks,
                ppos.x, ppos.y, ppos.z, pcell.x, pcell.y,
            );
        }

        if should_state {
            let _ = writeln!(
                writer,
                "[t={elapsed:8.2} f={frame:>7}] STATE\n\
                 \x20 CAMERA pos=({:.1},{:.1},{:.1}) zoom={zoom:.1} near={near:.1} far={far:.1} facing={pfacing:.2} desired=({:.1},{:.1},{:.1})\n\
                 \x20 GFX hdr={hdr_on} bloom={bloom_s} dof={dof_s} ssao={ssao_s} motion_blur={mb_s} msaa={msaa} depth_pp={depth_pp} normal_pp={normal_pp} motion_pp={motion_pp}\n\
                 \x20 LIGHT {light_str} shadow_map={shadow_map_size} ambient={ambient_str} clear=({})\n\
                 \x20 QUALITY fixed_high=true rd_base={} rd_dyn={} ssao={} mb={} dof={} bloom={} water={}(amp={:.2}) grass_radius={:.0} foliage_ring_max={} terrain_lod={:.0}\n\
                 \x20 WATER amp={} | ASSETS mesh={mesh_assets} mat={mat_assets} img={img_assets}",
                cam_pos.x, cam_pos.y, cam_pos.z,
                desired.x, desired.y, desired.z,
                rgb(clear.0),
                quality.render_distance_chunks, render_dist_dyn,
                quality.ssao_enabled, quality.motion_blur_enabled, quality.dof_enabled,
                quality.bloom_enabled, quality.water_enabled, quality.water_amplitude,
                quality.grass_radius_world, quality.foliage_ring_max,
                main_world.terrain_lod_distance,
                water_amp.map(|a| format!("{a:.2}")).unwrap_or_else(|| "off".to_string()),
            );
        }

        if is_spike || moving_changed || should_summary || should_state {
            let _ = writer.flush();
        }
    }

    // Borrow of `mon.writer` released; safe to mutate `mon` fields now.
    mon.prev = counts;
    mon.prev_moving = moving;
    if should_summary {
        mon.last_summary_t = elapsed;
        // Reset the per-interval accumulators for the next FRAME window.
        mon.interval_min_ms = f32::INFINITY;
        mon.interval_max_ms = 0.0;
        mon.interval_spikes = 0;
    }
    if should_state {
        mon.last_state_t = elapsed;
    }
}
