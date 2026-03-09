# AI Models for UltimateSlice

This directory contains ONNX model files used by UltimateSlice's AI features.

## Background Removal

**Required model:** `modnet_photographic_portrait_matting.onnx`

Download from the MODNet project:
- GitHub: https://github.com/ZHKKKe/MODNet
- ONNX model (Google Drive): https://drive.google.com/file/d/1cgycTQlYXpTh26gB9FTnthE7AvruV8hd/view?usp=sharing

Place the `.onnx` file in this directory, or use **Preferences → Models** to download it automatically.

### Expected input/output

- **Input:** `input` tensor, shape `[1, 3, 512, 512]`, float32, RGB normalized to [0, 1]
- **Output:** `output` tensor, shape `[1, 1, 512, 512]`, float32, alpha matte [0, 1]
