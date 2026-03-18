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
            if let Some(info) = Self::inspect_factory(factory) {
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

    fn inspect_factory(factory: &gstreamer::ElementFactory) -> Option<Frei0rPluginInfo> {
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
        });
    }

    if let Some(_pspec_str) = pspec.downcast_ref::<glib::ParamSpecString>() {
        return Some(Frei0rParamInfo {
            display_name,
            name,
            param_type: Frei0rParamType::String,
            default_value: 0.0,
            min: 0.0,
            max: 0.0,
        });
    }

    // Skip unsupported types (Color, Position, etc.) for now.
    None
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
}
