use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use noren_sdk::storage::atomic_write_text;
use serde::{Deserialize, Serialize};
use windows::core::{w, GUID, Interface, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_Desktop, FOLDERID_PublicDesktop, IEnumShellItems, IShellItem,
    IShellItem2, SHCreateItemFromParsingName, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// System.AppUserModel.ID = {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5
const PKEY_APPUSERMODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID {
        data1: 0x9F4C_2855,
        data2: 0x9F79,
        data3: 0x4B39,
        data4: [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3],
    },
    pid: 5,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEntry {
    pub name: String,
    pub id: String,
}

#[derive(Deserialize)]
struct RawStartApp {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "AppID")]
    id: String,
}

unsafe fn take_pwstr(pwstr: PWSTR) -> String {
    if pwstr.0.is_null() {
        return String::new();
    }
    let s = String::from_utf16_lossy(pwstr.as_wide()).trim().to_string();
    CoTaskMemFree(Some(pwstr.0 as _));
    s
}

/// Enumerate Start Menu applications directly via COM `shell:AppsFolder`.
pub fn load_apps_native() -> Vec<AppEntry> {
    unsafe {
        let com_ok = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let mut apps = Vec::new();

        if let Ok(folder) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(w!("shell:AppsFolder"), None)
        {
            if let Ok(enum_items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems)
            {
                let mut batch: [Option<IShellItem>; 1] = [None];
                loop {
                    let mut fetched = 0u32;
                    let hr = enum_items.Next(&mut batch, Some(&mut fetched));
                    if hr.is_err() || fetched == 0 {
                        break;
                    }
                    let Some(item) = batch[0].take() else {
                        break;
                    };

                    let name = item
                        .GetDisplayName(SIGDN_NORMALDISPLAY)
                        .ok()
                        .map(|p| take_pwstr(p))
                        .unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }

                    let mut id = item
                        .cast::<IShellItem2>()
                        .ok()
                        .and_then(|item2| item2.GetString(&PKEY_APPUSERMODEL_ID).ok())
                        .map(|p| take_pwstr(p))
                        .unwrap_or_default();
                    if id.is_empty() {
                        id = item
                            .GetDisplayName(SIGDN_PARENTRELATIVEPARSING)
                            .ok()
                            .map(|p| take_pwstr(p))
                            .unwrap_or_default();
                    }
                    if !id.is_empty() {
                        apps.push(AppEntry { name, id });
                    }
                }
            }
        }

        if com_ok {
            CoUninitialize();
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps.dedup_by(|a, b| a.name == b.name && a.id == b.id);
        apps
    }
}

fn load_apps_ps() -> Vec<AppEntry> {
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
                  ConvertTo-Json -Compress -InputObject @(Get-StartApps)";
    let Ok(out) = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim_start_matches('\u{feff}');
    let raw: Vec<RawStartApp> = serde_json::from_str(text).unwrap_or_default();

    let mut apps: Vec<AppEntry> = raw
        .into_iter()
        .filter(|a| !a.name.trim().is_empty() && !a.id.is_empty())
        .map(|a| AppEntry {
            name: a.name,
            id: a.id,
        })
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.name == b.name && a.id == b.id);
    apps
}

fn known_folder_path(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = take_pwstr(raw);
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

pub fn load_desktop_shortcuts_from_dirs(dirs: &[PathBuf]) -> Vec<AppEntry> {
    let mut entries = Vec::new();
    let mut seen_paths = HashSet::new();
    for dir in dirs {
        let Ok(rd) = fs::read_dir(dir) else {
            continue;
        };
        for item in rd.flatten() {
            let path = item.path();
            let Ok(ft) = item.file_type() else {
                continue;
            };
            if ft.is_dir() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if !matches!(ext.as_str(), "lnk" | "url" | "appref-ms" | "exe") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let full = path.to_string_lossy().into_owned();
            if seen_paths.insert(full.to_lowercase()) {
                entries.push(AppEntry {
                    name,
                    id: format!("path:{full}"),
                });
            }
        }
    }
    entries
}

fn load_desktop_shortcuts() -> Vec<AppEntry> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    let mut push_dir = |p: PathBuf| {
        let k = p.to_string_lossy().to_lowercase();
        if p.is_dir() && seen.insert(k) {
            dirs.push(p);
        }
    };

    if let Some(user_desktop) = known_folder_path(&FOLDERID_Desktop) {
        push_dir(user_desktop);
    } else if let Some(profile) = std::env::var_os("USERPROFILE") {
        push_dir(PathBuf::from(profile).join("Desktop"));
    }
    if let Some(public_desktop) = known_folder_path(&FOLDERID_PublicDesktop) {
        push_dir(public_desktop);
    }

    load_desktop_shortcuts_from_dirs(&dirs)
}

pub fn merge_start_and_desktop_apps(
    mut start_apps: Vec<AppEntry>,
    desktop_apps: Vec<AppEntry>,
) -> Vec<AppEntry> {
    let mut seen_names: HashSet<String> = start_apps
        .iter()
        .map(|a| a.name.trim().to_lowercase())
        .collect();
    for entry in desktop_apps {
        let key = entry.name.trim().to_lowercase();
        if seen_names.insert(key) {
            start_apps.push(entry);
        }
    }
    start_apps.sort_by_key(|a| a.name.to_lowercase());
    start_apps
}

pub fn load_apps() -> Vec<AppEntry> {
    let native = load_apps_native();
    let start_apps = if !native.is_empty() {
        native
    } else {
        load_apps_ps()
    };
    let desktop_apps = load_desktop_shortcuts();
    merge_start_and_desktop_apps(start_apps, desktop_apps)
}

pub fn launch_app(id: &str) -> Result<(), String> {
    if let Some(full_path) = id.strip_prefix("path:") {
        return crate::actions::open_file_or_folder(full_path);
    }
    Command::new("explorer.exe")
        .arg(format!("shell:AppsFolder\\{id}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FrecencyEntry {
    pub count: u32,
    pub last_used_secs: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FrecencyStore {
    #[serde(default)]
    pub entries: HashMap<String, FrecencyEntry>,
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn frecency_path() -> PathBuf {
    if let Ok(override_dir) = std::env::var("NOREN_CONFIG_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("launcher").join("frecency.json");
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local)
                .join("Noren")
                .join("launcher")
                .join("frecency.json");
        }
    }
    std::env::temp_dir().join("noren-launcher-frecency.json")
}

impl FrecencyStore {
    pub fn load() -> Self {
        Self::load_from(&frecency_path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save_to(&self, path: &std::path::Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = atomic_write_text(path, &text);
        }
    }

    pub fn record_launch(&mut self, app_id: &str, now_secs: u64) {
        self.record_launch_at(&frecency_path(), app_id, now_secs);
    }

    pub fn record_launch_at(&mut self, path: &std::path::Path, app_id: &str, now_secs: u64) {
        let entry = self.entries.entry(app_id.to_string()).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.last_used_secs = now_secs;
        self.save_to(path);
    }

    pub fn score(&self, app_id: &str, now_secs: u64) -> f64 {
        let Some(entry) = self.entries.get(app_id) else {
            return 0.0;
        };
        if entry.count == 0 {
            return 0.0;
        }
        let age = now_secs.saturating_sub(entry.last_used_secs);
        let recency = if age <= 14_400 {
            4.0
        } else if age <= 86_400 {
            2.5
        } else if age <= 604_800 {
            1.5
        } else if age <= 2_592_000 {
            1.0
        } else {
            0.5
        };
        recency * ((entry.count as f64).ln_1p() + 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frecency_record_and_decay_scoring() {
        let dir = std::env::temp_dir().join(format!(
            "noren_launcher_frecency_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frecency.json");

        let now = 1_750_000_000u64;
        let mut store = FrecencyStore::default();
        assert_eq!(store.score("missing", now), 0.0);

        store.record_launch_at(&path, "app.recent", now - 60);
        store.record_launch_at(&path, "app.recent", now - 30);
        store.record_launch_at(&path, "app.old", now - 3_000_000);

        let recent_score = store.score("app.recent", now);
        let old_score = store.score("app.old", now);
        assert!(recent_score > old_score && old_score > 0.0);

        let reloaded = FrecencyStore::load_from(&path);
        assert_eq!(reloaded.entries.get("app.recent").unwrap().count, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn desktop_shortcuts_load_and_merge_without_duplicates() {
        let dir = std::env::temp_dir().join(format!(
            "noren_launcher_desktop_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("SubFolder")).unwrap();
        fs::write(dir.join("Work - Chrome.lnk"), b"lnk").unwrap();
        fs::write(dir.join("Discord.lnk"), b"lnk").unwrap();
        fs::write(dir.join("Docs.url"), b"url").unwrap();
        fs::write(dir.join("MyTool.exe"), b"exe").unwrap();
        fs::write(dir.join("readme.txt"), b"txt").unwrap();

        let desktop = load_desktop_shortcuts_from_dirs(&[dir.clone()]);
        let desktop_names: Vec<&str> = desktop.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(desktop.len(), 4);
        assert!(desktop_names.contains(&"Work - Chrome"));
        assert!(desktop_names.contains(&"Docs"));
        assert!(desktop_names.contains(&"MyTool"));
        assert!(desktop.iter().all(|e| e.id.starts_with("path:")));

        let start = vec![AppEntry {
            name: "Discord".to_string(),
            id: "com.squirrel.Discord.Discord".to_string(),
        }];
        let merged = merge_start_and_desktop_apps(start, desktop);
        assert_eq!(merged.len(), 4);
        let discord = merged.iter().find(|a| a.name == "Discord").unwrap();
        assert_eq!(discord.id, "com.squirrel.Discord.Discord");
        assert!(merged.iter().any(|a| a.name == "Work - Chrome"));

        let _ = fs::remove_dir_all(&dir);
    }
}
