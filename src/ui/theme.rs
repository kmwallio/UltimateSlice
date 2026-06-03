//! Runtime theming.
//!
//! The app stylesheet (`src/style.css`) is written against a set of
//! `@define-color` tokens (`@bg0`, `@text`, `@accent`, …) rather than literal
//! hex. This module supplies the token *values* at runtime: it picks a
//! light or dark palette based on the user's [`ThemeMode`], folds in the chosen
//! accent color (deriving hover/active shades), concatenates the shared rule
//! body, and loads the result into a single [`gtk4::CssProvider`] held for the
//! life of the process. Re-applying just re-loads that provider — no restart.
//!
//! `ThemeMode::System` follows the desktop's light/dark preference via the
//! XDG desktop portal (`org.freedesktop.appearance` / `color-scheme`), and we
//! subscribe to the portal's `SettingChanged` signal so the UI flips live when
//! the OS theme toggles. When the portal is unavailable we fall back to dark.
//!
//! Only the GTK *chrome* is themed here. The timeline / program-monitor /
//! scopes are Cairo-drawn and intentionally stay on their dark professional
//! palette; the one exception is the clip-selection highlight, whose accent is
//! kept in sync via [`crate::ui::colors::set_selection_accent`].

use std::cell::RefCell;

use gtk4::prelude::*;

use crate::ui_state::ThemeMode;

/// The shared, tokenized rule body. Contains no `@define-color` of its own —
/// every palette value comes from the header this module prepends.
const BODY: &str = include_str!("../style.css");

/// `@define-color` header for the dark palette (accent tokens are appended
/// separately by [`build_css`]).
const DARK_HEADER: &str = "\
@define-color bg0 #131315;\n\
@define-color bg1 #1a1a1c;\n\
@define-color bg2 #242428;\n\
@define-color bg3 #2e2e33;\n\
@define-color bg_hover #2a2a2e;\n\
@define-color border #333338;\n\
@define-color border_strong #444450;\n\
@define-color text #e0e0e0;\n\
@define-color text_dim #aaaaaa;\n\
@define-color text_bright #ffffff;\n";

/// `@define-color` header for the light palette.
const LIGHT_HEADER: &str = "\
@define-color bg0 #fafafb;\n\
@define-color bg1 #f3f3f5;\n\
@define-color bg2 #ffffff;\n\
@define-color bg3 #e9e9ec;\n\
@define-color bg_hover #dcdce1;\n\
@define-color border #cfcfd6;\n\
@define-color border_strong #b4b4be;\n\
@define-color text #1c1c20;\n\
@define-color text_dim #5c5c66;\n\
@define-color text_bright #000000;\n";

const DEFAULT_ACCENT: &str = "#1565c0";

/// Concrete light/dark choice after `System` has been resolved.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Resolved {
    Light,
    Dark,
}

struct ThemeState {
    provider: gtk4::CssProvider,
    mode: ThemeMode,
    accent: String,
}

thread_local! {
    static STATE: RefCell<Option<ThemeState>> = const { RefCell::new(None) };
    static DBUS: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
}

/// Create the shared provider, attach it to the default display, apply the
/// saved theme, and start watching the system color scheme. Call once at
/// startup, before the first window is shown.
pub fn init(mode: ThemeMode, accent: &str) {
    let provider = gtk4::CssProvider::new();
    if let Some(display) = gdk4::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(ThemeState {
            provider,
            mode,
            accent: accent.to_string(),
        });
    });
    crate::ui::colors::set_selection_accent(accent);
    render(resolve(mode), accent);
    // Always subscribe; the callback only acts while the user is on System,
    // so switching to System later still picks up live OS changes.
    start_watch();
}

/// Apply a (possibly changed) theme mode + accent live. Safe to call from the
/// Preferences save path; no restart required.
pub fn apply(mode: ThemeMode, accent: &str) {
    render(resolve(mode), accent);
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.mode = mode;
            st.accent = accent.to_string();
        }
    });
    crate::ui::colors::set_selection_accent(accent);
}

/// Build the full stylesheet string for a resolved palette + accent.
fn build_css(resolved: Resolved, accent_hex: &str) -> String {
    let accent = normalize_hex(accent_hex);
    let hover = shade(&accent, 0.12); // lighten
    let active = shade(&accent, -0.15); // darken
    let header = match resolved {
        Resolved::Dark => DARK_HEADER,
        Resolved::Light => LIGHT_HEADER,
    };
    format!(
        "{header}\
@define-color accent {accent};\n\
@define-color accent_hover {hover};\n\
@define-color accent_active {active};\n\
@define-color accent_fg #ffffff;\n\
{BODY}"
    )
}

/// Re-load the live provider with a freshly built stylesheet and align the
/// native GTK light/dark hint with the resolved palette.
fn render(resolved: Resolved, accent: &str) {
    let css = build_css(resolved, accent);
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            st.provider.load_from_string(&css);
        }
    });
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_property(
            "gtk-application-prefer-dark-theme",
            resolved == Resolved::Dark,
        );
    }
}

fn resolve(mode: ThemeMode) -> Resolved {
    match mode {
        ThemeMode::Light => Resolved::Light,
        ThemeMode::Dark => Resolved::Dark,
        ThemeMode::System => resolve_system().unwrap_or(Resolved::Dark),
    }
}

/// Normalize a user hex string to canonical `#rrggbb`, falling back to the
/// default accent on malformed input.
fn normalize_hex(hex: &str) -> String {
    match crate::ui::colors::parse_hex_rgb(hex) {
        Some((r, g, b)) => format!(
            "#{:02x}{:02x}{:02x}",
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8
        ),
        None => DEFAULT_ACCENT.to_string(),
    }
}

/// Mix a hex color toward white (`t > 0`) or black (`t < 0`) by `|t|`.
fn shade(hex: &str, t: f64) -> String {
    let (r, g, b) = crate::ui::colors::parse_hex_rgb(hex).unwrap_or((0.0823, 0.3960, 0.7529));
    let mix = |c: f64| {
        if t >= 0.0 {
            c + (1.0 - c) * t
        } else {
            c * (1.0 + t)
        }
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        (mix(r) * 255.0).round() as u8,
        (mix(g) * 255.0).round() as u8,
        (mix(b) * 255.0).round() as u8
    )
}

// ─── System color-scheme (XDG desktop portal) ─────────────────────────────

/// Map the portal `color-scheme` enum to a resolved palette. `0` (no
/// preference) returns `None` so callers apply their own default.
fn scheme_to_resolved(value: u32) -> Option<Resolved> {
    match value {
        1 => Some(Resolved::Dark),
        2 => Some(Resolved::Light),
        _ => None,
    }
}

/// Lazily-cached session bus connection (kept alive for signal subscription).
fn session_bus() -> Option<gio::DBusConnection> {
    DBUS.with(|d| {
        if d.borrow().is_none() {
            if let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
                *d.borrow_mut() = Some(conn);
            }
        }
        d.borrow().clone()
    })
}

/// Unwrap any nesting of `v` (variant) boxes and read a `u32`.
fn variant_u32(mut v: glib::Variant) -> Option<u32> {
    while let Some(inner) = v.as_variant() {
        v = inner;
    }
    v.get::<u32>()
}

/// Synchronously read `org.freedesktop.appearance / color-scheme` from the
/// portal. Returns `None` if the portal is missing or expresses no preference.
fn resolve_system() -> Option<Resolved> {
    let conn = session_bus()?;
    let params = ("org.freedesktop.appearance", "color-scheme").to_variant();
    let reply = conn
        .call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "Read",
            Some(&params),
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    scheme_to_resolved(variant_u32(reply.child_value(0))?)
}

/// Subscribe to the portal's `SettingChanged` signal and re-apply the theme
/// when the OS color scheme changes (only while the user is on System).
fn start_watch() {
    let Some(conn) = session_bus() else {
        log::debug!("theme: no session bus; system-follow disabled");
        return;
    };
    // `subscribe_to_signal` is the 4.x successor but isn't in our gio baseline.
    #[allow(deprecated)]
    conn.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Settings"),
        Some("SettingChanged"),
        Some("/org/freedesktop/portal/desktop"),
        None,
        gio::DBusSignalFlags::NONE,
        move |_conn, _sender, _path, _iface, _signal, params| {
            // params: (s namespace, s key, v value)
            let ns = params.child_value(0).get::<String>().unwrap_or_default();
            let key = params.child_value(1).get::<String>().unwrap_or_default();
            if ns != "org.freedesktop.appearance" || key != "color-scheme" {
                return;
            }
            let Some(scheme) = variant_u32(params.child_value(2)) else {
                return;
            };
            let (follow, accent) = STATE.with(|s| {
                let b = s.borrow();
                match b.as_ref() {
                    Some(st) => (st.mode == ThemeMode::System, st.accent.clone()),
                    None => (false, DEFAULT_ACCENT.to_string()),
                }
            });
            if !follow {
                return;
            }
            render(scheme_to_resolved(scheme).unwrap_or(Resolved::Dark), &accent);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_css_includes_palette_and_accent_tokens() {
        let dark = build_css(Resolved::Dark, "#1565c0");
        assert!(dark.contains("@define-color bg0 #131315;"));
        assert!(dark.contains("@define-color accent #1565c0;"));
        assert!(dark.contains("@define-color accent_hover"));
        assert!(dark.contains("@define-color accent_active"));
        // The shared body is appended after the header.
        assert!(dark.contains("headerbar"));

        let light = build_css(Resolved::Light, "#1565c0");
        assert!(light.contains("@define-color bg0 #fafafb;"));
        assert!(light.contains("@define-color text #1c1c20;"));
    }

    #[test]
    fn malformed_accent_falls_back() {
        assert_eq!(normalize_hex("not-a-color"), DEFAULT_ACCENT);
        assert_eq!(normalize_hex("#1565c0"), "#1565c0");
    }

    #[test]
    fn shade_lightens_and_darkens() {
        // Black lightened by 1.0 → white; white darkened by 1.0 → black.
        assert_eq!(shade("#000000", 1.0), "#ffffff");
        assert_eq!(shade("#ffffff", -1.0), "#000000");
        // Hover is lighter than the base, active is darker.
        let base = (0.0823_f64, 0.3960, 0.7529);
        let hover = crate::ui::colors::parse_hex_rgb(&shade("#1565c0", 0.12)).unwrap();
        let active = crate::ui::colors::parse_hex_rgb(&shade("#1565c0", -0.15)).unwrap();
        assert!(hover.2 > base.2);
        assert!(active.2 < base.2);
    }

    #[test]
    fn scheme_maps_to_palette() {
        assert!(matches!(scheme_to_resolved(1), Some(Resolved::Dark)));
        assert!(matches!(scheme_to_resolved(2), Some(Resolved::Light)));
        assert!(scheme_to_resolved(0).is_none());
    }
}
