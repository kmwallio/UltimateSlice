//! LADSPA audio plugin discovery via GStreamer element factories.
//!
//! Enumerates all `ladspa-*` GStreamer elements (excluding `ladspasrc-*` sources),
//! extracts their parameter metadata, and caches the results for the audio effects
//! browser and per-clip effect application.

use gstreamer::prelude::*;
use std::collections::HashMap;
use std::sync::OnceLock;

static LADSPA_REGISTRY: OnceLock<LadspaRegistry> = OnceLock::new();

/// Describes a single parameter exposed by a LADSPA audio plugin.
#[derive(Debug, Clone)]
pub struct LadspaParamInfo {
    /// GStreamer property name (e.g. `"gain"`).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Default value.
    pub default_value: f64,
    /// Minimum value.
    pub min: f64,
    /// Maximum value.
    pub max: f64,
}

/// Metadata for a single LADSPA audio plugin.
#[derive(Debug, Clone)]
pub struct LadspaPluginInfo {
    /// Full GStreamer element factory name (e.g. `"ladspa-amp-so-amp-stereo"`).
    pub gst_element_name: String,
    /// Short identifier derived from the element name.
    pub ladspa_name: String,
    /// Human-readable display name from GStreamer metadata.
    pub display_name: String,
    /// Description from GStreamer metadata.
    pub description: String,
    /// Simplified category (e.g. `"Effect"`, `"Filter"`).
    pub category: String,
    /// Discovered controllable parameters.
    pub params: Vec<LadspaParamInfo>,
}

/// Cached registry of all discovered LADSPA audio plugins.
#[derive(Clone)]
pub struct LadspaRegistry {
    pub plugins: Vec<LadspaPluginInfo>,
    pub by_category: HashMap<String, Vec<usize>>,
}

impl LadspaRegistry {
    /// Get or lazily build the singleton registry.
    pub fn get_or_discover() -> &'static Self {
        LADSPA_REGISTRY.get_or_init(Self::discover)
    }

    /// Discover all LADSPA filter plugins from the GStreamer registry.
    pub fn discover() -> Self {
        let registry = gstreamer::Registry::get();
        let mut plugins = Vec::new();
        let mut by_category: HashMap<String, Vec<usize>> = HashMap::new();

        let factories: Vec<gstreamer::ElementFactory> = registry
            .features(gstreamer::ElementFactory::static_type())
            .into_iter()
            .filter_map(|f| f.downcast::<gstreamer::ElementFactory>().ok())
            .filter(|f| {
                let name = f.name().to_string();
                if !name.starts_with("ladspa-") || name.starts_with("ladspasrc-") {
                    return false;
                }
                // Skip source/generator plugins (oscillators, noise) — only keep filters.
                let klass = f.metadata("klass").unwrap_or_default().to_string();
                klass.contains("Filter") || klass.contains("Effect")
            })
            .collect();

        for factory in &factories {
            if let Some(info) = inspect_factory(factory) {
                let idx = plugins.len();
                by_category
                    .entry(info.category.clone())
                    .or_default()
                    .push(idx);
                plugins.push(info);
            }
        }

        log::info!(
            "LADSPA registry: discovered {} audio plugins in {} categories",
            plugins.len(),
            by_category.len()
        );

        Self {
            plugins,
            by_category,
        }
    }

    /// Sorted list of categories.
    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self.by_category.keys().cloned().collect();
        cats.sort();
        cats
    }

    /// Find a plugin by its short `ladspa_name`.
    pub fn find_by_name(&self, ladspa_name: &str) -> Option<&LadspaPluginInfo> {
        self.plugins.iter().find(|p| p.ladspa_name == ladspa_name)
    }

    /// Find a plugin by its full GStreamer element name.
    pub fn find_by_gst_name(&self, gst_name: &str) -> Option<&LadspaPluginInfo> {
        self.plugins.iter().find(|p| p.gst_element_name == gst_name)
    }

    /// Case-insensitive substring search across display name and description.
    pub fn search(&self, query: &str) -> Vec<&LadspaPluginInfo> {
        let q = query.to_ascii_lowercase();
        self.plugins
            .iter()
            .filter(|p| {
                p.display_name.to_ascii_lowercase().contains(&q)
                    || p.description.to_ascii_lowercase().contains(&q)
                    || p.ladspa_name.to_ascii_lowercase().contains(&q)
            })
            .collect()
    }
}

fn inspect_factory(factory: &gstreamer::ElementFactory) -> Option<LadspaPluginInfo> {
    let gst_element_name = factory.name().to_string();

    // Derive short name: strip "ladspa-" prefix and normalize.
    let ladspa_name = gst_element_name
        .strip_prefix("ladspa-")
        .unwrap_or(&gst_element_name)
        .to_string();

    let element = factory.create().build().ok()?;

    let display_name = factory
        .metadata("long-name")
        .map(|s| s.to_string())
        .unwrap_or_else(|| ladspa_name.clone());
    let description = factory
        .metadata("description")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let klass = factory
        .metadata("klass")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let category = simplify_category(&klass);

    // Discover controllable parameters.
    let skip_props = [
        "name",
        "parent",
        "qos",
        "latency",
        "last-message",
        "message-forward",
    ];
    let params: Vec<LadspaParamInfo> = element
        .list_properties()
        .into_iter()
        .filter(|pspec| {
            let name = pspec.name().to_string();
            !skip_props.contains(&name.as_str())
                && pspec.flags().contains(glib::ParamFlags::WRITABLE)
                && !pspec.flags().contains(glib::ParamFlags::CONSTRUCT_ONLY)
        })
        .filter_map(|pspec| inspect_param(&pspec))
        .collect();

    Some(LadspaPluginInfo {
        gst_element_name,
        ladspa_name,
        display_name,
        description,
        category,
        params,
    })
}

fn inspect_param(pspec: &glib::ParamSpec) -> Option<LadspaParamInfo> {
    let name = pspec.name().to_string();
    let display_name = pspec.nick().to_string();

    if let Some(spec) = pspec.downcast_ref::<glib::ParamSpecDouble>() {
        let min = sanitize_bound(spec.minimum(), 0.0);
        let max = sanitize_bound(spec.maximum(), 1.0);
        let default = spec.default_value().clamp(min, max);
        Some(LadspaParamInfo {
            name,
            display_name,
            default_value: default,
            min,
            max,
        })
    } else if let Some(spec) = pspec.downcast_ref::<glib::ParamSpecFloat>() {
        let min = sanitize_bound(spec.minimum() as f64, 0.0);
        let max = sanitize_bound(spec.maximum() as f64, 1.0);
        let default = (spec.default_value() as f64).clamp(min, max);
        Some(LadspaParamInfo {
            name,
            display_name,
            default_value: default,
            min,
            max,
        })
    } else if let Some(spec) = pspec.downcast_ref::<glib::ParamSpecInt>() {
        Some(LadspaParamInfo {
            name,
            display_name,
            default_value: spec.default_value() as f64,
            min: spec.minimum() as f64,
            max: spec.maximum() as f64,
        })
    } else if let Some(spec) = pspec.downcast_ref::<glib::ParamSpecBoolean>() {
        Some(LadspaParamInfo {
            name,
            display_name,
            default_value: if spec.default_value() { 1.0 } else { 0.0 },
            min: 0.0,
            max: 1.0,
        })
    } else {
        None
    }
}

/// Sanitize a parameter bound: clamp extreme/non-finite values.
fn sanitize_bound(val: f64, fallback: f64) -> f64 {
    if val.is_finite() && val.abs() < 1e15 {
        val
    } else {
        fallback
    }
}

/// Extract a simplified category from the GStreamer klass string.
fn simplify_category(klass: &str) -> String {
    // "Filter/Effect/Audio/LADSPA" → "Effect"
    klass.split('/').nth(1).unwrap_or("Audio").to_string()
}
