# Proxy Playback Performance Investigation (3-track Program Monitor)

Sample project: `Sample-Media/three-video-tracks.fcpxml`  
Test focus: quarter proxies + quarter preview quality with a 2x2x2 matrix over:
- hardware acceleration (off/on)
- occlusion optimization (off/on)
- real-time preview (off/on)

## What was added for tracing

- ProgramPlayer logs now include resolved decode path details during slot build/rebuild:
  - source path vs resolved path
  - proxy key
  - whether proxy or fallback-original was used
  - resolved path existence
- Added MCP tool: `set_experimental_preview_optimizations` so occlusion mode can be toggled for automated runs.

## Proxy usage verification

Quarter proxy usage was confirmed from runtime logs in every matrix run (fallback count was `0` in all runs).

Example trace (from run logs):

```text
rebuild_pipeline_at: timeline_pos=9000000000ns active_resolved_sources=[... mode=proxy ...]
ProgramPlayer: slot=0 ... resolved=.../GX010429.proxy_quarter_....mp4 mode=proxy ... resolved_exists=true
ProgramPlayer: slot=1 ... resolved=.../GX010430.proxy_quarter_....mp4 mode=proxy ... resolved_exists=true
ProgramPlayer: slot=2 ... resolved=.../C0378.proxy_quarter.mp4 mode=proxy ... resolved_exists=true
```

## Perf method

- For each combo:
  1. apply preferences via MCP,
  2. seek playhead to `9s` (`9000000000ns`) where 3 video tracks overlap,
  3. start playback,
  4. run `perf stat` for 6s attached to the app PID,
  5. pause playback,
  6. capture incremental app logs for that run.
- Perf counters: `task-clock`, `cycles`, `instructions`, `cache-misses`, `context-switches`, `cpu-migrations`, `page-faults`.

## Matrix results

| Combo | task_clock(s) | Δ vs baseline | IPC | ctx switches | rebuild(avg/max ms) | proxy/fallback | audio-only slots |
|---|---:|---:|---:|---:|---:|---:|---:|
| `hw_false_occ_false_rt_false` | 11.374 | +0.0% | 1.817 | 183,488 | 282.3/339 | 9/0 | 0 |
| `hw_false_occ_false_rt_true` | 12.224 | +7.5% | 1.799 | 236,718 | 303.0/303 | 4/0 | 0 |
| `hw_false_occ_true_rt_false` | 12.181 | +7.1% | 1.668 | 236,856 | 221.3/241 | 9/0 | 4 |
| `hw_false_occ_true_rt_true` | 12.784 | +12.4% | 1.686 | 290,899 | 242.0/242 | 4/0 | 2 |
| `hw_true_occ_false_rt_false` | 11.205 | -1.5% | 1.811 | 194,431 | 247.3/305 | 9/0 | 0 |
| `hw_true_occ_false_rt_true` | 12.226 | +7.5% | 1.799 | 248,364 | 301.0/301 | 4/0 | 0 |
| `hw_true_occ_true_rt_false` | 11.696 | +2.8% | 1.719 | 235,995 | 210.0/229 | 9/0 | 4 |
| `hw_true_occ_true_rt_true` | 12.734 | +12.0% | 1.719 | 293,012 | 242.0/242 | 4/0 | 2 |

## Key findings

1. **Quarter proxies are definitely being used** in this scenario (all runs show proxy mode with existing proxy files, no fallback-original).
2. **Real-time preview increased steady-state CPU load** in this sample (+7% to +12% task-clock), while reducing rebuild count during the sampled window.
3. **Occlusion optimization activated** (audio-only slot counts > 0 when enabled), and it reduced rebuild latency averages, but did not reduce total task-clock in this specific overlap segment.
4. **Hardware acceleration toggle had minimal impact here** (small change only), consistent with runtime capability log indicating no VAAPI availability for source decode path on this system.

## perf record trace (representative baseline run)

Top samples included:
- decode/codec worker activity (`av:h264:*`, pool worker threads),
- memory copy/allocation paths (`__memmove_avx_unaligned_erms`, allocator symbols),
- GTK/GSK/DRM render path activity (renderer + ioctl stack).

This points to a mixed bottleneck: decode + memory movement + rendering overhead.

## Suggested improvements (ranked)

1. **Limit real-time preview prebuild work to boundary windows only** (avoid paying continuous overhead during steady playback).
2. **Increase decoder-slot reuse across boundaries** to reduce full rebuild frequency in 3-track overlap playback.
3. **Make occlusion fast-path cheaper** (avoid extra per-rebuild probing/overhead where possible; cache decisions earlier).
4. **Add renderer-focused profiling pass** (`set_gsk_renderer` matrix: `cairo`/`opengl`/`vulkan`) since render-path cost is visible in perf samples.
5. **Add a repeatable perf harness script in-repo** to standardize playback window, counters, and report generation for future regressions.

## Implementation follow-up (completed)

The top recommendations from this report were implemented:

1. Realtime prewarm work is now gated to active playback near imminent boundaries.
2. Decoder-slot reuse matching was broadened (safe topology/source/effects compatibility) and compositor z-order is explicitly re-applied.
3. Occlusion hot-path checks were made cheaper by using fast cached audio-presence checks in rebuild hot loops.
4. A repeatable perf harness and FPS regression check were added in-repo:
   - `tools/proxy_perf_matrix.sh`
   - `tools/proxy_fps_regression.py`
5. Playback boundary handling now debounces duplicate same-signature rebuild attempts in a short window (~120ms) to reduce transient rebuild churn.
6. Added optional background disk prerender for complex upcoming overlap windows (3+ active tracks), with fallback to normal live rebuild on cache miss and cleanup on close.

## Post-change findings

- Quarter proxies are still confirmed as the active decode input in the overlap window (no proxy fallback observed in traced runs).
- Realtime preview is now less wasteful than before due to prewarm gating, but still carries steady-state overhead in this specific 3-track overlap scenario.
- Reuse + occlusion path changes improved boundary/rebuild behavior without regressing playback progress.
- Relative FPS regression check passes with optimized/baseline median ratio above the configured threshold (`--min-ratio 0.95`), with a representative measured ratio of about `1.007`.

## Raw artifacts

- `/tmp/proxyperf-*/perf_hw_*_occ_*_rt_*.csv`
- `/tmp/proxyperf-*/resp_hw_*_occ_*_rt_*.json`
- `/tmp/proxyperf-*/matrix.csv`

## Follow-up telemetry run (MCP + three-video-tracks) — audio-only/black-video investigation

- Scenario: `open_fcpxml` + quarter proxies + quarter preview + `background_prerender=true`, seek to ~8s overlap region, play through 3-track boundary.
- Added runtime telemetry in `ProgramPlayer`:
  - `background_prerender: queued ...`
  - `background_prerender: ready ...`
  - `background_prerender: failed ...`
- Root cause observed from logs: playback-path arrival waits were effectively capped at `180ms` for 3+ slot rebuilds, causing boundary resume before compositor arrivals in some runs (`wait_for_compositor_arrivals: timeout ... pending slots=...`), which matches user-visible audio continuity with missing/black video.
- Fix applied: increased playback wait cap for 3+ slot rebuild arrival handling (correctness-first), and kept prerender status telemetry for future diagnosis.
- Post-fix telemetry: same MCP scenario now logs `wait_for_compositor_arrivals: all 3 slots arrived` at 3-track boundary crossings and no timeout lines in the sampled run.

### Additional root cause: prerender "ready but not used"

- A second issue was identified in the promotion path: when a prerender segment became ready while playback was already in the overlap range, promotion triggered `rebuild_pipeline_at(...)` but was intercepted by the continue-decoder fast path (`continue_decoders_at`), so prerender slot selection never ran.
- Added diagnostics now clearly show this sequence:
  - `prerender unavailable ... pending=1`
  - `background_prerender: ready ...`
  - `background_prerender: promote requested ...`
  - (previously no `using background prerender segment ...`)
- Fix: promotion now forces a full rebuild (tears down slots before rebuild) so `try_use_background_prerender_slots` executes. Post-fix logs confirm:
  - `background_prerender: promoting live playback to prerender ...`
  - `rebuild_pipeline_at: using background prerender segment ...`
