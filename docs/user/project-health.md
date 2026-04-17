# Project Health

Open **Export ▼ → Project Health…** to inspect offline media and managed cache usage for the current project.

## What it shows

- **Offline media**: counts for missing source paths used by the project, plus the matching Media Library offline count.
- **Generated caches**: disk usage and file counts for the managed local proxy cache, background prerender cache, background removal cache, frame interpolation cache, voice enhancement cache, clip embedding cache, and auto-tag cache.
- **Alongside-media proxy caches**: project-matching proxy files stored in `UltimateSlice.cache/` directories beside source media are tracked separately from the managed local proxy root.
- **Installed models**: disk usage for clip-search, background-removal, frame-interpolation, and speech-to-text model directories.

Thumbnail previews are intentionally **not** listed here because the current thumbnail cache is in-memory only.

## Offline media workflow

- If any source paths are missing, the dialog lists the first few unresolved paths and enables **Relink Offline Media…**
- That button opens the normal relink flow, which scans a folder recursively and remaps missing paths by filename plus tail-path match.
- After relinking, click **Refresh** to update the snapshot.

## Clearing generated caches

Each generated cache row includes a **Clear cache** button.

- Cleanup only targets files UltimateSlice manages as generated cache output.
- Original source media is never deleted from Project Health.
- Installed model directories are **report-only** in this first pass, so they do not expose cleanup buttons here.

After a cache is cleared, UltimateSlice falls back to the original media or live processing path and rebuilds that cache later if the feature is used again.

## MCP tools

Project Health is also available through MCP:

```bash
python3 tools/mcp_call.py get_project_health '{}'
python3 tools/mcp_call.py cleanup_project_cache '{"cache":"proxy_local"}'
```

Supported cleanup values:

- `proxy_local`
- `proxy_sidecars`
- `prerender`
- `background_removal`
- `frame_interpolation`
- `voice_enhancement`
- `clip_embeddings`
- `auto_tags`
