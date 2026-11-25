use std::collections::HashMap;

use noren_sdk::paths::module_config_path;
use noren_sdk::schema::{
    default_values_for_manifest, parse_shortcut_list, ConfigFormat, FieldDef, FieldGroup,
    FieldKind, FieldValue, ModuleManifest, ShortcutEntry,
};
use noren_sdk::storage::{load_config_values, save_config_values};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_BACK, VK_DOWN,
    VK_ESCAPE, VK_LEFT, VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7,
    VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB,
    VK_UP,
};

fn field_bool(key: &str, label: &str, default_val: bool, description: Option<&str>) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Boolean,
        description: description.map(ToString::to_string),
        requires_running: false,
        default_bool: Some(default_val),
        default_number: None,
        default_text: None,
        min: None,
        max: None,
        step: None,
        choices: Vec::new(),
        nullable: false,
        placeholder: None,
        command: None,
    }
}

fn field_number(
    key: &str,
    label: &str,
    default_val: f64,
    min: f64,
    max: f64,
    step: f64,
    description: Option<&str>,
) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Number,
        description: description.map(ToString::to_string),
        requires_running: false,
        default_bool: None,
        default_number: Some(default_val),
        default_text: None,
        min: Some(min),
        max: Some(max),
        step: Some(step),
        choices: Vec::new(),
        nullable: false,
        placeholder: None,
        command: None,
    }
}

fn field_choice(
    key: &str,
    label: &str,
    choices: &[&str],
    default_val: &str,
    description: Option<&str>,
) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Choice,
        description: description.map(ToString::to_string),
        requires_running: false,
        default_bool: None,
        default_number: None,
        default_text: Some(default_val.to_string()),
        min: None,
        max: None,
        step: None,
        choices: choices.iter().map(|s| (*s).to_string()).collect(),
        nullable: false,
        placeholder: None,
        command: None,
    }
}

fn field_text(
    key: &str,
    label: &str,
    default_val: &str,
    placeholder: Option<&str>,
    description: Option<&str>,
) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Text,
        description: description.map(ToString::to_string),
        requires_running: false,
        default_bool: None,
        default_number: None,
        default_text: Some(default_val.to_string()),
        min: None,
        max: None,
        step: None,
        choices: Vec::new(),
        nullable: false,
        placeholder: placeholder.map(ToString::to_string),
        command: None,
    }
}

fn field_key_chord(
    key: &str,
    label: &str,
    default_val: &str,
    description: Option<&str>,
) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::KeyChord,
        description: description.map(ToString::to_string),
        requires_running: false,
        default_bool: None,
        default_number: None,
        default_text: Some(default_val.to_string()),
        min: None,
        max: None,
        step: None,
        choices: Vec::new(),
        nullable: false,
        placeholder: Some("alt+space".to_string()),
        command: None,
    }
}

fn field_action(key: &str, label: &str, command: &str, description: Option<&str>) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::Action,
        description: description.map(ToString::to_string),
        requires_running: true,
        default_bool: None,
        default_number: None,
        default_text: None,
        min: None,
        max: None,
        step: None,
        choices: Vec::new(),
        nullable: false,
        placeholder: None,
        command: Some(command.to_string()),
    }
}

fn field_shortcut_list(
    key: &str,
    label: &str,
    default_val: &str,
    description: Option<&str>,
) -> FieldDef {
    FieldDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FieldKind::ShortcutList,
        description: description.map(ToString::to_string),
        requires_running: false,
        default_bool: None,
        default_number: None,
        default_text: Some(default_val.to_string()),
        min: None,
        max: None,
        step: None,
        choices: Vec::new(),
        nullable: false,
        placeholder: None,
        command: None,
    }
}

pub fn default_bang_shortcuts_text() -> String {
    [
        "yt | YouTube | https://www.youtube.com/results?search_query={query}",
        "gh | GitHub | https://github.com/search?q={query}",
        "ddg | DuckDuckGo | https://duckduckgo.com/?q={query}",
        "g | Google | https://www.google.com/search?q={query}",
        "wiki | Wikipedia | https://en.wikipedia.org/w/index.php?search={query}",
        "bing | Bing | https://www.bing.com/search?q={query}",
        "brave | Brave | https://search.brave.com/search?q={query}",
    ]
    .join("\n")
}

/// Declarative settings manifest installed into `%APPDATA%\Noren\manifests\launcher.toml`.
pub fn build_launcher_manifest() -> ModuleManifest {
    ModuleManifest {
        id: "launcher".to_string(),
        name: "Launcher".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        executable: std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        config_format: ConfigFormat::Toml,
        config_file: "config.toml".to_string(),
        groups: vec![
            FieldGroup {
                id: "general".to_string(),
                title: "General & Layout".to_string(),
                fields: vec![
                    field_key_chord(
                        "hotkey",
                        "Toggle Hotkey",
                        "alt+space",
                        Some("Global shortcut to toggle the launcher (or 'off')."),
                    ),
                    field_bool(
                        "compact_mode",
                        "Compact Mode",
                        false,
                        Some("Hide the application list until you start typing."),
                    ),
                    field_number(
                        "max_visible_rows",
                        "Max Visible Rows",
                        8.0,
                        4.0,
                        14.0,
                        1.0,
                        Some("Maximum visible result rows before scrolling."),
                    ),
                    field_number(
                        "panel_width",
                        "Panel Width (px)",
                        640.0,
                        480.0,
                        900.0,
                        20.0,
                        Some("Launcher panel width in logical pixels."),
                    ),
                    field_bool(
                        "autostart",
                        "Run at Login",
                        false,
                        Some("Run Noren Launcher automatically at Windows login."),
                    ),
                ],
            },
            FieldGroup {
                id: "web_search".to_string(),
                title: "Unified ! Shortcuts".to_string(),
                fields: vec![
                    field_choice(
                        "web_engine",
                        "Default Browser Engine (!b)",
                        &["google", "duckduckgo", "bing", "brave", "custom"],
                        "google",
                        Some(
                            "Engine used by !b <query> (unless overridden in Custom ! Shortcuts below).",
                        ),
                    ),
                    field_text(
                        "web_custom_url",
                        "Custom !b URL Template",
                        "https://www.google.com/search?q={query}",
                        Some("https://example.com/search?q={query}"),
                        Some("Custom URL template ({query} or %s is replaced with the encoded query)."),
                    ),
                    field_shortcut_list(
                        "bang_shortcuts",
                        "Custom ! Shortcuts",
                        &default_bang_shortcuts_text(),
                        Some(
                            "Type !<prefix> <query> directly in Launcher (e.g. !yt lofi, !gh tokio). Omit {query} for direct bookmark URLs.",
                        ),
                    ),
                ],
            },
            FieldGroup {
                id: "wallpaper".to_string(),
                title: "Wallpaper (>wallpaper)".to_string(),
                fields: vec![
                    field_choice(
                        "wallpaper_mode",
                        "Wallpaper Switcher Mode",
                        &["builtin", "external"],
                        "builtin",
                        Some("Built-in carousel inside Launcher, or run an external command."),
                    ),
                    field_text(
                        "wallpaper_dir",
                        "Wallpaper Folder",
                        "",
                        Some("D:\\Wallpapers (empty = Windows Pictures)"),
                        Some("Wallpaper folder for >wallpaper (empty uses Windows Pictures)."),
                    ),
                    field_text(
                        "wallpaper_command",
                        "External Wallpaper Command",
                        "",
                        Some("\"C:\\Tools\\wallswitch.exe\""),
                        Some("Command run when Wallpaper Mode is set to external."),
                    ),
                ],
            },
            FieldGroup {
                id: "actions".to_string(),
                title: "Live Launcher Actions".to_string(),
                fields: vec![
                    field_action("toggle_action", "Toggle Launcher Window", "toggle", None),
                    field_action(
                        "reload_apps_action",
                        "Reload Application List",
                        "reload_apps",
                        None,
                    ),
                ],
            },
        ],
        advanced_editors: Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LauncherConfig {
    pub hotkey: String,
    pub compact_mode: bool,
    pub max_visible_rows: usize,
    pub panel_width: f32,
    pub autostart: bool,
    pub web_engine: String,
    pub web_custom_url: String,
    pub bang_shortcuts: String,
    pub wallpaper_mode: String,
    pub wallpaper_dir: String,
    pub wallpaper_command: String,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        let manifest = build_launcher_manifest();
        Self::from_values(&default_values_for_manifest(&manifest))
    }
}

impl LauncherConfig {
    pub fn from_values(values: &HashMap<String, FieldValue>) -> Self {
        let get_bool = |k: &str, d: bool| -> bool {
            match values.get(k) {
                Some(FieldValue::Bool(v)) => *v,
                _ => d,
            }
        };
        let get_num = |k: &str, d: f64| -> f64 {
            match values.get(k) {
                Some(FieldValue::Number(v)) => *v,
                _ => d,
            }
        };
        let get_text = |k: &str, d: &str| -> String {
            match values.get(k) {
                Some(FieldValue::Text(v)) => v.clone(),
                _ => d.to_string(),
            }
        };

        Self {
            hotkey: get_text("hotkey", "alt+space"),
            compact_mode: get_bool("compact_mode", false),
            max_visible_rows: get_num("max_visible_rows", 8.0).round().clamp(4.0, 14.0) as usize,
            panel_width: get_num("panel_width", 640.0).round().clamp(480.0, 900.0) as f32,
            autostart: get_bool("autostart", false),
            web_engine: get_text("web_engine", "google"),
            web_custom_url: get_text("web_custom_url", "https://www.google.com/search?q={query}"),
            bang_shortcuts: get_text("bang_shortcuts", &default_bang_shortcuts_text()),
            wallpaper_mode: get_text("wallpaper_mode", "builtin"),
            wallpaper_dir: get_text("wallpaper_dir", ""),
            wallpaper_command: get_text("wallpaper_command", ""),
        }
    }

    pub fn parsed_bangs(&self) -> Vec<ShortcutEntry> {
        parse_shortcut_list(&self.bang_shortcuts)
            .into_iter()
            .filter_map(|mut entry| {
                let trig = entry.trigger.trim().trim_start_matches('!').trim().to_string();
                let url = entry.url.trim().to_string();
                if trig.is_empty() || url.is_empty() {
                    return None;
                }
                if entry.label.trim().is_empty() {
                    entry.label = trig.clone();
                } else {
                    entry.label = entry.label.trim().to_string();
                }
                entry.trigger = trig;
                entry.url = url;
                Some(entry)
            })
            .collect()
    }

    pub fn load_or_default(manifest: &ModuleManifest) -> Self {
        let path = module_config_path(&manifest.id, &manifest.config_file);
        Self::load_from_path(&path, manifest)
    }

    pub fn load_from_path(path: &std::path::Path, manifest: &ModuleManifest) -> Self {
        match load_config_values(path, manifest) {
            Ok(vals) => Self::from_values(&vals),
            Err(_) => Self::default(),
        }
    }

    pub fn apply_updates(
        manifest: &ModuleManifest,
        updates: HashMap<String, FieldValue>,
    ) -> Result<Self, String> {
        let path = module_config_path(&manifest.id, &manifest.config_file);
        Self::apply_updates_at(&path, manifest, updates)
    }

    pub fn apply_updates_at(
        path: &std::path::Path,
        manifest: &ModuleManifest,
        updates: HashMap<String, FieldValue>,
    ) -> Result<Self, String> {
        let mut vals = load_config_values(path, manifest).map_err(|e| e.to_string())?;
        for (k, v) in updates {
            vals.insert(k, v);
        }
        save_config_values(path, manifest, &vals).map_err(|e| e.to_string())?;
        Ok(Self::from_values(&vals))
    }
}

/// Parses a human-readable hotkey chord like `"alt+space"` into Win32 `RegisterHotKey` inputs.
pub fn parse_hotkey_chord(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let trimmed = s.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed == "off" || trimmed == "none" {
        return None;
    }

    let parts: Vec<&str> = trimmed
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }

    let mut mods = HOT_KEY_MODIFIERS(0);
    for m in &parts[..parts.len() - 1] {
        match *m {
            "win" => mods |= MOD_WIN,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            _ => return None,
        }
    }
    if mods == HOT_KEY_MODIFIERS(0) {
        return None;
    }

    let key = parts[parts.len() - 1];
    let vk: u32 = match key {
        "space" => VK_SPACE.0 as u32,
        "tab" => VK_TAB.0 as u32,
        "enter" | "return" => VK_RETURN.0 as u32,
        "esc" | "escape" => VK_ESCAPE.0 as u32,
        "backspace" | "back" => VK_BACK.0 as u32,
        "up" => VK_UP.0 as u32,
        "down" => VK_DOWN.0 as u32,
        "left" => VK_LEFT.0 as u32,
        "right" => VK_RIGHT.0 as u32,
        "/" | "slash" => VK_OEM_2.0 as u32,
        "\\" | "backslash" => VK_OEM_5.0 as u32,
        ";" => VK_OEM_1.0 as u32,
        "'" => VK_OEM_7.0 as u32,
        "," => VK_OEM_COMMA.0 as u32,
        "." => VK_OEM_PERIOD.0 as u32,
        "[" => VK_OEM_4.0 as u32,
        "]" => VK_OEM_6.0 as u32,
        "-" => VK_OEM_MINUS.0 as u32,
        "=" => VK_OEM_PLUS.0 as u32,
        "`" => VK_OEM_3.0 as u32,
        s if s.len() == 1 => {
            let b = s.as_bytes()[0];
            if b.is_ascii_lowercase() {
                b.to_ascii_uppercase() as u32
            } else if b.is_ascii_digit() {
                b as u32
            } else {
                return None;
            }
        }
        s if s.starts_with('f') => {
            let num: u32 = s[1..].parse().ok()?;
            if (1..=12).contains(&num) {
                0x70 + (num - 1)
            } else {
                return None;
            }
        }
        _ => return None,
    };

    Some((mods | MOD_NOREPEAT, vk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn manifest_and_config_roundtrip() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("noren_launcher_cfg_test_{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");

        let manifest = build_launcher_manifest();
        assert_eq!(manifest.id, "launcher");
        assert_eq!(manifest.config_format, ConfigFormat::Toml);

        let mut updates = HashMap::new();
        updates.insert(
            "hotkey".to_string(),
            FieldValue::Text("ctrl+space".to_string()),
        );
        updates.insert("compact_mode".to_string(), FieldValue::Bool(true));
        updates.insert("max_visible_rows".to_string(), FieldValue::Number(10.0));
        updates.insert(
            "web_engine".to_string(),
            FieldValue::Text("duckduckgo".to_string()),
        );
        updates.insert(
            "bang_shortcuts".to_string(),
            FieldValue::Text("r | Reddit | https://www.reddit.com/search/?q={query}".to_string()),
        );

        let applied = LauncherConfig::apply_updates_at(&cfg_path, &manifest, updates).unwrap();
        assert_eq!(applied.hotkey, "ctrl+space");
        assert!(applied.compact_mode);
        assert_eq!(applied.max_visible_rows, 10);
        assert_eq!(applied.web_engine, "duckduckgo");
        assert_eq!(applied.parsed_bangs().len(), 1);
        assert_eq!(applied.parsed_bangs()[0].trigger, "r");

        let reloaded = LauncherConfig::load_from_path(&cfg_path, &manifest);
        assert_eq!(reloaded, applied);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_hotkey_chord_cases() {
        let (mods, vk) = parse_hotkey_chord("alt+space").unwrap();
        assert_eq!(mods, MOD_ALT | MOD_NOREPEAT);
        assert_eq!(vk, VK_SPACE.0 as u32);

        let (mods2, vk2) = parse_hotkey_chord("Ctrl + Shift + P").unwrap();
        assert_eq!(mods2, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT);
        assert_eq!(vk2, b'P' as u32);

        let (mods3, vk3) = parse_hotkey_chord("win+/").unwrap();
        assert_eq!(mods3, MOD_WIN | MOD_NOREPEAT);
        assert_eq!(vk3, VK_OEM_2.0 as u32);

        assert!(parse_hotkey_chord("off").is_none());
        assert!(parse_hotkey_chord("none").is_none());
        assert!(parse_hotkey_chord("space").is_none());
        assert!(parse_hotkey_chord("super+space").is_none());
    }
}
