//! Keyboard shortcut registry: the single source of truth for every
//! user-rebindable editing shortcut.
//!
//! * [`ActionId`] / [`ACTIONS`] — every action with its title, context,
//!   category and default chords.
//! * [`Chord`] — a normalised key + modifier combination with a GTK-style
//!   accelerator syntax (`<Control><Shift>z`) and a display label
//!   (`Ctrl+Shift+Z`). Parsing/formatting is implemented here because GTK's
//!   `accelerator_parse` asserts an initialised main thread and cannot run in
//!   unit tests; only `gdk::Key` helpers (which do not) are used.
//! * [`Keymap`] — defaults + user overrides, resolution of a chord in a
//!   context (context first, then Global), conflict detection.
//! * [`KeymapOverrides`] — the on-disk format of
//!   `~/.config/ultimateslice/keybindings.json` (overrides only).
//! * [`ActionMap`] — the handler table the UI registers closures into and
//!   that key dispatch, the command palette and MCP `trigger_action` call.
//! * [`render_shortcuts_markdown`] — generates `docs/user/shortcuts.md`.
//!
//! Rule (see docs/ARCHITECTURE.md): adding a shortcut in the Global,
//! Timeline or Source Monitor context means adding an `ActionId`, an
//! `ActionInfo` entry and an `ActionMap::bind` registration — never a raw
//! `connect_key_pressed` match.

use gtk4::gdk;
use gtk4::glib::translate::IntoGlib;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

// ── Contexts ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionContext {
    /// Handled on the main window (and the popped-out Program Monitor) in
    /// the Capture phase, regardless of which panel has focus.
    Global,
    /// Handled when the timeline area has focus.
    Timeline,
    /// Handled when the Source Monitor panel has focus.
    SourceMonitor,
}

impl ActionContext {
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Timeline => "Timeline",
            Self::SourceMonitor => "Source Monitor",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Timeline => "timeline",
            Self::SourceMonitor => "source_monitor",
        }
    }

    pub const ALL: &'static [ActionContext] =
        &[Self::Global, Self::Timeline, Self::SourceMonitor];
}

// ── Actions ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionId {
    // Global
    NewProject,
    OpenProject,
    SaveProject,
    ShuttleReverse,
    ShuttleStop,
    ShuttleForward,
    AddMarker,
    ToggleMinimap,
    ToggleHud,
    InsertAtPlayhead,
    OverwriteAtPlayhead,
    GoToTimecode,
    OpenPreferences,
    ToggleProxyPlayback,
    GenerateSubtitles,
    CommandPalette,
    PrevKeyframe,
    NextKeyframe,
    ToggleAnimationMode,
    // Timeline
    Undo,
    Redo,
    SelectAll,
    Copy,
    PasteInsert,
    PasteAttributes,
    CopyColorGrade,
    PasteColorGrade,
    PasteColorGradeAll,
    MatchColor,
    Group,
    Ungroup,
    Link,
    Unlink,
    CreateCompound,
    CreateMulticam,
    Delete,
    RippleDelete,
    KeyframeInterpLinear,
    KeyframeInterpEaseIn,
    KeyframeInterpEaseOut,
    KeyframeInterpEaseInOut,
    SelectForward,
    SelectBackward,
    ToggleTrackSolo,
    ToggleTrackMute,
    ToggleTrackLock,
    PlayPause,
    MatchFrame,
    FreezeFrame,
    PrecisionTrim,
    JoinThroughEdit,
    ToolRazor,
    ToolRipple,
    ToolRoll,
    ToolSlip,
    ToolSlide,
    ToolDraw,
    EscapeCancel,
    ShowShortcuts,
    // Source Monitor
    SourceMarkIn,
    SourceMarkOut,
    SourcePlayPause,
    SourceShuttleReverse,
    SourceShuttleStop,
    SourceShuttleForward,
    SourceStepBack,
    SourceStepForward,
}

/// Static metadata for one action.
pub struct ActionInfo {
    pub id: ActionId,
    /// Stable snake_case identifier used in `keybindings.json` and MCP.
    pub key: &'static str,
    /// Short title for the Preferences table, palette and `?` dialog.
    pub title: &'static str,
    /// Longer description for the generated docs page ("" = use `title`).
    pub doc: &'static str,
    pub context: ActionContext,
    pub category: &'static str,
    /// Default chords in accelerator syntax; empty = unbound by default.
    pub defaults: &'static [&'static str],
}

use ActionContext::{Global, SourceMonitor, Timeline};
use ActionId::*;

macro_rules! action {
    ($id:ident, $key:literal, $ctx:expr, $cat:literal, $title:literal, [$($d:literal),*]) => {
        ActionInfo { id: $id, key: $key, title: $title, doc: "", context: $ctx, category: $cat, defaults: &[$($d),*] }
    };
    ($id:ident, $key:literal, $ctx:expr, $cat:literal, $title:literal, $doc:literal, [$($d:literal),*]) => {
        ActionInfo { id: $id, key: $key, title: $title, doc: $doc, context: $ctx, category: $cat, defaults: &[$($d),*] }
    };
}

/// Every rebindable action, in display order.
pub const ACTIONS: &[ActionInfo] = &[
    // ── Global ────────────────────────────────────────────────────────────
    action!(NewProject, "new_project", Global, "Project", "New project", ["<Control>n"]),
    action!(OpenProject, "open_project", Global, "Project", "Open project", "Open project XML (`.uspxml` / `.fcpxml`)", ["<Control>o"]),
    action!(SaveProject, "save_project", Global, "Project", "Save project", "Save project XML (default `.uspxml`)", ["<Control>s"]),
    action!(OpenPreferences, "open_preferences", Global, "Project", "Open Preferences", ["<Control>comma"]),
    action!(CommandPalette, "command_palette", Global, "Project", "Command palette", ["<Control><Shift>p"]),
    action!(ShuttleReverse, "shuttle_reverse", Global, "Transport", "Shuttle reverse", "Shuttle the Program Monitor in reverse (1× → 2× → 4× → 8×)", ["j"]),
    action!(ShuttleStop, "shuttle_stop", Global, "Transport", "Stop shuttle", "Stop shuttle / pause the Program Monitor", ["k"]),
    action!(ShuttleForward, "shuttle_forward", Global, "Transport", "Shuttle forward", "Shuttle the Program Monitor forward (1× → 2× → 4× → 8×)", ["l"]),
    action!(GoToTimecode, "go_to_timecode", Global, "Transport", "Go to timecode", "Go to timecode (jump playhead)", ["<Control>j"]),
    action!(InsertAtPlayhead, "insert_at_playhead", Global, "Editing", "Insert at playhead", "Insert the Source Monitor selection at the playhead (shift subsequent clips)", ["comma"]),
    action!(OverwriteAtPlayhead, "overwrite_at_playhead", Global, "Editing", "Overwrite at playhead", "Overwrite at the playhead with the Source Monitor selection (replace existing material)", ["period"]),
    action!(AddMarker, "add_marker", Global, "Markers", "Add marker", "Add a chapter marker at the playhead", ["m"]),
    action!(PrevKeyframe, "prev_keyframe", Global, "Keyframes", "Previous keyframe", "Jump to the previous keyframe of the selected clip", ["<Alt>Left"]),
    action!(NextKeyframe, "next_keyframe", Global, "Keyframes", "Next keyframe", "Jump to the next keyframe of the selected clip", ["<Alt>Right"]),
    action!(ToggleAnimationMode, "toggle_animation_mode", Global, "Keyframes", "Toggle Record Keyframes", "Toggle Inspector animation mode (Record Keyframes)", ["<Shift>k"]),
    action!(ToggleMinimap, "toggle_minimap", Global, "View", "Toggle timeline mini-map", ["<Shift>m"]),
    action!(ToggleHud, "toggle_hud", Global, "View", "Toggle Program Monitor HUD", "Toggle the Program Monitor HUD overlay (timecode / frame / fps / resolution / dropped)", ["<Shift>h"]),
    action!(ToggleProxyPlayback, "toggle_proxy_playback", Global, "View", "Toggle proxy playback", "Toggle proxy playback on/off (switches the status bar between `Original Media` and `Using Proxies`, restoring the last non-Off proxy size)", ["<Shift>p"]),
    action!(GenerateSubtitles, "generate_subtitles", Global, "Tools", "Generate subtitles", "Generate subtitles (speech-to-text) for the selected clip", ["<Control><Shift>t"]),
    // ── Timeline ──────────────────────────────────────────────────────────
    action!(PlayPause, "play_pause", Timeline, "Transport", "Play / Pause", "Play / pause the Program Monitor", ["space"]),
    action!(Undo, "undo", Timeline, "Editing", "Undo", "Undo — shows a brief `Undid: …` toast", ["<Control>z"]),
    action!(Redo, "redo", Timeline, "Editing", "Redo", "Redo — shows a brief `Redid: …` toast", ["<Control><Shift>z", "<Control>y"]),
    action!(SelectAll, "select_all", Timeline, "Selection", "Select all clips", ["<Control>a"]),
    action!(SelectForward, "select_forward", Timeline, "Selection", "Select clips forward from playhead", ["<Control><Shift>Right"]),
    action!(SelectBackward, "select_backward", Timeline, "Selection", "Select clips backward from playhead", ["<Control><Shift>Left"]),
    action!(Copy, "copy", Timeline, "Editing", "Copy clip", "Copy selected timeline clip", ["<Control>c"]),
    action!(PasteInsert, "paste_insert", Timeline, "Editing", "Paste as insert", "Paste copied clip as insert at playhead", ["<Control>v"]),
    action!(PasteAttributes, "paste_attributes", Timeline, "Editing", "Paste attributes", "Paste copied clip attributes onto selected clip", ["<Control><Shift>v"]),
    action!(Delete, "delete", Timeline, "Editing", "Delete", "Delete selected clip(s)", ["Delete", "BackSpace"]),
    action!(RippleDelete, "ripple_delete", Timeline, "Editing", "Ripple delete", "Ripple delete selected clip(s) (track-local gap close)", ["<Shift>Delete", "<Shift>BackSpace"]),
    action!(JoinThroughEdit, "join_through_edit", Timeline, "Editing", "Join through-edit", "Join selected through-edit boundary into one clip", ["<Control><Shift>b"]),
    action!(PrecisionTrim, "precision_trim", Timeline, "Editing", "Precision Trim", "Open Precision Trim for the active Ripple/Roll/Slip/Slide tool, or commit the current Precision Trim session", ["Return"]),
    action!(MatchFrame, "match_frame", Timeline, "Editing", "Match frame", "Match Frame — load the selected clip's source in the Source Monitor at the matching timecode", ["f"]),
    action!(FreezeFrame, "freeze_frame", Timeline, "Editing", "Freeze frame", "Create a freeze-frame clip from the selected video clip at the playhead", ["<Shift>f"]),
    action!(CopyColorGrade, "copy_color_grade", Timeline, "Color", "Copy color grade", "Copy color grade from selected clip", ["<Control><Alt>c"]),
    action!(PasteColorGrade, "paste_color_grade", Timeline, "Color", "Paste color grade", "Paste color grade onto selected clip", ["<Control><Alt>v"]),
    action!(PasteColorGradeAll, "paste_color_grade_all", Timeline, "Color", "Paste color grade to all selected", "Paste color grade onto every selected clip (single undo)", ["<Control><Shift><Alt>v"]),
    action!(MatchColor, "match_color", Timeline, "Color", "Match color", "Match selected clip's color to a reference clip", ["<Control><Alt>m"]),
    action!(Group, "group", Timeline, "Clips", "Group clips", ["<Control>g"]),
    action!(Ungroup, "ungroup", Timeline, "Clips", "Ungroup clips", ["<Control><Shift>g"]),
    action!(Link, "link", Timeline, "Clips", "Link clips", ["<Control>l"]),
    action!(Unlink, "unlink", Timeline, "Clips", "Unlink clips", ["<Control><Shift>l"]),
    action!(CreateCompound, "create_compound", Timeline, "Clips", "Create compound clip", "Create compound clip from selected clips", ["<Alt>g"]),
    action!(CreateMulticam, "create_multicam", Timeline, "Clips", "Create multicam clip", "Create multicam clip from selected clips", ["<Alt>m"]),
    action!(KeyframeInterpLinear, "keyframe_interp_linear", Timeline, "Keyframes", "Keyframe interpolation: Linear", "Set selected keyframes to Linear interpolation", ["1"]),
    action!(KeyframeInterpEaseIn, "keyframe_interp_ease_in", Timeline, "Keyframes", "Keyframe interpolation: Ease In", "Set selected keyframes to Ease In", ["2"]),
    action!(KeyframeInterpEaseOut, "keyframe_interp_ease_out", Timeline, "Keyframes", "Keyframe interpolation: Ease Out", "Set selected keyframes to Ease Out", ["3"]),
    action!(KeyframeInterpEaseInOut, "keyframe_interp_ease_in_out", Timeline, "Keyframes", "Keyframe interpolation: Ease In/Out", "Set selected keyframes to Ease In/Out", ["4"]),
    action!(ToggleTrackSolo, "toggle_track_solo", Timeline, "Tracks", "Toggle track solo", "Toggle solo for the selected track", ["s"]),
    action!(ToggleTrackMute, "toggle_track_mute", Timeline, "Tracks", "Toggle track mute", "Toggle mute for the selected track", ["<Control>m"]),
    action!(ToggleTrackLock, "toggle_track_lock", Timeline, "Tracks", "Toggle track lock", "Toggle lock for the selected track", ["<Shift>l"]),
    action!(ToolRazor, "tool_razor", Timeline, "Tools", "Razor (Blade) tool", "Toggle Razor (Blade) tool", ["b"]),
    action!(ToolRipple, "tool_ripple", Timeline, "Tools", "Ripple edit tool", "Toggle Ripple edit tool", ["r"]),
    action!(ToolRoll, "tool_roll", Timeline, "Tools", "Roll edit tool", "Toggle Roll edit tool (2-up precision trim display in the Program Monitor while dragging)", ["e"]),
    action!(ToolSlip, "tool_slip", Timeline, "Tools", "Slip edit tool", "Toggle Slip edit tool (4-up precision trim display while dragging)", ["y"]),
    action!(ToolSlide, "tool_slide", Timeline, "Tools", "Slide edit tool", "Toggle Slide edit tool (4-up precision trim display while dragging)", ["u"]),
    action!(ToolDraw, "tool_draw", Timeline, "Tools", "Draw tool", "Toggle Draw tool", ["d"]),
    action!(EscapeCancel, "escape_cancel", Timeline, "Tools", "Cancel / Select tool", "Cancel Precision Trim or an armed Generate Music Region draw, exit a compound clip drill-down, or switch to the Select tool", ["Escape"]),
    action!(ShowShortcuts, "show_shortcuts", Timeline, "Help", "Show keyboard shortcuts", "Show this keyboard shortcut reference", ["question", "slash"]),
    // ── Source Monitor ────────────────────────────────────────────────────
    action!(SourcePlayPause, "source_play_pause", SourceMonitor, "Transport", "Play / Pause", ["space"]),
    action!(SourceMarkIn, "source_mark_in", SourceMonitor, "Marks", "Set In-point", ["i"]),
    action!(SourceMarkOut, "source_mark_out", SourceMonitor, "Marks", "Set Out-point", ["o"]),
    action!(SourceShuttleReverse, "source_shuttle_reverse", SourceMonitor, "Transport", "Shuttle reverse", "Shuttle reverse (1× → 2× → 4×)", ["j"]),
    action!(SourceShuttleStop, "source_shuttle_stop", SourceMonitor, "Transport", "Stop shuttle", "Stop shuttle / pause", ["k"]),
    action!(SourceShuttleForward, "source_shuttle_forward", SourceMonitor, "Transport", "Shuttle forward", "Shuttle forward (1× → 2× → 4×)", ["l"]),
    action!(SourceStepBack, "source_step_back", SourceMonitor, "Transport", "Step one frame back", ["Left"]),
    action!(SourceStepForward, "source_step_forward", SourceMonitor, "Transport", "Step one frame forward", ["Right"]),
];

impl ActionId {
    pub fn info(self) -> &'static ActionInfo {
        ACTIONS
            .iter()
            .find(|a| a.id == self)
            .expect("every ActionId has an ActionInfo entry")
    }

    /// Stable snake_case identifier (`keybindings.json`, MCP).
    pub fn key(self) -> &'static str {
        self.info().key
    }

    pub fn title(self) -> &'static str {
        self.info().title
    }

    pub fn context(self) -> ActionContext {
        self.info().context
    }

    pub fn from_key(key: &str) -> Option<ActionId> {
        ACTIONS.iter().find(|a| a.key == key).map(|a| a.id)
    }

    pub fn all() -> impl Iterator<Item = ActionId> {
        ACTIONS.iter().map(|a| a.id)
    }
}

// ── Chords ─────────────────────────────────────────────────────────────────

const CHORD_MODS: gdk::ModifierType = gdk::ModifierType::CONTROL_MASK
    .union(gdk::ModifierType::SHIFT_MASK)
    .union(gdk::ModifierType::ALT_MASK)
    .union(gdk::ModifierType::SUPER_MASK);

/// A normalised key combination.
///
/// Normalisation (applied by [`Chord::parse`] and [`Chord::from_event`]):
/// modifiers are masked to Ctrl/Shift/Alt/Super (Lock, Num, button state
/// dropped); letters are lowercased with Shift kept as a modifier
/// (`<Shift>z`); keypad keys map to their main-keyboard equivalents; for
/// shifted punctuation the keyboard already reports the shifted symbol
/// (`?` arrives as `question` + Shift), so Shift is dropped and the symbol
/// itself is the chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: gdk::Key,
    pub mods: gdk::ModifierType,
}

impl Chord {
    pub fn new(key: gdk::Key, mods: gdk::ModifierType) -> Chord {
        let mods = mods & CHORD_MODS;
        let key = Self::normalise_key(key);
        let shifted_symbol = mods.contains(gdk::ModifierType::SHIFT_MASK)
            && key
                .to_unicode()
                .map(|c| !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control())
                .unwrap_or(false);
        let mods = if shifted_symbol {
            mods & !gdk::ModifierType::SHIFT_MASK
        } else {
            mods
        };
        Chord { key, mods }
    }

    /// Build from a key event (`connect_key_pressed` arguments).
    pub fn from_event(key: gdk::Key, state: gdk::ModifierType) -> Chord {
        Self::new(key, state)
    }

    fn normalise_key(key: gdk::Key) -> gdk::Key {
        use gdk::Key;
        let key = match key {
            Key::KP_0 => Key::_0,
            Key::KP_1 => Key::_1,
            Key::KP_2 => Key::_2,
            Key::KP_3 => Key::_3,
            Key::KP_4 => Key::_4,
            Key::KP_5 => Key::_5,
            Key::KP_6 => Key::_6,
            Key::KP_7 => Key::_7,
            Key::KP_8 => Key::_8,
            Key::KP_9 => Key::_9,
            Key::KP_Enter => Key::Return,
            Key::KP_Left => Key::Left,
            Key::KP_Right => Key::Right,
            Key::KP_Up => Key::Up,
            Key::KP_Down => Key::Down,
            Key::KP_Delete => Key::Delete,
            Key::KP_Home => Key::Home,
            Key::KP_End => Key::End,
            Key::KP_Page_Up => Key::Page_Up,
            Key::KP_Page_Down => Key::Page_Down,
            Key::KP_Insert => Key::Insert,
            Key::KP_Add => Key::plus,
            Key::KP_Subtract => Key::minus,
            Key::KP_Space => Key::space,
            Key::KP_Tab => Key::Tab,
            Key::ISO_Left_Tab => Key::Tab,
            other => other,
        };
        key.to_lower()
    }

    /// True for keyvals that are modifiers themselves (never a chord).
    pub fn is_modifier_key(key: gdk::Key) -> bool {
        use gdk::Key;
        matches!(
            key,
            Key::Shift_L
                | Key::Shift_R
                | Key::Control_L
                | Key::Control_R
                | Key::Alt_L
                | Key::Alt_R
                | Key::Super_L
                | Key::Super_R
                | Key::Meta_L
                | Key::Meta_R
                | Key::Hyper_L
                | Key::Hyper_R
                | Key::ISO_Level3_Shift
                | Key::ISO_Level5_Shift
                | Key::Caps_Lock
                | Key::Num_Lock
                | Key::Scroll_Lock
                | Key::Mode_switch
        )
    }

    /// Parse GTK accelerator syntax: `<Control><Shift>z`, `<Primary>s`,
    /// `<Alt>Left`, `question`, `Delete`. Returns `None` for an unknown
    /// modifier or key name.
    pub fn parse(accel: &str) -> Option<Chord> {
        let mut rest = accel.trim();
        let mut mods = gdk::ModifierType::empty();
        while let Some(stripped) = rest.strip_prefix('<') {
            let end = stripped.find('>')?;
            let name = stripped[..end].to_ascii_lowercase();
            mods |= match name.as_str() {
                "control" | "ctrl" | "ctl" | "primary" => gdk::ModifierType::CONTROL_MASK,
                "shift" | "shft" => gdk::ModifierType::SHIFT_MASK,
                "alt" | "mod1" => gdk::ModifierType::ALT_MASK,
                "super" | "meta" | "mod4" | "hyper" => gdk::ModifierType::SUPER_MASK,
                "release" | "lock" => gdk::ModifierType::empty(),
                _ => return None,
            };
            rest = &stripped[end + 1..];
        }
        if rest.is_empty() {
            return None;
        }
        let key = gdk::Key::from_name(rest)
            .or_else(|| {
                // Accept a single character as its unicode keyval.
                let mut chars = rest.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                let kv = gtk4::gdk::unicode_to_keyval(c as u32);
                (kv != 0).then(|| unsafe { gtk4::glib::translate::from_glib(kv) })
            })
            .filter(|k| *k != gdk::Key::VoidSymbol)?;
        Some(Chord::new(key, mods))
    }

    /// Canonical accelerator syntax (round-trips through [`Chord::parse`]).
    pub fn accel_name(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(gdk::ModifierType::SHIFT_MASK) {
            out.push_str("<Shift>");
        }
        if self.mods.contains(gdk::ModifierType::CONTROL_MASK) {
            out.push_str("<Control>");
        }
        if self.mods.contains(gdk::ModifierType::ALT_MASK) {
            out.push_str("<Alt>");
        }
        if self.mods.contains(gdk::ModifierType::SUPER_MASK) {
            out.push_str("<Super>");
        }
        match self.key.name() {
            Some(n) => out.push_str(&n),
            None => out.push_str(&format!("U+{:04X}", self.key.into_glib())),
        }
        out
    }

    /// Human-readable label: `Ctrl+Shift+Z`, `Alt+←`, `Shift+Del`, `?`.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.mods.contains(gdk::ModifierType::CONTROL_MASK) {
            parts.push("Ctrl".into());
        }
        if self.mods.contains(gdk::ModifierType::ALT_MASK) {
            parts.push("Alt".into());
        }
        if self.mods.contains(gdk::ModifierType::SHIFT_MASK) {
            parts.push("Shift".into());
        }
        if self.mods.contains(gdk::ModifierType::SUPER_MASK) {
            parts.push("Super".into());
        }
        parts.push(Self::key_label(self.key));
        parts.join("+")
    }

    fn key_label(key: gdk::Key) -> String {
        use gdk::Key;
        let fixed = match key {
            Key::space => Some("Space"),
            Key::Return => Some("Enter"),
            Key::Escape => Some("Esc"),
            Key::Delete => Some("Del"),
            Key::BackSpace => Some("Backspace"),
            Key::Tab => Some("Tab"),
            Key::Left => Some("←"),
            Key::Right => Some("→"),
            Key::Up => Some("↑"),
            Key::Down => Some("↓"),
            Key::Home => Some("Home"),
            Key::End => Some("End"),
            Key::Page_Up => Some("PgUp"),
            Key::Page_Down => Some("PgDn"),
            Key::Insert => Some("Ins"),
            _ => None,
        };
        if let Some(f) = fixed {
            return f.to_string();
        }
        if let Some(c) = key.to_unicode() {
            if !c.is_control() && !c.is_whitespace() {
                return c.to_uppercase().collect();
            }
        }
        key.name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("U+{:04X}", key.into_glib()))
    }
}

/// `"Ctrl+Z / Ctrl+Y"` style label for a binding list; `"—"` when unbound.
pub fn chords_label(chords: &[Chord]) -> String {
    if chords.is_empty() {
        "—".to_string()
    } else {
        chords
            .iter()
            .map(Chord::label)
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

// ── Keymap ─────────────────────────────────────────────────────────────────

/// Current bindings (defaults with the user's overrides applied) plus the
/// reverse index used for dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<ActionId, Vec<Chord>>,
    index: HashMap<(ActionContext, Chord), ActionId>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Keymap {
    /// The built-in default keymap.
    pub fn defaults() -> Keymap {
        let mut bindings = BTreeMap::new();
        for info in ACTIONS {
            let chords: Vec<Chord> = info
                .defaults
                .iter()
                .filter_map(|a| Chord::parse(a))
                .collect();
            bindings.insert(info.id, chords);
        }
        let mut km = Keymap {
            bindings,
            index: HashMap::new(),
        };
        km.reindex();
        km
    }

    /// Defaults with `overrides` applied. Returns the keymap and a list of
    /// human-readable warnings for entries that were skipped (unknown action
    /// id, unparsable accelerator).
    pub fn with_overrides(overrides: &KeymapOverrides) -> (Keymap, Vec<String>) {
        let mut km = Keymap::defaults();
        let mut warnings = Vec::new();
        for (key, accels) in &overrides.bindings {
            let Some(id) = ActionId::from_key(key) else {
                warnings.push(format!("unknown action id `{key}` ignored"));
                continue;
            };
            let mut chords = Vec::new();
            for accel in accels {
                match Chord::parse(accel) {
                    Some(c) => {
                        if !chords.contains(&c) {
                            chords.push(c);
                        }
                    }
                    None => warnings.push(format!(
                        "unparsable accelerator `{accel}` for `{key}` skipped"
                    )),
                }
            }
            km.bindings.insert(id, chords);
        }
        km.reindex();
        (km, warnings)
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (id, chords) in &self.bindings {
            let ctx = id.context();
            for c in chords {
                self.index.entry((ctx, *c)).or_insert(*id);
            }
        }
    }

    /// Resolve a chord pressed while `ctx` is the focused context: the
    /// context's own binding wins, then a Global one.
    pub fn resolve(&self, ctx: ActionContext, chord: &Chord) -> Option<ActionId> {
        if let Some(id) = self.index.get(&(ctx, *chord)) {
            return Some(*id);
        }
        if ctx != ActionContext::Global {
            if let Some(id) = self.index.get(&(ActionContext::Global, *chord)) {
                return Some(*id);
            }
        }
        None
    }

    pub fn chords_for(&self, id: ActionId) -> &[Chord] {
        self.bindings.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn is_default(&self, id: ActionId) -> bool {
        let defaults: Vec<Chord> = id
            .info()
            .defaults
            .iter()
            .filter_map(|a| Chord::parse(a))
            .collect();
        self.chords_for(id) == defaults.as_slice()
    }

    pub fn set(&mut self, id: ActionId, chords: Vec<Chord>) {
        let mut dedup: Vec<Chord> = Vec::new();
        for c in chords {
            if !dedup.contains(&c) {
                dedup.push(c);
            }
        }
        self.bindings.insert(id, dedup);
        self.reindex();
    }

    pub fn add_chord(&mut self, id: ActionId, chord: Chord) {
        let mut chords = self.chords_for(id).to_vec();
        if !chords.contains(&chord) {
            chords.push(chord);
        }
        self.set(id, chords);
    }

    pub fn remove_chord(&mut self, id: ActionId, chord: &Chord) {
        let chords: Vec<Chord> = self
            .chords_for(id)
            .iter()
            .copied()
            .filter(|c| c != chord)
            .collect();
        self.set(id, chords);
    }

    pub fn reset(&mut self, id: ActionId) {
        let chords = id
            .info()
            .defaults
            .iter()
            .filter_map(|a| Chord::parse(a))
            .collect();
        self.set(id, chords);
    }

    pub fn reset_all(&mut self) {
        *self = Keymap::defaults();
    }

    /// Another action (≠ `id`) in the *same* context already bound to
    /// `chord` — a real conflict: only one of them could ever fire.
    pub fn conflict(&self, id: ActionId, chord: &Chord) -> Option<ActionId> {
        let ctx = id.context();
        self.bindings.iter().find_map(|(other, chords)| {
            (*other != id && other.context() == ctx && chords.contains(chord)).then_some(*other)
        })
    }

    /// A Global ↔ context overlap for `chord`: not an error (the focused
    /// panel's binding wins, the Global one is unreachable while that panel
    /// has focus — e.g. Source Monitor J/K/L shadow the Program Monitor
    /// J/K/L by default), but worth showing in the Preferences table.
    pub fn shadows(&self, id: ActionId, chord: &Chord) -> Option<ActionId> {
        let ctx = id.context();
        self.bindings.iter().find_map(|(other, chords)| {
            if *other == id || !chords.contains(chord) {
                return None;
            }
            let octx = other.context();
            let overlap = octx != ctx
                && (octx == ActionContext::Global || ctx == ActionContext::Global);
            overlap.then_some(*other)
        })
    }

    /// All conflicts in the current keymap (empty for the defaults).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn all_conflicts(&self) -> Vec<(ActionId, ActionId, Chord)> {
        let mut out = Vec::new();
        for (id, chords) in &self.bindings {
            for c in chords {
                if let Some(other) = self.conflict(*id, c) {
                    if id < &other {
                        out.push((*id, other, *c));
                    }
                }
            }
        }
        out
    }

    /// Only the actions whose bindings differ from the defaults.
    pub fn to_overrides(&self) -> KeymapOverrides {
        let mut bindings = BTreeMap::new();
        for id in ActionId::all() {
            if !self.is_default(id) {
                bindings.insert(
                    id.key().to_string(),
                    self.chords_for(id).iter().map(Chord::accel_name).collect(),
                );
            }
        }
        KeymapOverrides {
            version: KEYBINDINGS_VERSION,
            bindings,
        }
    }

    /// Rows for the `?` dialog / docs: per context, (action, label).
    pub fn rows_grouped(&self) -> Vec<(ActionContext, Vec<(ActionId, String)>)> {
        ActionContext::ALL
            .iter()
            .map(|ctx| {
                let rows = ACTIONS
                    .iter()
                    .filter(|a| a.context == *ctx)
                    .map(|a| (a.id, chords_label(self.chords_for(a.id))))
                    .collect();
                (*ctx, rows)
            })
            .collect()
    }
}

// ── Overrides file ─────────────────────────────────────────────────────────

pub const KEYBINDINGS_VERSION: u32 = 1;

/// On-disk format of `keybindings.json`: only actions that differ from the
/// defaults, as `action_id → [accelerator, …]` (an empty list = unbound).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeymapOverrides {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub bindings: BTreeMap<String, Vec<String>>,
}

fn default_version() -> u32 {
    KEYBINDINGS_VERSION
}

/// `$XDG_CONFIG_HOME/ultimateslice/keybindings.json` (fallback
/// `~/.config/...`), mirroring `recent.rs` / `ui_state.rs`.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ultimateslice").join("keybindings.json"))
}

/// Read an overrides file (Preferences → Shortcuts → Import…).
pub fn load_overrides_from_path(path: &std::path::Path) -> Result<KeymapOverrides, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str::<KeymapOverrides>(&text).map_err(|e| e.to_string())
}

pub fn load_overrides() -> KeymapOverrides {
    let Some(path) = config_path() else {
        return KeymapOverrides::default();
    };
    if !path.exists() {
        return KeymapOverrides::default();
    }
    match load_overrides_from_path(&path) {
        Ok(o) => o,
        Err(e) => {
            log::warn!("keybindings: {} is not valid: {e}; using defaults", path.display());
            KeymapOverrides::default()
        }
    }
}

pub fn save_overrides(overrides: &KeymapOverrides) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(overrides)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, text)
}

/// Load the user's keymap, logging any skipped entries.
pub fn load_keymap() -> Keymap {
    let (km, warnings) = Keymap::with_overrides(&load_overrides());
    for w in warnings {
        log::warn!("keybindings: {w}");
    }
    km
}

// ── Handler table ──────────────────────────────────────────────────────────

/// Handler closures registered by the UI. A handler returns `true` when it
/// handled the key (event propagation stops) and `false` to let GTK's
/// default handling proceed.
#[derive(Default)]
pub struct ActionMap {
    handlers: HashMap<ActionId, Rc<dyn Fn() -> bool>>,
}

impl ActionMap {
    pub fn bind(&mut self, id: ActionId, handler: Rc<dyn Fn() -> bool>) {
        self.handlers.insert(id, handler);
    }

    pub fn is_bound(&self, id: ActionId) -> bool {
        self.handlers.contains_key(&id)
    }

    /// The handler for `id`, cloned out so the caller can release any
    /// `RefCell` borrow of the map before invoking it (handlers may open
    /// dialogs that mutate the keymap or re-enter dispatch).
    pub fn handler(&self, id: ActionId) -> Option<Rc<dyn Fn() -> bool>> {
        self.handlers.get(&id).cloned()
    }
}

// ── Docs generation ────────────────────────────────────────────────────────

/// Mouse gestures and panel-internal keys that are not rebindable, rendered
/// after the generated tables (shared by the `?` dialog and the docs page).
pub const STATIC_SECTIONS: &[(&str, &str, &[(&str, &str)])] = &[
    (
        "Timeline — fixed keys and mouse",
        "",
        &[
            ("←  /  →  (Precision Trim active)", "Nudge the active Precision Trim session by 1 frame"),
            ("Shift+←  /  Shift+→  (Precision Trim active)", "Nudge the active Precision Trim session by 5 frames"),
            ("5 – 9  (multicam clip selected)", "Switch to angle 5–9 at the playhead"),
            ("Double-click (compound clip)", "Drill into compound clip sub-timeline"),
            ("Shift+Click", "Add range selection (same-track span, or cross-track time-range select)"),
            ("Ctrl/Cmd+Click", "Toggle a clip in the current selection"),
            ("Drag in empty timeline body", "Marquee-select clips intersecting the rectangle"),
            ("Right-click clip", "Clip context menu with the currently actionable clip actions (including **Precision Trim…**)"),
            ("Right-click ruler", "Remove nearest marker"),
            ("Right-click transition marker", "Remove transition at clip boundary"),
            ("Scroll (vertical)", "Zoom timeline in/out"),
            ("Scroll (horizontal)", "Pan timeline left/right"),
            ("Click (mini-map)", "Centre viewport on clicked position"),
            ("Drag (mini-map)", "Pan viewport continuously"),
            ("Ctrl/Cmd+Click (mini-map)", "Seek playhead to clicked position"),
            ("Double-click (mini-map)", "Zoom timeline to fit entire project"),
        ],
    ),
    (
        "Inspector / Program Monitor overlay",
        "All Inspector controls are mouse-driven sliders and text fields. Transform edits can also be nudged from the Program Monitor overlay:",
        &[
            ("←  /  →  /  ↑  /  ↓", "Nudge selected clip position by 0.01"),
            ("Shift+Arrow", "Nudge selected clip position by 0.1"),
            ("+", "Increase selected clip scale"),
            ("-", "Decrease selected clip scale"),
            ("1 – 4  (Draw tool)", "Stroke / Rectangle / Ellipse / Arrow"),
            ("Delete / Backspace  (Draw tool)", "Delete the selected (or last) drawing item"),
            ("Ctrl/Cmd+Click  (Keyframes panel focused)", "Toggle clicked keyframe in dopesheet selection"),
            ("Shift+Click  (Keyframes panel focused)", "Select same-lane keyframe range from anchor to clicked keyframe"),
            ("Delete / Backspace  (Keyframes panel focused)", "Remove selected dopesheet keyframe(s)"),
            ("←  /  →  (Keyframes panel focused)", "Nudge selected dopesheet keyframe(s) by 1 frame"),
            ("Shift+←  /  Shift+→  (Keyframes panel focused)", "Nudge selected dopesheet keyframe(s) by 10 frames"),
            ("Ctrl+Scroll  (Keyframes panel focused)", "Zoom dopesheet time scale in/out"),
            ("Scroll  (Keyframes panel focused)", "Pan dopesheet time view"),
        ],
    ),
    (
        "Transcript",
        "The Transcript panel lives next to the keyframe dopesheet at the bottom of the timeline; toggle it via **Show Transcript** in the track-management bar. See [transcript.md](transcript.md) for details.",
        &[
            ("Click  (Transcript panel)", "Seek the playhead to the clicked word"),
            ("Shift+Click  (Transcript panel)", "Extend the word selection within the same clip"),
            ("Delete / Backspace  (Transcript panel focused)", "Split the underlying clip at the selection edges and ripple-delete the middle slice as one undo entry"),
        ],
    ),
];

#[cfg_attr(not(test), allow(dead_code))] // consumed by the docs-generation test
pub const DOCS_NOTES: &str = "\
## Notes

- **Customise**: Preferences → **Shortcuts** lists every rebindable action; click a shortcut and press the new key combination. Overrides are stored in `~/.config/ultimateslice/keybindings.json` (only actions that differ from the defaults), which can be exported, imported or deleted to reset.
- **Contexts**: *Global* shortcuts work anywhere in the window (and in the popped-out Program Monitor); *Timeline* shortcuts need the timeline to have focus; *Source Monitor* shortcuts need the Source Monitor panel to have focus. A Timeline or Source Monitor binding takes precedence over a Global one for the same key while that panel has focus.
- **Typing**: while a text field has focus, shortcuts without Ctrl or Alt are passed to the field.
- **J/K/L** control the **Program Monitor** from anywhere; in the Source Monitor, J/K/L shuttle the source when that panel has focus.
- **Space** toggles playback in whichever monitor has focus.
- Right-click an audio track header and choose **Generate Music Region…** to arm the one-shot MusicGen draw gesture; `Esc` cancels it before you drag.
- In timeline selection, **Ctrl/Cmd+Shift+Click** behaves like **Ctrl/Cmd+Click** (toggle clicked clip).
";

/// Generate `docs/user/shortcuts.md` for `keymap` (normally the defaults).
/// Driven by the `docs_shortcuts_md_is_current` test (`UPDATE_SHORTCUTS_DOC=1`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn render_shortcuts_markdown(keymap: &Keymap) -> String {
    let mut out = String::new();
    out.push_str("# UltimateSlice — Keyboard Shortcuts Reference\n\n");
    out.push_str("<!-- GENERATED from src/keybindings.rs by `UPDATE_SHORTCUTS_DOC=1 cargo test keybindings` — edit the registry, not this file. -->\n\n");
    out.push_str("> Press **?** or **/** in the timeline to open this reference in-app. Every shortcut below (except the fixed keys and mouse gestures) can be changed in **Preferences → Shortcuts**.\n\n---\n\n");
    for (ctx, rows) in keymap.rows_grouped() {
        out.push_str(&format!("## {}\n\n| Shortcut | Action |\n|---|---|\n", ctx.label()));
        for (id, label) in rows {
            let info = id.info();
            let desc = if info.doc.is_empty() { info.title } else { info.doc };
            let keys = label
                .split(" / ")
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(" / ");
            out.push_str(&format!("| {keys} | {desc} |\n"));
        }
        out.push_str("\n---\n\n");
    }
    for (title, intro, rows) in STATIC_SECTIONS {
        out.push_str(&format!("## {title}\n\n"));
        if !intro.is_empty() {
            out.push_str(intro);
            out.push_str("\n\n");
        }
        out.push_str("| Shortcut | Action |\n|---|---|\n");
        for (k, a) in rows.iter() {
            out.push_str(&format!("| `{k}` | {a} |\n"));
        }
        out.push_str("\n---\n\n");
    }
    out.push_str(DOCS_NOTES);
    out
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::ModifierType as M;

    fn chord(s: &str) -> Chord {
        Chord::parse(s).unwrap_or_else(|| panic!("parse {s}"))
    }

    #[test]
    fn chord_parse_and_name_round_trip() {
        for s in [
            "<Control><Shift>z",
            "<Shift><Control><Alt>v",
            "<Alt>Left",
            "space",
            "Delete",
            "question",
            "comma",
            "<Control>comma",
            "1",
            "<Shift>k",
        ] {
            let c = chord(s);
            let again = Chord::parse(&c.accel_name()).unwrap();
            assert_eq!(c, again, "{s} → {} did not round-trip", c.accel_name());
        }
        assert_eq!(chord("<Primary>s"), chord("<Control>s"));
        assert_eq!(chord("<Ctrl>s").accel_name(), "<Control>s");
        assert!(Chord::parse("<Bogus>s").is_none());
        assert!(Chord::parse("notakey").is_none());
        assert!(Chord::parse("").is_none());
    }

    #[test]
    fn chord_normalisation_matches_events() {
        // Uppercase letter with Shift (Caps/Num lock dropped).
        let ev = Chord::from_event(gdk::Key::Z, M::SHIFT_MASK | M::CONTROL_MASK | M::LOCK_MASK);
        assert_eq!(ev, chord("<Control><Shift>z"));
        // Typed `?` arrives as `question` + Shift → stored as plain `question`.
        assert_eq!(Chord::from_event(gdk::Key::question, M::SHIFT_MASK), chord("question"));
        assert_eq!(chord("<Shift>question"), chord("question"));
        assert_eq!(chord("<Shift>slash"), chord("slash"));
        // Digits keep Shift; keypad digits fold to the main row.
        assert_eq!(Chord::from_event(gdk::Key::KP_1, M::empty()), chord("1"));
        assert_ne!(chord("<Shift>1"), chord("1"));
        assert_eq!(Chord::from_event(gdk::Key::KP_Enter, M::empty()), chord("Return"));
        // Shift+Delete stays shifted (no unicode symbol).
        assert_eq!(Chord::from_event(gdk::Key::Delete, M::SHIFT_MASK), chord("<Shift>Delete"));
        assert_ne!(chord("<Shift>Delete"), chord("Delete"));
    }

    #[test]
    fn chord_labels() {
        assert_eq!(chord("<Control><Shift>z").label(), "Ctrl+Shift+Z");
        assert_eq!(chord("<Alt>Left").label(), "Alt+←");
        assert_eq!(chord("<Shift>Delete").label(), "Shift+Del");
        assert_eq!(chord("space").label(), "Space");
        assert_eq!(chord("question").label(), "?");
        assert_eq!(chord("comma").label(), ",");
        assert_eq!(chord("Return").label(), "Enter");
        assert_eq!(chords_label(&[chord("<Control>y"), chord("<Control><Shift>z")]), "Ctrl+Y / Ctrl+Shift+Z");
        assert_eq!(chords_label(&[]), "—");
    }

    #[test]
    fn every_action_has_info_title_and_parsable_defaults() {
        let mut keys = std::collections::HashSet::new();
        for info in ACTIONS {
            assert!(!info.title.is_empty(), "{:?} has no title", info.id);
            assert!(keys.insert(info.key), "duplicate action key {}", info.key);
            assert_eq!(ActionId::from_key(info.key), Some(info.id));
            for d in info.defaults {
                assert!(Chord::parse(d).is_some(), "{:?}: default `{d}` does not parse", info.id);
            }
        }
    }

    #[test]
    fn defaults_have_no_conflicts() {
        let km = Keymap::defaults();
        assert!(
            km.all_conflicts().is_empty(),
            "default keymap conflicts: {:?}",
            km.all_conflicts()
                .iter()
                .map(|(a, b, c)| format!("{} vs {} on {}", a.key(), b.key(), c.label()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn resolve_prefers_context_then_global() {
        let km = Keymap::defaults();
        // Space is bound in both panels; J is Global and Source Monitor.
        assert_eq!(km.resolve(Timeline, &chord("space")), Some(PlayPause));
        assert_eq!(km.resolve(SourceMonitor, &chord("space")), Some(SourcePlayPause));
        assert_eq!(km.resolve(Global, &chord("space")), None);
        assert_eq!(km.resolve(SourceMonitor, &chord("j")), Some(SourceShuttleReverse));
        assert_eq!(km.resolve(Timeline, &chord("j")), Some(ShuttleReverse));
        assert_eq!(km.resolve(Timeline, &chord("m")), Some(AddMarker));
        assert_eq!(km.resolve(Timeline, &chord("<Control>m")), Some(ToggleTrackMute));
        assert_eq!(km.resolve(Timeline, &chord("<Control>j")), Some(GoToTimecode));
        assert_eq!(km.resolve(Global, &chord("<Shift>k")), Some(ToggleAnimationMode));
        assert_eq!(km.resolve(Timeline, &chord("<Control>y")), Some(Redo));
        assert_eq!(km.resolve(Timeline, &chord("<Control><Shift>z")), Some(Redo));
    }

    #[test]
    fn overrides_merge_and_validate() {
        let mut o = KeymapOverrides::default();
        o.bindings.insert("undo".into(), vec!["<Control>u".into(), "garbage key".into()]);
        o.bindings.insert("not_an_action".into(), vec!["a".into()]);
        o.bindings.insert("tool_draw".into(), vec![]);
        let (km, warnings) = Keymap::with_overrides(&o);
        assert_eq!(km.chords_for(Undo), &[chord("<Control>u")]);
        assert!(km.chords_for(ToolDraw).is_empty());
        assert_eq!(km.resolve(Timeline, &chord("d")), None);
        assert_eq!(km.resolve(Timeline, &chord("<Control>z")), None);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(!km.is_default(Undo) && !km.is_default(ToolDraw) && km.is_default(Redo));
        // Only the changed actions are written back.
        let back = km.to_overrides();
        assert_eq!(back.bindings.len(), 2);
        assert_eq!(back.bindings["undo"], vec!["<Control>u".to_string()]);
        assert!(back.bindings["tool_draw"].is_empty());
        let json = serde_json::to_string(&back).unwrap();
        let parsed: KeymapOverrides = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, back);
        assert!(Keymap::defaults().to_overrides().bindings.is_empty());
    }

    #[test]
    fn conflicts_and_shadows() {
        let km = Keymap::defaults();
        // Same context = conflict.
        assert_eq!(km.conflict(ToolSlide, &chord("b")), Some(ToolRazor));
        assert_eq!(km.conflict(ToggleHud, &chord("m")), Some(AddMarker));
        // Global ↔ context = shadowing, not a conflict.
        assert_eq!(km.conflict(ToggleTrackMute, &chord("m")), None);
        assert_eq!(km.shadows(ToggleTrackMute, &chord("m")), Some(AddMarker));
        assert_eq!(km.shadows(ToggleHud, &chord("b")), Some(ToolRazor));
        assert_eq!(km.shadows(SourceShuttleReverse, &chord("j")), Some(ShuttleReverse));
        // Source Monitor and Timeline do not see each other.
        assert_eq!(km.conflict(SourceMarkIn, &chord("b")), None);
        assert_eq!(km.shadows(SourceMarkIn, &chord("b")), None);
        let mut km2 = km.clone();
        km2.set(ToggleTrackMute, vec![chord("<Control>m"), chord("<Shift>u")]);
        assert_eq!(km2.conflict(ToolSlide, &chord("<Shift>u")), Some(ToggleTrackMute));
        km2.remove_chord(ToggleTrackMute, &chord("<Shift>u"));
        assert_eq!(km2.conflict(ToolSlide, &chord("<Shift>u")), None);
        km2.reset(ToggleTrackMute);
        assert!(km2.is_default(ToggleTrackMute));
    }

    #[test]
    fn action_map_dispatches() {
        let mut map = ActionMap::default();
        let hits = Rc::new(std::cell::Cell::new(0));
        let h = hits.clone();
        map.bind(
            Undo,
            Rc::new(move || {
                h.set(h.get() + 1);
                true
            }),
        );
        assert!(map.is_bound(Undo) && !map.is_bound(Redo));
        assert_eq!(map.handler(Undo).map(|f| f()), Some(true));
        assert_eq!(hits.get(), 1);
    }

    /// `docs/user/shortcuts.md` is generated from the registry. Run with
    /// `UPDATE_SHORTCUTS_DOC=1` to rewrite it after changing defaults.
    #[test]
    fn docs_shortcuts_md_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/user/shortcuts.md");
        let expected = render_shortcuts_markdown(&Keymap::defaults());
        if std::env::var_os("UPDATE_SHORTCUTS_DOC").is_some() {
            std::fs::write(path, &expected).expect("write shortcuts.md");
            return;
        }
        let actual = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            actual, expected,
            "docs/user/shortcuts.md is stale; run `UPDATE_SHORTCUTS_DOC=1 cargo test keybindings`"
        );
    }
}
