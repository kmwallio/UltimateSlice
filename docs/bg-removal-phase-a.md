# Background Removal — Phase A Planning (Train Our Own Model)

This document captures the technical details for completing **Phase A** of
the custom background-removal roadmap item: training and exporting our
own portrait-matting ONNX model to replace the third-party MODNet binary
we currently depend on. It's written as a reference for the future
training session — most decisions here can be made in advance of touching
a GPU.

---

## Why this exists

MODNet ships under **Creative Commons BY-NC-SA 4.0** — explicitly
non-commercial. UltimateSlice itself is GPL-3.0 and has no commercial
restriction, but every user who enables the background-removal feature
is using a non-commercially-licensed model. For freelancers and studios
shipping client work that's a real license-compliance problem; we're
effectively gating the feature behind a "personal use only" disclaimer.

Replacing MODNet with a model trained on permissively-licensed data
and released under Apache 2.0 / MIT removes that gate.

---

## What's already shipped (Phase C)

| Piece | Files |
|---|---|
| Manifest abstraction (URL, hash, size, license, filename) | `src/media/model_manifest.rs` |
| SHA-256 verified downloads with real progress UI | `src/media/model_manifest.rs`, `src/ui/preferences.rs` |
| License attribution shown before download | `src/ui/preferences.rs` |
| `bg_removal_cache` reads identity from manifest (no hardcoded URL) | `src/media/bg_removal_cache.rs` |

When Phase A + B finish, the only code change needed is editing the
`PORTRAIT_MATTING` const in `model_manifest.rs` (`url`, `expected_sha256`,
`license_short`, `license_url`) and bumping `expected_size_bytes`. The
inference cache, preview pipeline, FCPXML persistence, and MCP tool
surface stay untouched.

---

## The hard contract

For Phase D to remain a one-line manifest swap, the new ONNX model
**must** match this I/O spec exactly. Deviating from any of these
breaks `run_bg_removal()` in `src/media/bg_removal_cache.rs:345`:

| Aspect | Required value | Source of constraint |
|---|---|---|
| Format | ONNX | `Session::commit_from_file` is the only loader |
| Opset | ≥ 17 (recommended), ≤ 22 | ort 2.0.0-rc.12 supports opset 7-22 |
| Input tensor name | `input` (or update the call site) | `ort::inputs!["input" => …]` |
| Input shape | `[1, 3, 512, 512]` | Pre-resize is hardcoded; dynamic batch is fine |
| Input dtype | `float32` | Cache uses `TensorRef::from_array_view` on `f32` |
| Input layout | NCHW | RGB channel order |
| Input range | `[0.0, 1.0]` | No mean/std normalization in current code |
| Output tensor count | 1 (the alpha matte) | Extra outputs are silently ignored |
| Output shape | `[1, 1, 512, 512]` or compatible | Cache reshapes to 2D matte |
| Output dtype | `float32` | Same |
| Output range | `[0.0, 1.0]` | Threshold is applied as `< threshold * 0.5` |

**Open question:** should we lock the input size to 512×512 (matches
MODNet, simpler) or take this opportunity to make the input dynamic
(e.g. dynamic axis on H/W with `dynamic_axes={"input": {2: "h", 3: "w"}}`
during export)? Dynamic input would let us process higher-resolution
frames without resize loss but would mean a runtime change in
`bg_removal_cache.rs`. **Recommendation:** stay with 512×512 for
Phase A to keep Phase D truly one-line; revisit dynamic input in a
follow-up after we have baseline parity.

---

## Training data — options & license analysis

The hardest part of Phase A isn't the architecture or the training
recipe — it's getting training data we can legally use to produce a
commercially-licensed derivative model.

| Dataset | Size | License | Commercial use? | Notes |
|---|---|---|---|---|
| **P3M-10k** | 10k portrait images + alpha mattes | CC BY 4.0 | ✅ Yes (with attribution) | Best single source. Privacy-preserving (faces blurred in some splits). |
| **AM-2k** | 2k animal matting | CC BY-NC 4.0 | ❌ NC clause | Skip — non-commercial only |
| **AIM-500** | 500 portrait test set | Academic only | ❌ | Eval-only at best, can't train |
| **PPM-100** | 100 portrait test set | Academic only | ❌ | Eval-only at best |
| **VideoMatte-240k** | 240k synthesized frames | Apache 2.0 | ✅ Yes | RVM training set. Composite portraits over backgrounds. |
| **Adobe Image Matting (AIM)** | ~50k composited images | Adobe research license | ❌ | Famously license-restricted |
| **COCO + SAM-derived mattes** | Variable | COCO BY 4.0 + SAM Apache 2.0 | ✅ Yes | Generate alpha via SAM, filter to person-only |
| **Self-composited** | Variable | Whatever we choose | ✅ | Buy/license a portrait photo bank + background bank, composite at training time |
| **OpenImages persons** | Large | CC BY 2.0 | ✅ Yes (with attribution) | Bounding boxes only — need to derive mattes |

**Recommended pipeline:** P3M-10k (real) + VideoMatte-240k (synthetic)
+ COCO+SAM-derived (synthetic). All three are permissively licensed
for commercial derivative work. The mix gives real-photo edge cases
+ large-scale background variation + diverse human poses.

**Open question:** do we want to support animals (pets) in v1? AM-2k
is NC and Adobe's portrait+animal sets are restricted. Skipping
animals for v1 simplifies licensing. **Recommendation:** portrait-only
v1, animal-matting becomes its own roadmap item later.

---

## Model architecture — options & trade-offs

| Architecture | Params | ONNX size | Quality | Inference cost | License | Notes |
|---|---|---|---|---|---|---|
| **MODNet-style** (retrained) | ~6.5M | ~25 MB | Baseline | ~15ms / frame (CPU 512²) | Apache 2.0 (our retrain) | Direct replacement. Branched architecture: semantic + detail + fusion |
| **U²-Net portrait** | ~4.7M | ~18 MB | Slightly worse on hair detail | ~10ms / frame | Apache 2.0 | Smaller, well-supported, single-path |
| **MobileNetV3 + DeepLabV3+ head** | ~3M | ~12 MB | Good for soft segmentation, weak on hair fronds | ~6ms / frame | Apache 2.0 | Smallest; semantic seg → matte refinement is mediocre |
| **RVM** (Robust Video Matting) | ~3.7M | ~14 MB | Best for video (temporal coherence) | ~8ms / frame + state | MIT | Stateful — needs per-frame hidden state, breaks the current "frame in → matte out" call |
| **PP-Matting (PaddleSeg)** | ~7M | ~28 MB | Strong on hair | ~14ms | Apache 2.0 | Trimap-free, but training recipe is PaddlePaddle-specific |

**Recommended architecture:** **Retrain MODNet's architecture** on our
own data. Reasons:
1. Same I/O spec by construction — zero runtime risk for Phase D.
2. The architecture itself is open (MIT/Apache code at `ZHKKKe/MODNet`),
   only the *weights* are CC BY-NC-SA. Training from scratch on legal
   data produces a clean-licensed derivative.
3. We already know it works for the UltimateSlice use case — users have
   tuned their workflow expectations around its quality envelope.
4. ~25 MB file size matches the user expectation set in Preferences.

**Alternative if MODNet retrain underperforms:** swap to U²-Net portrait
variant. Smaller, simpler training recipe, slightly worse hair detail
but most users won't notice on typical talking-head footage.

**Explicit non-choice:** RVM. Its temporal state breaks our stateless
per-frame contract; adopting it would force a Phase D code change.
Defer to a future "Video Matting v2" roadmap item.

---

## Training recipe

Standard alpha-matting training. None of this is novel; documenting
so the next session doesn't have to re-derive.

**Loss function** (sum of four terms, weighted):
- `α_L1`: absolute difference between predicted and ground-truth alpha (weight 1.0)
- `composition_L1`: L1 on `α·FG + (1-α)·BG` reconstructed images (weight 1.0)
- `Laplacian`: pyramid of α differences at 5 scales — captures fine edges (weight 0.5)
- `gradient`: Sobel gradient L1 — sharpens edges (weight 0.5)

(MODNet additionally uses a semantic estimation loss on a low-resolution
branch and a detail-prediction loss on a high-resolution branch. If
we're retraining MODNet's architecture, replicate those.)

**Augmentation pipeline (per training sample):**
- Random crop to 512×512 from a 0.7–1.3× scale of the source
- Horizontal flip (50%)
- Color jitter: brightness ±0.2, contrast ±0.2, saturation ±0.2, hue ±0.05
- Random background swap (composite the GT foreground onto a random background from a background image bank)
- Motion blur 10% (helps generalize to video)
- Random JPEG compression q=50–95 (helps real-world robustness)

**Training schedule (ballpark):**
- Optimizer: AdamW, lr=1e-4, weight decay 1e-4
- Batch size: 16 (on a single 24GB GPU)
- Warmup: 1 epoch linear
- Schedule: cosine to 1e-6 over 100 epochs
- Mixed precision (FP16)
- ~100 epochs × ~250 steps/epoch = 25k iterations ≈ 6-12 hours on a single
  RTX 4090. ~$5-15 of rented A100 time on RunPod/Lambda.

**Validation:** held-out 10% of P3M-10k, no synthetic data in the val
set (we want to measure real-image quality, not augmentation fit).

---

## Export pipeline (PyTorch → ONNX)

```python
# Sketch — exact code lives outside this repo (training repo).
import torch
import onnx

model = load_trained_checkpoint("best.ckpt").eval()

dummy = torch.randn(1, 3, 512, 512)
torch.onnx.export(
    model,
    dummy,
    "modnet_replacement.onnx",
    input_names=["input"],
    output_names=["output"],
    opset_version=17,                       # safe for ort 2.0.0-rc.12
    do_constant_folding=True,
    dynamic_axes=None,                      # static 1×3×512×512 — see contract above
)

# Strip metadata fields that aren't needed at runtime (shrinks file).
m = onnx.load("modnet_replacement.onnx")
m.doc_string = ""
onnx.save(m, "modnet_replacement.onnx")
```

**Validation step (must pass before handing off to Phase B):**

```python
# Round-trip: PyTorch output vs ONNX Runtime output on the same input.
# Per-pixel max absolute difference must be < 1e-4.
import onnxruntime, numpy as np
sess = onnxruntime.InferenceSession("modnet_replacement.onnx")
ort_out = sess.run(["output"], {"input": dummy.numpy()})[0]
torch_out = model(dummy).detach().numpy()
assert np.abs(ort_out - torch_out).max() < 1e-4, "PyTorch/ONNX divergence"
```

**Optional: FP16 / INT8 quantization.** Cuts file size 2-4× and may speed
up inference 1.5-2× on supported hardware. Not required for Phase A —
ship FP32 first, quantize as a follow-up if file size is an issue. INT8
quantization typically loses 1-2% in matte quality which may not be
acceptable for hair edges.

---

## Evaluation methodology

A new model can ship only if it **matches or beats MODNet** on a fixed
test set using the same input pipeline. No vibes-based acceptance.

**Test set:** PPM-100 (100 portrait images with ground-truth mattes,
academic license — fine for eval, just not for training). Held out from
all training. Always evaluate at 512×512 to match the production input.

**Metrics** (lower is better for SAD/MSE/Grad/Conn):

| Metric | What it measures | MODNet baseline (paper) | Pass threshold |
|---|---|---|---|
| SAD (Sum of Absolute Difference) | Overall matte error | ~7.5 | ≤ MODNet × 1.05 |
| MSE (Mean Squared Error) | Outlier-sensitive error | ~0.008 | ≤ MODNet × 1.05 |
| Grad (Gradient error) | Edge sharpness | ~3.1 | ≤ MODNet × 1.10 |
| Conn (Connectivity error) | Connected-region fidelity | ~3.3 | ≤ MODNet × 1.10 |

**Qualitative checks** (visual inspection on a curated 20-image set):
- Hair fronds — fine strands preserved, no white halo
- Eyeglasses / earrings — no spurious holes in foreground
- Translucent fabric (chiffon, lace) — sensible mid-alpha values
- Skin/background color similarity — no leak through (e.g. tan skin against tan wall)
- Profile / partial occlusion — robust to non-frontal poses

**Latency benchmark** (on the developer's RTX 4090):
- CPU (ort default): record ms/frame, must be within ±20% of MODNet
- WebGPU: same
- CUDA: same

**Failure modes to track** (test images that stress these):
- Long hair against a busy background
- Hands at the edge of the frame
- Dark clothing on dark background

---

## Phase A handoff artifacts → Phase B

A successful Phase A produces this drop:

```
ultimateslice-portrait-matting-v1/
├── model.onnx                 # The trained model, opset 17, static 1×3×512×512
├── model.onnx.sha256          # `sha256sum model.onnx > model.onnx.sha256`
├── LICENSE                    # Apache 2.0 or MIT (whichever we choose)
├── README.md                  # Brief — model card style
├── EVAL_REPORT.md             # SAD/MSE/Grad/Conn vs MODNet on PPM-100
├── training-recipe.md         # Hyperparameters, data sources, augmentation
└── attribution.md             # Which datasets contributed (P3M-10k, etc.)
```

`model.onnx` + `model.onnx.sha256` + the file size in bytes are
everything Phase B needs to start: those three plus the chosen hosting
URL go into the `PORTRAIT_MATTING` manifest entry.

---

## Time & cost estimate

Calendar time, assuming one engineer with ML experience working part-time:

| Step | Time | $ |
|---|---|---|
| Data acquisition (download, license-audit, split) | 2-3 days | $0 |
| Training pipeline setup (loss, data loader, eval script) | 3-5 days | $0 |
| First training run + debug | 1-2 days, ~12 GPU-hours | $5-15 (rented A100) |
| Eval against PPM-100, iterate on data mix | 1-2 weeks, 3-5 training runs | $30-75 |
| ONNX export + round-trip validation | 1 day | $0 |
| Quality bar passed → handoff | — | — |
| **Total** | **3-4 weeks part-time** | **~$50-100 in compute** |

Worst case (model doesn't reach parity with MODNet on first architecture):
add 1-2 weeks for trying U²-Net portrait as fallback. Still cheap.

---

## Open questions to resolve before training starts

1. **Output license — Apache 2.0 or MIT?** Both work. Apache 2.0 has patent grant which is mildly preferable for users in defensive-patent contexts. MIT is shorter and arguably more popular. **Recommendation:** Apache 2.0.
2. **Hosting target — GitHub Releases / Cloudflare R2 / S3?** Affects Phase B, not A, but worth deciding early. GitHub Releases is free + has stable URLs but caps individual file uploads at 2 GB (no issue at 25 MB). R2 has no egress fees. **Recommendation:** GitHub Releases for v1, revisit if traffic grows.
3. **Where does the training repo live?** A separate `ultimateslice-models` repo (training scripts + dataset prep + eval), or in-tree as a `training/` subdirectory? **Recommendation:** separate repo — keeps the main project's `cargo check` time stable, lets ML deps (PyTorch, CUDA) live separately from Rust deps.
4. **Should we publish the trained checkpoint, or only the ONNX?** Publishing the PyTorch checkpoint enables community fine-tuning but doubles the storage cost. **Recommendation:** ONNX only for v1, publish the checkpoint if community asks.
5. **Animal matting deferred — formal roadmap entry?** Probably worth adding `bg-removal-animal-matting-v1` to ROADMAP.md when Phase A lands so we don't lose track.

---

## Non-goals for Phase A (deferred to follow-ups)

- **Video temporal coherence (RVM-style).** Would force a Phase D code change to add hidden-state plumbing. Document as `bg-removal-temporal-v2`.
- **Dynamic input resolution.** Same reason as above. Document as `bg-removal-multi-res-v2`.
- **INT8 quantization.** Premature; ship FP32 first, measure if file size is a real complaint.
- **Trimap-guided matting.** Different UX, different I/O contract.
- **Real-time WebGPU optimization.** Phase A optimizes for quality parity; perf comes later.
