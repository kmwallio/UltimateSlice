//! Frei0r plugin discovery via GStreamer element factories.
//!
//! At startup, enumerates all `frei0r-filter-*` GStreamer elements, extracts
//! their parameter metadata, and caches the results for the Effects Browser
//! and per-clip effect application.

use gstreamer::prelude::*;
use std::collections::HashMap;

/// Describes a single parameter exposed by a frei0r filter plugin.
#[derive(Debug, Clone)]
pub struct Frei0rParamInfo {
    /// GStreamer property name (e.g. `"Triplevel"`).
    pub name: String,
    /// Human-readable display name (may be the same as `name`).
    pub display_name: String,
    /// Parameter type.
    pub param_type: Frei0rParamType,
    /// Default value (0.0–1.0 for doubles; 0.0 or 1.0 for bools).
    pub default_value: f64,
    /// Minimum value.
    pub min: f64,
    /// Maximum value.
    pub max: f64,
    /// For String params: accepted enum values parsed from GStreamer blurb,
    /// e.g. `["normal", "add", "multiply"]`.  `None` for non-enum strings.
    pub enum_values: Option<Vec<String>>,
    /// For String params: the default string value (e.g. `"normal"`).
    pub default_string: Option<String>,
}

/// Frei0r parameter types supported in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frei0rParamType {
    /// Continuous 0.0–1.0 (most frei0r params).
    Double,
    /// Boolean toggle (0.0 or 1.0).
    Bool,
    /// String parameter (e.g. file paths in some plugins).
    String,
}

/// Metadata for one discovered frei0r filter plugin.
#[derive(Debug, Clone)]
pub struct Frei0rPluginInfo {
    /// Full GStreamer element name (e.g. `"frei0r-filter-cartoon"`).
    pub gst_element_name: String,
    /// Short frei0r name (prefix stripped, e.g. `"cartoon"`).
    pub frei0r_name: String,
    /// FFmpeg frei0r module name (the .so filename without extension,
    /// e.g. `"three_point_balance"`).  Falls back to `frei0r_name` when
    /// the .so cannot be found on disk.
    pub ffmpeg_name: String,
    /// Human-friendly display name (title-cased, hyphens → spaces).
    pub display_name: String,
    /// Description from GStreamer element metadata.
    pub description: String,
    /// Category (from GStreamer element classification, e.g. `"Filter/Effect"`).
    pub category: String,
    /// Parameter descriptors.
    pub params: Vec<Frei0rParamInfo>,
}

/// Cached registry of all discovered frei0r filter plugins.
#[derive(Debug, Clone)]
pub struct Frei0rRegistry {
    pub plugins: Vec<Frei0rPluginInfo>,
    /// Category → indices into `plugins`.
    pub by_category: HashMap<String, Vec<usize>>,
}

const FILTER_PREFIX: &str = "frei0r-filter-";

/// GStreamer properties that are inherited from GstElement / GstObject /
/// GObject and are not frei0r-specific parameters.
const SKIP_PROPERTIES: &[&str] = &["name", "parent", "qos"];

impl Frei0rRegistry {
    /// Discover all available frei0r filter plugins via GStreamer.
    ///
    /// Should be called once at startup after `gstreamer::init()`.
    pub fn discover() -> Self {
        let mut plugins = Vec::new();

        // Build a map from GStreamer-normalized names to actual .so module names
        // so FFmpeg export can reference the correct frei0r module.
        let ffmpeg_name_map = build_ffmpeg_name_map();

        let registry = gstreamer::Registry::get();
        let mut factories: Vec<_> = registry
            .features(gstreamer::ElementFactory::static_type())
            .into_iter()
            .filter_map(|f| f.downcast::<gstreamer::ElementFactory>().ok())
            .filter(|f| {
                let name = f.name();
                name.starts_with(FILTER_PREFIX)
            })
            .collect();

        factories.sort_by(|a, b| a.name().cmp(&b.name()));

        for factory in &factories {
            if let Some(info) = Self::inspect_factory(factory, &ffmpeg_name_map) {
                plugins.push(info);
            }
        }

        let mut by_category: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, plugin) in plugins.iter().enumerate() {
            by_category
                .entry(plugin.category.clone())
                .or_default()
                .push(i);
        }

        log::info!(
            "Frei0r registry: discovered {} filter plugins in {} categories",
            plugins.len(),
            by_category.len(),
        );

        Frei0rRegistry {
            plugins,
            by_category,
        }
    }

    /// Look up a plugin by its short frei0r name.
    pub fn find_by_name(&self, frei0r_name: &str) -> Option<&Frei0rPluginInfo> {
        self.plugins.iter().find(|p| p.frei0r_name == frei0r_name)
    }

    /// Look up a plugin by its full GStreamer element name.
    pub fn find_by_gst_name(&self, gst_name: &str) -> Option<&Frei0rPluginInfo> {
        self.plugins
            .iter()
            .find(|p| p.gst_element_name == gst_name)
    }

    /// Returns sorted, deduplicated category names.
    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self.by_category.keys().cloned().collect();
        cats.sort();
        cats
    }

    /// Returns plugins matching a search query (case-insensitive substring
    /// match on display name or description).
    pub fn search(&self, query: &str) -> Vec<&Frei0rPluginInfo> {
        let q = query.to_ascii_lowercase();
        self.plugins
            .iter()
            .filter(|p| {
                p.display_name.to_ascii_lowercase().contains(&q)
                    || p.description.to_ascii_lowercase().contains(&q)
                    || p.frei0r_name.to_ascii_lowercase().contains(&q)
            })
            .collect()
    }

    fn inspect_factory(
        factory: &gstreamer::ElementFactory,
        ffmpeg_name_map: &HashMap<String, String>,
    ) -> Option<Frei0rPluginInfo> {
        let gst_element_name = factory.name().to_string();
        let frei0r_name = gst_element_name.strip_prefix(FILTER_PREFIX)?.to_string();

        // Create a temporary element to inspect properties.
        let element = factory.create().build().ok()?;

        let metadata = factory.metadata("long-name").unwrap_or_default();
        let display_name = if metadata.is_empty() {
            humanize_name(&frei0r_name)
        } else {
            metadata.to_string()
        };

        let description = factory
            .metadata("description")
            .unwrap_or_default()
            .to_string();

        let klass = factory
            .metadata("klass")
            .unwrap_or_default()
            .to_string();
        let category = simplify_category(&klass);

        let ffmpeg_name = ffmpeg_name_map
            .get(&frei0r_name)
            .cloned()
            .unwrap_or_else(|| frei0r_name.clone());

        let mut params = Vec::new();
        for pspec in element.list_properties() {
            let prop_name = pspec.name().to_string();
            if SKIP_PROPERTIES.contains(&prop_name.as_str()) {
                continue;
            }
            // Skip read-only properties.
            if !pspec.flags().contains(glib::ParamFlags::WRITABLE) {
                continue;
            }
            if let Some(param_info) = inspect_param(&element, &pspec) {
                params.push(param_info);
            }
        }

        Some(Frei0rPluginInfo {
            gst_element_name,
            frei0r_name,
            ffmpeg_name,
            display_name,
            description,
            category,
            params,
        })
    }
}

fn inspect_param(
    element: &gstreamer::Element,
    pspec: &glib::ParamSpec,
) -> Option<Frei0rParamInfo> {
    let name = pspec.name().to_string();
    let display_name = pspec.nick().to_string();

    if let Some(pspec_double) = pspec.downcast_ref::<glib::ParamSpecDouble>() {
        let default_val = element
            .property::<f64>(&name);
        // Sanitize NaN/Inf defaults (e.g. defish0r's "non-linear-scale" defaults to NaN).
        let safe_default = if default_val.is_finite() {
            default_val
        } else {
            let mid = (pspec_double.minimum() + pspec_double.maximum()) / 2.0;
            if mid.is_finite() { mid } else { 0.0 }
        };
        return Some(Frei0rParamInfo {
            display_name,
            name,
            param_type: Frei0rParamType::Double,
            default_value: safe_default,
            min: pspec_double.minimum(),
            max: pspec_double.maximum(),
            enum_values: None,
            default_string: None,
        });
    }

    if let Some(pspec_float) = pspec.downcast_ref::<glib::ParamSpecFloat>() {
        let default_val = element.property::<f32>(&name) as f64;
        let safe_default = if default_val.is_finite() {
            default_val
        } else {
            let mid = (pspec_float.minimum() as f64 + pspec_float.maximum() as f64) / 2.0;
            if mid.is_finite() { mid } else { 0.0 }
        };
        return Some(Frei0rParamInfo {
            display_name,
            name,
            param_type: Frei0rParamType::Double,
            default_value: safe_default,
            min: pspec_float.minimum() as f64,
            max: pspec_float.maximum() as f64,
            enum_values: None,
            default_string: None,
        });
    }

    if let Some(_pspec_bool) = pspec.downcast_ref::<glib::ParamSpecBoolean>() {
        let default_val = if element.property::<bool>(&name) {
            1.0
        } else {
            0.0
        };
        return Some(Frei0rParamInfo {
            display_name,
            name,
            param_type: Frei0rParamType::Bool,
            default_value: default_val,
            min: 0.0,
            max: 1.0,
            enum_values: None,
            default_string: None,
        });
    }

    if let Some(_pspec_str) = pspec.downcast_ref::<glib::ParamSpecString>() {
        let default_str = element
            .property::<Option<String>>(&name)
            .unwrap_or_default();
        let blurb = pspec.blurb().map(|s| s.to_string()).unwrap_or_default();
        let enum_values = parse_accepted_values(&blurb);
        return Some(Frei0rParamInfo {
            display_name,
            name,
            param_type: Frei0rParamType::String,
            default_value: 0.0,
            min: 0.0,
            max: 0.0,
            enum_values,
            default_string: Some(default_str),
        });
    }

    // Skip unsupported types (Color, Position, etc.) for now.
    None
}

/// Build a mapping from GStreamer-normalized frei0r names to the actual .so
/// module names that FFmpeg uses.  Scans standard frei0r-1 directories for
/// `.so` files, loads each to read the plugin's f0r_get_plugin_info name,
/// normalizes it the same way GStreamer does, and records the mapping.
fn build_ffmpeg_name_map() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let frei0r_dirs = [
        "/usr/lib/frei0r-1",
        "/usr/local/lib/frei0r-1",
        "/usr/lib64/frei0r-1",
        "/usr/lib/x86_64-linux-gnu/frei0r-1",
    ];

    #[repr(C)]
    struct Frei0rPluginInfo {
        name: *const std::os::raw::c_char,
        author: *const std::os::raw::c_char,
        plugin_type: std::os::raw::c_int,
        _color_model: std::os::raw::c_int,
        _frei0r_version: std::os::raw::c_int,
        _major_version: std::os::raw::c_int,
        _minor_version: std::os::raw::c_int,
        _num_params: std::os::raw::c_int,
        _explanation: *const std::os::raw::c_char,
    }

    for dir in &frei0r_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("so") {
                continue;
            }
            let Some(so_name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let so_name = so_name.to_string();

            // dlopen the .so to read f0r_get_plugin_info().name
            let c_path =
                match std::ffi::CString::new(path.to_string_lossy().as_bytes().to_vec()) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
            unsafe {
                let handle = libc::dlopen(c_path.as_ptr(), libc::RTLD_LAZY);
                if handle.is_null() {
                    continue;
                }
                let sym_name = b"f0r_get_plugin_info\0";
                let sym = libc::dlsym(handle, sym_name.as_ptr() as *const _);
                if !sym.is_null() {
                    let get_info: extern "C" fn(*mut Frei0rPluginInfo) =
                        std::mem::transmute(sym);
                    let mut info: Frei0rPluginInfo = std::mem::zeroed();
                    get_info(&mut info);
                    if !info.name.is_null() && info.plugin_type == 0 {
                        let real_name =
                            std::ffi::CStr::from_ptr(info.name).to_string_lossy();
                        let gst_name = normalize_frei0r_name(&real_name);
                        if gst_name != so_name {
                            map.insert(gst_name, so_name.clone());
                        }
                    }
                }
                libc::dlclose(handle);
            }
        }
    }
    map
}

/// Normalize a frei0r plugin name the same way GStreamer does:
/// lowercase, non-alphanumeric → hyphens, collapse consecutive hyphens.
fn normalize_frei0r_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_hyphen = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen && !result.is_empty() {
            result.push('-');
            prev_hyphen = true;
        }
    }
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// Parse "Accepted values: 'val1', 'val2', ..." from a GStreamer property blurb.
/// Returns `None` if the pattern is not found.
fn parse_accepted_values(blurb: &str) -> Option<Vec<String>> {
    let marker = "Accepted values:";
    let idx = blurb.find(marker)?;
    let tail = &blurb[idx + marker.len()..];
    let values: Vec<String> = tail
        .split('\'')
        .enumerate()
        .filter_map(|(i, s)| {
            // Odd indices are inside single-quoted strings.
            if i % 2 == 1 && !s.trim().is_empty() {
                Some(s.to_string())
            } else {
                None
            }
        })
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Convert a frei0r name like `"3-point-color-balance"` to `"3 Point Color Balance"`.
fn humanize_name(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    upper + c.as_str()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Simplify GStreamer klass like `"Filter/Effect/Video"` to a short category.
fn simplify_category(klass: &str) -> String {
    // GStreamer frei0r filters have klass like "Filter/Effect/Video",
    // "Filter/Converter/Video", etc.
    let parts: Vec<&str> = klass.split('/').collect();
    if parts.len() >= 2 {
        parts[1].to_string()
    } else {
        "Uncategorized".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_name() {
        assert_eq!(humanize_name("cartoon"), "Cartoon");
        assert_eq!(
            humanize_name("3-point-color-balance"),
            "3 Point Color Balance"
        );
        assert_eq!(humanize_name("coloradj-rgb"), "Coloradj Rgb");
    }

    #[test]
    fn test_simplify_category() {
        assert_eq!(simplify_category("Filter/Effect/Video"), "Effect");
        assert_eq!(simplify_category("Filter/Converter/Video"), "Converter");
        assert_eq!(simplify_category("Filter"), "Uncategorized");
    }

    #[test]
    fn test_parse_accepted_values() {
        let blurb = "Blend mode used to compose gradient on image. Accepted values: 'normal', 'add', 'saturate', 'multiply'";
        let vals = parse_accepted_values(blurb).unwrap();
        assert_eq!(vals, vec!["normal", "add", "saturate", "multiply"]);

        assert!(parse_accepted_values("Just a description").is_none());
        assert!(parse_accepted_values("Accepted values:").is_none());
    }

    #[test]
    fn test_normalize_frei0r_name() {
        assert_eq!(normalize_frei0r_name("3 point color balance"), "3-point-color-balance");
        assert_eq!(normalize_frei0r_name("coloradj_RGB"), "coloradj-rgb");
        assert_eq!(normalize_frei0r_name("B"), "b");
        assert_eq!(normalize_frei0r_name("Cartoon"), "cartoon");
        assert_eq!(normalize_frei0r_name("Color Distance"), "color-distance");
    }

    #[test]
    fn test_build_ffmpeg_name_map() {
        let map = build_ffmpeg_name_map();
        // On a system with frei0r installed, this should find mismatches.
        // The key test: "3-point-color-balance" maps to "three_point_balance".
        if let Some(so_name) = map.get("3-point-color-balance") {
            assert_eq!(so_name, "three_point_balance");
        }
        if let Some(so_name) = map.get("coloradj-rgb") {
            assert_eq!(so_name, "coloradj_RGB");
        }
    }
}
