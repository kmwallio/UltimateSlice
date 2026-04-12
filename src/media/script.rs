// SPDX-License-Identifier: GPL-3.0-or-later
//! Screenplay parsing for Final Draft (FDX) and Fountain formats.
//!
//! Both parsers produce a common [`Script`] model that the alignment engine
//! and assembly pipeline consume.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Data model ──────────────────────────────────────────────────────────

/// The type of a screenplay element within a scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptElementKind {
    SceneHeading,
    Action,
    Character,
    Dialogue,
    Parenthetical,
    Transition,
}

/// A single element (line) within a scene.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptElement {
    pub kind: ScriptElementKind,
    pub text: String,
    /// Populated for Dialogue and Parenthetical elements.
    pub character: Option<String>,
}

/// A scene parsed from a screenplay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    /// Stable UUID for this scene (generated at parse time).
    pub id: String,
    /// Explicit scene number if present in the script (e.g. "42A").
    pub scene_number: Option<String>,
    /// The scene heading text (e.g. "INT. OFFICE - DAY").
    pub heading: String,
    /// Ordered elements within the scene.
    pub elements: Vec<ScriptElement>,
    /// Concatenated text of all elements, lowercased, for alignment matching.
    pub full_text: String,
}

/// A parsed screenplay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Script {
    /// Path to the original script file.
    pub path: String,
    /// Script title if present in metadata.
    pub title: Option<String>,
    /// Scenes in script order.
    pub scenes: Vec<Scene>,
}

// ── Auto-detect and parse ───────────────────────────────────────────────

/// Parse a screenplay file, auto-detecting format by extension.
///
/// Supported formats: `.fdx` (Final Draft), `.fountain` (Fountain).
pub fn parse_script(path: &str) -> Result<Script> {
    let lower = path.to_lowercase();
    if lower.ends_with(".fdx") {
        parse_fdx(path)
    } else if lower.ends_with(".fountain") || lower.ends_with(".spmd") {
        parse_fountain(path)
    } else {
        // Try Fountain as a fallback for plain-text scripts.
        parse_fountain(path)
    }
}

// ── Final Draft FDX parser ──────────────────────────────────────────────

/// Parse a Final Draft XML (.fdx) file.
///
/// FDX structure (simplified):
/// ```xml
/// <FinalDraft>
///   <TitlePage>...</TitlePage>
///   <Content>
///     <Paragraph Type="Scene Heading"><Text>INT. OFFICE - DAY</Text></Paragraph>
///     <Paragraph Type="Action"><Text>John enters.</Text></Paragraph>
///     <Paragraph Type="Character"><Text>JOHN</Text></Paragraph>
///     <Paragraph Type="Dialogue"><Text>Hello there.</Text></Paragraph>
///   </Content>
/// </FinalDraft>
/// ```
pub fn parse_fdx(path: &str) -> Result<Script> {
    let xml =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read FDX: {path}"))?;
    parse_fdx_str(&xml, path)
}

fn parse_fdx_str(xml: &str, path: &str) -> Result<Script> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);

    let mut title: Option<String> = None;
    let mut scenes: Vec<Scene> = Vec::new();
    let mut current_elements: Vec<ScriptElement> = Vec::new();
    let mut current_heading = String::new();
    let mut current_scene_number: Option<String> = None;
    let mut current_character: Option<String> = None;
    let mut pending_scene_number: Option<String> = None;

    // Parser state
    let mut in_content = false;
    let mut in_title_page = false;
    let mut para_type = String::new();
    let mut in_paragraph = false;
    let mut in_text = false;
    let mut text_buf = String::new();
    let mut depth_in_content: u32 = 0;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name_bytes = e.name();
                let local_name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                match local_name {
                    "Content" => {
                        in_content = true;
                        depth_in_content = 0;
                    }
                    "TitlePage" => in_title_page = true,
                    "Paragraph" if in_content => {
                        in_paragraph = true;
                        para_type.clear();
                        // Capture scene number into a temporary — it belongs
                        // to this paragraph's scene heading, not the previous
                        // scene.  We assign it after flushing the old scene.
                        let mut para_scene_number: Option<String> = None;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"Type" {
                                para_type = String::from_utf8_lossy(&attr.value).to_string();
                            }
                            if attr.key.as_ref() == b"Number" {
                                para_scene_number =
                                    Some(String::from_utf8_lossy(&attr.value).to_string());
                            }
                        }
                        pending_scene_number = para_scene_number;
                    }
                    "Text" if in_paragraph || in_title_page => {
                        in_text = true;
                        text_buf.clear();
                    }
                    _ => {
                        if in_content {
                            depth_in_content += 1;
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name_bytes = e.name();
                let local_name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                match local_name {
                    "Content" => in_content = false,
                    "TitlePage" => in_title_page = false,
                    "Text" => {
                        in_text = false;
                    }
                    "Paragraph" if in_content => {
                        in_paragraph = false;
                        let trimmed = text_buf.trim().to_string();
                        if !trimmed.is_empty() {
                            let kind = fdx_para_type_to_kind(&para_type);
                            if kind == ScriptElementKind::SceneHeading {
                                // Flush previous scene.
                                flush_scene(
                                    &mut scenes,
                                    &current_heading,
                                    &current_scene_number,
                                    &mut current_elements,
                                );
                                current_heading = trimmed.clone();
                                // Assign scene number from this heading's paragraph.
                                current_scene_number = pending_scene_number.take();
                            }
                            if kind == ScriptElementKind::Character {
                                current_character = Some(trimmed.clone());
                            }
                            let character = if kind == ScriptElementKind::Dialogue
                                || kind == ScriptElementKind::Parenthetical
                            {
                                current_character.clone()
                            } else {
                                if kind != ScriptElementKind::Character {
                                    current_character = None;
                                }
                                None
                            };
                            current_elements.push(ScriptElement {
                                kind,
                                text: trimmed,
                                character,
                            });
                        }
                        text_buf.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) if in_text => {
                if let Ok(t) = e.unescape() {
                    if !text_buf.is_empty() {
                        text_buf.push(' ');
                    }
                    text_buf.push_str(&t);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(anyhow::anyhow!("FDX parse error at position {}: {e}", reader.error_position()));
            }
            _ => {}
        }
    }

    // Flush last scene.
    flush_scene(
        &mut scenes,
        &current_heading,
        &current_scene_number,
        &mut current_elements,
    );

    // Try to extract title from TitlePage if we didn't get one.
    // (Simple heuristic: first non-empty text in TitlePage.)
    if title.is_none() {
        // Re-scan for title page.
        title = extract_fdx_title(xml);
    }

    Ok(Script {
        path: path.to_string(),
        title,
        scenes,
    })
}

fn extract_fdx_title(xml: &str) -> Option<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut in_title_page = false;
    let mut in_text = false;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name_bytes = e.name();
                let name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                if name == "TitlePage" {
                    in_title_page = true;
                }
                if name == "Text" && in_title_page {
                    in_text = true;
                    text_buf.clear();
                }
            }
            Ok(Event::End(ref e)) => {
                let name_bytes = e.name();
                let name = std::str::from_utf8(name_bytes.as_ref()).unwrap_or("");
                if name == "TitlePage" {
                    return None;
                }
                if name == "Text" && in_title_page {
                    in_text = false;
                    let trimmed = text_buf.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
            Ok(Event::Text(ref e)) if in_text => {
                if let Ok(t) = e.unescape() {
                    text_buf.push_str(&t);
                }
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
    }
}

fn fdx_para_type_to_kind(para_type: &str) -> ScriptElementKind {
    match para_type {
        "Scene Heading" => ScriptElementKind::SceneHeading,
        "Action" => ScriptElementKind::Action,
        "Character" => ScriptElementKind::Character,
        "Dialogue" => ScriptElementKind::Dialogue,
        "Parenthetical" => ScriptElementKind::Parenthetical,
        "Transition" => ScriptElementKind::Transition,
        // General / unknown → Action.
        _ => ScriptElementKind::Action,
    }
}

fn flush_scene(
    scenes: &mut Vec<Scene>,
    heading: &str,
    scene_number: &Option<String>,
    elements: &mut Vec<ScriptElement>,
) {
    if heading.is_empty() && elements.is_empty() {
        return;
    }
    let heading_text = if heading.is_empty() {
        format!("Scene {}", scenes.len() + 1)
    } else {
        heading.to_string()
    };

    let full_text = build_full_text(&heading_text, elements);
    scenes.push(Scene {
        id: Uuid::new_v4().to_string(),
        scene_number: scene_number.clone(),
        heading: heading_text,
        elements: std::mem::take(elements),
        full_text,
    });
}

fn build_full_text(heading: &str, elements: &[ScriptElement]) -> String {
    let mut parts = vec![heading.to_lowercase()];
    for el in elements {
        parts.push(el.text.to_lowercase());
    }
    parts.join(" ")
}

// ── Fountain parser ─────────────────────────────────────────────────────

/// Parse a Fountain (.fountain) plain-text screenplay.
///
/// Implements the core Fountain spec:
/// - Scene headings: lines starting with `INT.`, `EXT.`, `INT/EXT.`, `I/E.`,
///   or forced with a leading `.` (but not `..`).
/// - Character: all-uppercase line (possibly with `(V.O.)`, `(O.S.)`, etc.)
///   followed by dialogue on the next line. `@` forces a character cue.
/// - Dialogue: lines immediately following a Character or Parenthetical.
/// - Parenthetical: lines in `(parentheses)` within dialogue blocks.
/// - Transition: lines ending with `TO:` in all caps, or forced with `>`.
/// - Title page: `Key: Value` pairs at the start of the file before first blank line.
/// - Action: everything else.
pub fn parse_fountain(path: &str) -> Result<Script> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read Fountain: {path}"))?;
    Ok(parse_fountain_str(&text, path))
}

fn parse_fountain_str(text: &str, path: &str) -> Script {
    // Normalize line endings.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();

    let mut title: Option<String> = None;
    let mut scenes: Vec<Scene> = Vec::new();
    let mut current_elements: Vec<ScriptElement> = Vec::new();
    let mut current_heading = String::new();
    let mut current_scene_number: Option<String> = None;
    let mut current_character: Option<String> = None;
    let mut in_dialogue_block = false;

    let mut idx = 0;

    // Parse optional title page (key: value pairs before first blank line).
    if let Some(title_val) = parse_fountain_title_page(&lines, &mut idx) {
        title = Some(title_val);
    }

    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();
        idx += 1;

        // Blank line ends dialogue blocks.
        if trimmed.is_empty() {
            in_dialogue_block = false;
            current_character = None;
            continue;
        }

        // Skip boneyard (/* ... */) and notes ([[ ... ]]) — simplified: skip lines
        // that are entirely within these markers. Full boneyard handling would need
        // multi-line state, but this covers the common single-line case.
        if trimmed.starts_with("/*") || trimmed.starts_with("[[") {
            continue;
        }

        // Section headers (# ... ) and synopses (= ...) — skip, not screenplay content.
        if trimmed.starts_with('#') || trimmed.starts_with('=') {
            continue;
        }

        // Scene heading?
        if let Some((heading, scene_num)) = try_parse_scene_heading(trimmed) {
            in_dialogue_block = false;
            current_character = None;
            // Flush previous scene.
            flush_scene(
                &mut scenes,
                &current_heading,
                &current_scene_number,
                &mut current_elements,
            );
            current_heading = heading;
            current_scene_number = scene_num;
            continue;
        }

        // Transition?
        if is_fountain_transition(trimmed) {
            in_dialogue_block = false;
            current_character = None;
            current_elements.push(ScriptElement {
                kind: ScriptElementKind::Transition,
                text: trimmed.trim_start_matches('>').trim().to_string(),
                character: None,
            });
            continue;
        }

        // Parenthetical within dialogue block?
        if in_dialogue_block && trimmed.starts_with('(') && trimmed.ends_with(')') {
            current_elements.push(ScriptElement {
                kind: ScriptElementKind::Parenthetical,
                text: trimmed.to_string(),
                character: current_character.clone(),
            });
            continue;
        }

        // Dialogue continuation?
        if in_dialogue_block {
            current_elements.push(ScriptElement {
                kind: ScriptElementKind::Dialogue,
                text: trimmed.to_string(),
                character: current_character.clone(),
            });
            continue;
        }

        // Character cue? Must be followed by a non-blank line (dialogue).
        if let Some(char_name) = try_parse_character(trimmed, &lines, idx) {
            current_character = Some(char_name.clone());
            in_dialogue_block = true;
            current_elements.push(ScriptElement {
                kind: ScriptElementKind::Character,
                text: char_name,
                character: None,
            });
            continue;
        }

        // Default: Action.
        current_elements.push(ScriptElement {
            kind: ScriptElementKind::Action,
            text: trimmed.to_string(),
            character: None,
        });
    }

    // Flush last scene.
    flush_scene(
        &mut scenes,
        &current_heading,
        &current_scene_number,
        &mut current_elements,
    );

    Script {
        path: path.to_string(),
        title,
        scenes,
    }
}

fn parse_fountain_title_page(lines: &[&str], idx: &mut usize) -> Option<String> {
    // Title page is key:value pairs at file start, terminated by first blank line.
    if lines.is_empty() {
        return None;
    }

    // Check first non-blank line has a colon (key:value).
    let first = lines[0].trim();
    if !first.contains(':') || first.is_empty() {
        return None;
    }

    let mut title = None;
    while *idx < lines.len() {
        let line = lines[*idx].trim();
        if line.is_empty() {
            *idx += 1;
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            if key == "title" && !value.is_empty() {
                title = Some(value);
            }
        }
        *idx += 1;
    }
    title
}

fn try_parse_scene_heading(line: &str) -> Option<(String, Option<String>)> {
    let upper = line.to_uppercase();

    // Forced scene heading: leading `.` (but not `..`).
    if line.starts_with('.') && !line.starts_with("..") {
        let heading = line[1..].trim().to_string();
        let scene_num = extract_scene_number(&heading);
        return Some((heading, scene_num));
    }

    // Standard scene heading prefixes.
    let prefixes = ["INT./EXT.", "INT/EXT.", "I/E.", "INT.", "EXT."];
    for prefix in &prefixes {
        if upper.starts_with(prefix) {
            let heading = line.trim().to_string();
            let scene_num = extract_scene_number(&heading);
            return Some((heading, scene_num));
        }
    }

    None
}

fn extract_scene_number(heading: &str) -> Option<String> {
    // Fountain scene numbers: `#number#` at end of heading.
    if let Some(start) = heading.rfind('#') {
        let before = &heading[..start];
        if let Some(num_start) = before.rfind('#') {
            let num = heading[num_start + 1..start].trim();
            if !num.is_empty() {
                return Some(num.to_string());
            }
        }
    }
    None
}

fn is_fountain_transition(line: &str) -> bool {
    // Forced transition: `> FADE OUT.`
    if line.starts_with('>') {
        return true;
    }
    // Standard: all uppercase ending with "TO:"
    if line.ends_with("TO:") && line == line.to_uppercase() && !line.is_empty() {
        return true;
    }
    false
}

fn try_parse_character(line: &str, lines: &[&str], next_idx: usize) -> Option<String> {
    // Forced character: leading `@`.
    if line.starts_with('@') {
        let name = line[1..].trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }

    // Standard character cue: all uppercase, optionally followed by extension
    // like (V.O.), (O.S.), (CONT'D).
    // Must not be a scene heading or transition.
    let base = if let Some(paren_start) = line.find('(') {
        line[..paren_start].trim()
    } else {
        line.trim()
    };

    if base.is_empty() {
        return None;
    }

    // Must be all uppercase letters (allow spaces, periods, apostrophes).
    let is_upper = base
        .chars()
        .all(|c| c.is_uppercase() || !c.is_alphabetic());
    // Must contain at least one letter.
    let has_letter = base.chars().any(|c| c.is_alphabetic());

    if !is_upper || !has_letter {
        return None;
    }

    // Must be followed by a non-blank line (the dialogue).
    if next_idx >= lines.len() || lines[next_idx].trim().is_empty() {
        return None;
    }

    Some(line.trim().to_string())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_fdx_basic() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>INT. OFFICE - DAY</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>John enters the room.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>JOHN</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>Hello there.</Text>
    </Paragraph>
    <Paragraph Type="Scene Heading">
      <Text>EXT. PARK - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>The wind howls.</Text>
    </Paragraph>
  </Content>
</FinalDraft>"#;
        let script = parse_fdx_str(xml, "test.fdx").unwrap();
        assert_eq!(script.scenes.len(), 2);
        assert_eq!(script.scenes[0].heading, "INT. OFFICE - DAY");
        assert_eq!(script.scenes[0].elements.len(), 4); // heading + action + character + dialogue
        assert_eq!(script.scenes[1].heading, "EXT. PARK - NIGHT");
        assert_eq!(script.scenes[1].elements.len(), 2); // heading + action

        // Check dialogue has character reference.
        let dialogue = &script.scenes[0].elements[3];
        assert_eq!(dialogue.kind, ScriptElementKind::Dialogue);
        assert_eq!(dialogue.character.as_deref(), Some("JOHN"));
    }

    #[test]
    fn test_parse_fdx_scene_number() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<FinalDraft DocumentType="Script">
  <Content>
    <Paragraph Type="Scene Heading" Number="42A">
      <Text>INT. LAB - DAY</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Bubbling beakers.</Text>
    </Paragraph>
  </Content>
</FinalDraft>"#;
        let script = parse_fdx_str(xml, "test.fdx").unwrap();
        assert_eq!(script.scenes[0].scene_number.as_deref(), Some("42A"));
    }

    #[test]
    fn test_parse_fountain_basic() {
        let text = "Title: My Movie\n\
                     \n\
                     INT. OFFICE - DAY\n\
                     \n\
                     John enters.\n\
                     \n\
                     JOHN\n\
                     Hello there.\n\
                     \n\
                     EXT. PARK - NIGHT\n\
                     \n\
                     The wind howls.\n";

        let script = parse_fountain_str(text, "test.fountain");
        assert_eq!(script.title.as_deref(), Some("My Movie"));
        assert_eq!(script.scenes.len(), 2);
        assert_eq!(script.scenes[0].heading, "INT. OFFICE - DAY");
        assert_eq!(script.scenes[1].heading, "EXT. PARK - NIGHT");
    }

    #[test]
    fn test_parse_fountain_forced_heading() {
        let text = ".FLASHBACK - THE TRENCHES\n\n\
                     Soldiers crouch in mud.\n";
        let script = parse_fountain_str(text, "test.fountain");
        assert_eq!(script.scenes.len(), 1);
        assert_eq!(script.scenes[0].heading, "FLASHBACK - THE TRENCHES");
    }

    #[test]
    fn test_parse_fountain_scene_number() {
        let text = "INT. HOUSE - DAY #42#\n\n\
                     A quiet room.\n";
        let script = parse_fountain_str(text, "test.fountain");
        assert_eq!(script.scenes[0].scene_number.as_deref(), Some("42"));
    }

    #[test]
    fn test_parse_fountain_transitions() {
        let text = "INT. OFFICE - DAY\n\n\
                     John sits.\n\n\
                     CUT TO:\n\n\
                     EXT. PARK - DAY\n\n\
                     Birds sing.\n";
        let script = parse_fountain_str(text, "test.fountain");
        assert_eq!(script.scenes.len(), 2);
        // Transition should be in scene 1's elements.
        let has_transition = script.scenes[0]
            .elements
            .iter()
            .any(|e| e.kind == ScriptElementKind::Transition);
        assert!(has_transition);
    }

    #[test]
    fn test_parse_fountain_parenthetical() {
        let text = "INT. OFFICE - DAY\n\n\
                     JOHN\n\
                     (whispering)\n\
                     Hello.\n";
        let script = parse_fountain_str(text, "test.fountain");
        let elements = &script.scenes[0].elements;
        let paren = elements
            .iter()
            .find(|e| e.kind == ScriptElementKind::Parenthetical);
        assert!(paren.is_some());
        assert_eq!(paren.unwrap().text, "(whispering)");
        assert_eq!(paren.unwrap().character.as_deref(), Some("JOHN"));
    }

    #[test]
    fn test_parse_fountain_forced_character() {
        let text = "INT. OFFICE - DAY\n\n\
                     @McCLANE\n\
                     Yippee ki-yay.\n";
        let script = parse_fountain_str(text, "test.fountain");
        let char_el = script.scenes[0]
            .elements
            .iter()
            .find(|e| e.kind == ScriptElementKind::Character);
        assert!(char_el.is_some());
        assert_eq!(char_el.unwrap().text, "McCLANE");
    }

    #[test]
    fn test_full_text_contains_all_dialogue() {
        let text = "INT. OFFICE - DAY\n\n\
                     JOHN\n\
                     Hello there.\n\n\
                     JANE\n\
                     Hi John.\n";
        let script = parse_fountain_str(text, "test.fountain");
        let ft = &script.scenes[0].full_text;
        assert!(ft.contains("hello there"));
        assert!(ft.contains("hi john"));
        assert!(ft.contains("int. office - day"));
    }

    #[test]
    fn test_empty_script() {
        let script = parse_fountain_str("", "test.fountain");
        assert!(script.scenes.is_empty());
    }

    #[test]
    fn test_no_scene_heading_creates_unnamed_scene() {
        let text = "John walks in.\n\n\
                     JOHN\n\
                     Hello.\n";
        let script = parse_fountain_str(text, "test.fountain");
        assert_eq!(script.scenes.len(), 1);
        assert!(script.scenes[0].heading.starts_with("Scene "));
    }
}
