@echo off
REM ── inf3d run helper ────────────────────────────────────────────────────
REM Run the game with:   .\run.cmd        (works from PowerShell too)
REM
REM Writes a FRESH run.log every launch (the single ">" truncates it), so the
REM assistant never reads compounded, million-line logs. run.log captures BOTH
REM the cargo build output AND the bevy runtime console — frame diagnostics,
REM asset loads, and shader/pipeline (naga/wgpu) errors print to the console,
REM NOT to inf3d-monitor.log. The monitor plugin already overwrites
REM inf3d-monitor.log fresh each run (per-frame SPIKE/SUMMARY telemetry).
cargo run -p inf3d_app > run.log 2>&1
