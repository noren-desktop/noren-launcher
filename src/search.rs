use noren_sdk::ShortcutEntry;
use serde_json::Value;

use crate::apps::{AppEntry, FrecencyStore};
use crate::config::LauncherConfig;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAction {
    pub peer: String,
    pub peer_title: String,
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub cap: String,
    pub cmd: String,
}

fn peer_display_name(peer: &str) -> String {
    match peer {
        "wm" => "Window Manager".to_string(),
        "notch" => "Notch".to_string(),
        "bar" => "Bar".to_string(),
        other => other.to_string(),
    }
}

/// Parses `launcher.actions` state snapshots (e.g. from `noren-wm`).
pub fn parse_remote_launcher_actions(peer: &str, val: &Value) -> Vec<RemoteAction> {
    let Some(arr) = val.get("actions").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let peer_title = peer_display_name(peer);
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let cap = entry
            .get("cap")
            .and_then(|v| v.as_str())
            .unwrap_or("launcher.actions")
            .to_string();
        let cmd = entry
            .get("cmd")
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        out.push(RemoteAction {
            peer: peer.to_string(),
            peer_title: peer_title.clone(),
            id: id.to_string(),
            label,
            description,
            cap,
            cmd,
        });
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Apps,
    Actions,
    Bangs,
}

pub fn detect_mode(query: &str) -> SearchMode {
    let trimmed_start = query.trim_start();
    if trimmed_start.starts_with('>') {
        SearchMode::Actions
    } else if trimmed_start.starts_with('!') {
        SearchMode::Bangs
    } else {
        SearchMode::Apps
    }
}

/// Case-insensitive fuzzy match score. `0` = no match.
pub fn match_score(text: &str, query: &str) -> u32 {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return 100;
    }
    let t = text.to_lowercase();
    if t == q {
        return 100;
    }
    if t.starts_with(&q) {
        return 80;
    }
    if t.split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.'))
        .any(|w| w.starts_with(&q))
    {
        return 65;
    }
    if t.contains(&q) {
        return 45;
    }
    let mut qi = q.chars();
    let mut current = qi.next();
    for ch in t.chars() {
        if let Some(target) = current {
            if ch == target {
                current = qi.next();
            }
        } else {
            break;
        }
    }
    if current.is_none() {
        20
    } else {
        0
    }
}

pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push_str("%20"),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn template_for_engine(engine: &str, custom_template: &str) -> (&'static str, String) {
    match engine.trim().to_ascii_lowercase().as_str() {
        "duckduckgo" | "ddg" => ("DuckDuckGo", "https://duckduckgo.com/?q={query}".to_string()),
        "bing" => ("Bing", "https://www.bing.com/search?q={query}".to_string()),
        "brave" => ("Brave", "https://search.brave.com/search?q={query}".to_string()),
        "custom" => {
            let tpl = custom_template.trim();
            if tpl.is_empty() {
                ("Google", "https://www.google.com/search?q={query}".to_string())
            } else {
                ("Custom Web", tpl.to_string())
            }
        }
        _ => ("Google", "https://www.google.com/search?q={query}".to_string()),
    }
}

fn is_search_template(url: &str) -> bool {
    url.contains("{query}") || url.contains("%s")
}

fn apply_url_template(template: &str, term: &str) -> String {
    let trimmed = term.trim();
    if trimmed.is_empty() && !is_search_template(template) {
        return template.to_string();
    }
    let encoded = percent_encode(trimmed);
    if template.contains("{query}") {
        template.replace("{query}", &encoded)
    } else if template.contains("%s") {
        template.replace("%s", &encoded)
    } else {
        format!("{template}{encoded}")
    }
}

fn looks_like_direct_url(term: &str) -> bool {
    let t = term.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return false;
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return t.len() > 8;
    }
    let host = t.split('/').next().unwrap_or("");
    if host == "localhost" || host.starts_with("localhost:") {
        return true;
    }
    !host.starts_with('.') && !host.ends_with('.') && host.contains('.')
}

fn normalize_direct_url(term: &str) -> String {
    let t = term.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else if t == "localhost" || t.starts_with("localhost:") || t.starts_with("localhost/") {
        format!("http://{t}")
    } else {
        format!("https://{t}")
    }
}

fn effective_bangs(cfg: &LauncherConfig) -> Vec<ShortcutEntry> {
    let mut bangs = cfg.parsed_bangs();
    if !bangs.iter().any(|b| b.trigger.eq_ignore_ascii_case("b")) {
        let (engine_name, engine_tpl) = template_for_engine(&cfg.web_engine, &cfg.web_custom_url);
        bangs.insert(
            0,
            ShortcutEntry {
                trigger: "b".to_string(),
                label: format!("{engine_name} (Browser)"),
                url: engine_tpl,
            },
        );
    }
    bangs
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemEffect {
    LaunchApp { id: String },
    OpenWebUrl { url: String },
    FillQuery { text: String },
    OpenSettings,
    OpenWallpaper,
    ReloadApps,
    PowerCommand { id: String },
    RemoteNipCommand { peer: String, cap: String, cmd: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchItem {
    pub key: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub glyph: char,
    pub app_id: Option<String>,
    pub dangerous: bool,
    pub effect: ItemEffect,
}

fn remote_action_glyph(id: &str) -> char {
    match id {
        "overview" => '\u{F0E2}',
        "cheatsheet" => '\u{E765}',
        _ => '\u{E756}',
    }
}

fn builtin_actions() -> Vec<SearchItem> {
    vec![
        SearchItem {
            key: "builtin:wallpaper".to_string(),
            title: "Wallpaper".to_string(),
            subtitle: Some("Change desktop wallpaper".to_string()),
            glyph: '\u{E8B9}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::OpenWallpaper,
        },
        SearchItem {
            key: "builtin:settings".to_string(),
            title: "Settings".to_string(),
            subtitle: Some("Open Noren Control Center".to_string()),
            glyph: '\u{E713}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::OpenSettings,
        },
        SearchItem {
            key: "builtin:reload_apps".to_string(),
            title: "Reload apps".to_string(),
            subtitle: Some("Reload Start Menu application list".to_string()),
            glyph: '\u{E72C}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::ReloadApps,
        },
        SearchItem {
            key: "builtin:lock".to_string(),
            title: "Lock".to_string(),
            subtitle: Some("Lock the screen".to_string()),
            glyph: '\u{E72E}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::PowerCommand {
                id: "lock".to_string(),
            },
        },
        SearchItem {
            key: "builtin:sleep".to_string(),
            title: "Sleep".to_string(),
            subtitle: Some("Put PC to sleep".to_string()),
            glyph: '\u{E708}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::PowerCommand {
                id: "sleep".to_string(),
            },
        },
        SearchItem {
            key: "builtin:hibernate".to_string(),
            title: "Hibernate".to_string(),
            subtitle: Some("Save session to disk and power off".to_string()),
            glyph: '\u{E823}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::PowerCommand {
                id: "hibernate".to_string(),
            },
        },
        SearchItem {
            key: "builtin:display_off".to_string(),
            title: "Display off".to_string(),
            subtitle: Some("Turn off the display only".to_string()),
            glyph: '\u{E7F4}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::PowerCommand {
                id: "display_off".to_string(),
            },
        },
        SearchItem {
            key: "builtin:logout".to_string(),
            title: "Sign out".to_string(),
            subtitle: Some("Sign out of Windows".to_string()),
            glyph: '\u{F3B1}',
            app_id: None,
            dangerous: true,
            effect: ItemEffect::PowerCommand {
                id: "logout".to_string(),
            },
        },
        SearchItem {
            key: "builtin:restart".to_string(),
            title: "Restart".to_string(),
            subtitle: Some("Restart the PC".to_string()),
            glyph: '\u{E777}',
            app_id: None,
            dangerous: true,
            effect: ItemEffect::PowerCommand {
                id: "restart".to_string(),
            },
        },
        SearchItem {
            key: "builtin:shutdown".to_string(),
            title: "Shutdown".to_string(),
            subtitle: Some("Shut down the PC".to_string()),
            glyph: '\u{E7E8}',
            app_id: None,
            dangerous: true,
            effect: ItemEffect::PowerCommand {
                id: "shutdown".to_string(),
            },
        },
    ]
}

fn query_actions(filter: &str, remote_actions: &[RemoteAction]) -> Vec<SearchItem> {
    let builtins = builtin_actions();
    let mut all: Vec<SearchItem> = Vec::with_capacity(builtins.len() + remote_actions.len());
    all.extend(builtins.iter().take(2).cloned());
    for ra in remote_actions {
        let subtitle = match &ra.description {
            Some(desc) => format!("{desc} · {}", ra.peer_title),
            None => ra.peer_title.clone(),
        };
        all.push(SearchItem {
            key: format!("remote:{}:{}", ra.peer, ra.id),
            title: ra.label.clone(),
            subtitle: Some(subtitle),
            glyph: remote_action_glyph(&ra.id),
            app_id: None,
            dangerous: false,
            effect: ItemEffect::RemoteNipCommand {
                peer: ra.peer.clone(),
                cap: ra.cap.clone(),
                cmd: ra.cmd.clone(),
            },
        });
    }
    all.extend(builtins.into_iter().skip(2));

    let f = filter.trim();
    if f.is_empty() {
        return all;
    }

    let mut scored: Vec<(u32, usize, SearchItem)> = all
        .into_iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let short_id = item.key.rsplit(':').next().unwrap_or(&item.key);
            let s = match_score(&item.title, f).max(match_score(short_id, f));
            (s > 0).then_some((s, idx, item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

fn bang_to_hint_item(b: &ShortcutEntry, prefix_hint: bool) -> SearchItem {
    if is_search_template(&b.url) {
        let subtitle = if prefix_hint {
            format!("Press Enter or Tab to search {} (!{} <query>)", b.label, b.trigger)
        } else {
            b.url.clone()
        };
        SearchItem {
            key: format!("bang:hint:{}", b.trigger),
            title: format!("{} (!{})", b.label, b.trigger),
            subtitle: Some(subtitle),
            glyph: '\u{E774}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::FillQuery {
                text: format!("!{} ", b.trigger),
            },
        }
    } else {
        SearchItem {
            key: format!("bang:open:{}", b.trigger),
            title: format!("Open {} (!{})", b.label, b.trigger),
            subtitle: Some(b.url.clone()),
            glyph: '\u{E774}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::OpenWebUrl { url: b.url.clone() },
        }
    }
}

fn query_bangs(query: &str, cfg: &LauncherConfig) -> Vec<SearchItem> {
    let after = query.trim_start().strip_prefix('!').unwrap_or("");
    let bangs = effective_bangs(cfg);
    let (default_engine_name, default_engine_tpl) =
        template_for_engine(&cfg.web_engine, &cfg.web_custom_url);

    // Bare "!" -> list all unified ! shortcuts.
    if after.is_empty() {
        return bangs
            .iter()
            .map(|b| bang_to_hint_item(b, false))
            .collect();
    }

    // No whitespace yet: user is typing "!<partial>" (e.g. "!y", "!yt", "!b", "!mail").
    if !after.chars().any(char::is_whitespace) {
        let partial = after.trim();
        let partial_lower = partial.to_lowercase();
        let mut matched: Vec<(u32, usize, &ShortcutEntry)> = bangs
            .iter()
            .enumerate()
            .filter_map(|(idx, b)| {
                let trig_lower = b.trigger.to_lowercase();
                let s = if trig_lower == partial_lower {
                    200
                } else if trig_lower.starts_with(&partial_lower) {
                    150
                } else {
                    match_score(&b.label, partial).max(match_score(&b.trigger, partial))
                };
                (s > 0).then_some((s, idx, b))
            })
            .collect();
        matched.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        if !matched.is_empty() {
            return matched
                .into_iter()
                .map(|(_, _, b)| bang_to_hint_item(b, true))
                .collect();
        }

        let fallback_url = apply_url_template(&default_engine_tpl, partial);
        return vec![SearchItem {
            key: format!("bang:fallback:{partial}"),
            title: format!("Search {default_engine_name} for \"{partial}\""),
            subtitle: Some(fallback_url.clone()),
            glyph: '\u{E774}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::OpenWebUrl { url: fallback_url },
        }];
    }

    // Contains whitespace: "!<trigger> <term>" (including "!yt ").
    let mut parts = after.splitn(2, char::is_whitespace);
    let trigger = parts.next().unwrap_or("").trim();
    let term = parts.next().unwrap_or("").trim();

    if let Some(b) = bangs.iter().find(|b| b.trigger.eq_ignore_ascii_case(trigger)) {
        if !is_search_template(&b.url) {
            let url = apply_url_template(&b.url, term);
            return vec![SearchItem {
                key: format!("bang:direct:{}:{term}", b.trigger),
                title: format!("Open {} (!{})", b.label, b.trigger),
                subtitle: Some(url.clone()),
                glyph: '\u{E774}',
                app_id: None,
                dangerous: false,
                effect: ItemEffect::OpenWebUrl { url },
            }];
        }

        if term.is_empty() {
            return vec![SearchItem {
                key: format!("bang:prompt:{}", b.trigger),
                title: format!("Type a query to search {}…", b.label),
                subtitle: Some(format!("Example: !{} rust", b.trigger)),
                glyph: '\u{E774}',
                app_id: None,
                dangerous: false,
                effect: ItemEffect::FillQuery {
                    text: format!("!{} ", b.trigger),
                },
            }];
        }

        let search_url = apply_url_template(&b.url, term);
        let mut items = Vec::with_capacity(2);
        if b.trigger.eq_ignore_ascii_case("b") && looks_like_direct_url(term) {
            let direct_url = normalize_direct_url(term);
            items.push(SearchItem {
                key: format!("bang:b:direct:{term}"),
                title: format!("Open \"{direct_url}\" in Browser"),
                subtitle: Some(direct_url.clone()),
                glyph: '\u{E774}',
                app_id: None,
                dangerous: false,
                effect: ItemEffect::OpenWebUrl { url: direct_url },
            });
        }
        items.push(SearchItem {
            key: format!("bang:{}:{term}", b.trigger),
            title: format!("Search {} for \"{term}\"", b.label),
            subtitle: Some(search_url.clone()),
            glyph: '\u{E774}',
            app_id: None,
            dangerous: false,
            effect: ItemEffect::OpenWebUrl { url: search_url },
        });
        return items;
    }

    let raw_term = after.trim();
    let fallback_url = apply_url_template(&default_engine_tpl, raw_term);
    vec![SearchItem {
        key: format!("bang:unknown:{raw_term}"),
        title: format!("Search {default_engine_name} for \"{raw_term}\""),
        subtitle: Some(fallback_url.clone()),
        glyph: '\u{E774}',
        app_id: None,
        dangerous: false,
        effect: ItemEffect::OpenWebUrl { url: fallback_url },
    }]
}

pub fn query_results(
    query: &str,
    cfg: &LauncherConfig,
    apps: &[AppEntry],
    frecency: &FrecencyStore,
    now_secs: u64,
    remote_actions: &[RemoteAction],
) -> Vec<SearchItem> {
    match detect_mode(query) {
        SearchMode::Actions => {
            let filter = query.trim_start().strip_prefix('>').unwrap_or("");
            query_actions(filter, remote_actions)
        }
        SearchMode::Bangs => query_bangs(query, cfg),
        SearchMode::Apps => {
            let q = query.trim();
            if q.is_empty() {
                if cfg.compact_mode {
                    return Vec::new();
                }
                let mut entries: Vec<(&AppEntry, f64)> = apps
                    .iter()
                    .map(|a| (a, frecency.score(&a.id, now_secs)))
                    .collect();
                entries.sort_by(|(a, sa), (b, sb)| {
                    let a_has = *sa > 0.0;
                    let b_has = *sb > 0.0;
                    b_has
                        .cmp(&a_has)
                        .then_with(|| sb.total_cmp(sa))
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                return entries
                    .into_iter()
                    .map(|(a, _)| app_entry_to_item(a))
                    .collect();
            }

            let mut matched: Vec<(&AppEntry, u32, f64)> = apps
                .iter()
                .filter_map(|a| {
                    let ms = match_score(&a.name, q);
                    (ms > 0).then(|| (a, ms, frecency.score(&a.id, now_secs)))
                })
                .collect();
            matched.sort_by(|(a, ma, fa), (b, mb, fb)| {
                mb.cmp(ma)
                    .then_with(|| fb.total_cmp(fa))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            matched
                .into_iter()
                .map(|(a, _, _)| app_entry_to_item(a))
                .collect()
        }
    }
}

fn app_entry_to_item(a: &AppEntry) -> SearchItem {
    let glyph = a
        .name
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('A');
    SearchItem {
        key: format!("app:{}", a.id),
        title: a.name.clone(),
        subtitle: None,
        glyph,
        app_id: Some(a.id.clone()),
        dangerous: false,
        effect: ItemEffect::LaunchApp { id: a.id.clone() },
    }
}

#[derive(Default)]
pub struct ConfirmationState {
    armed_key: Option<String>,
}

impl ConfirmationState {
    pub fn armed_key(&self) -> Option<&str> {
        self.armed_key.as_deref()
    }

    pub fn clear(&mut self) {
        self.armed_key = None;
    }

    /// Returns `true` if the item is ready to execute, or `false` if this first Enter armed confirmation.
    pub fn arm_or_confirm(&mut self, item: &SearchItem) -> bool {
        if !item.dangerous {
            self.armed_key = None;
            return true;
        }
        if self.armed_key.as_deref() == Some(&item.key) {
            self.armed_key = None;
            true
        } else {
            self.armed_key = Some(item.key.clone());
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_apps() -> Vec<AppEntry> {
        vec![
            AppEntry {
                name: "Calculator".to_string(),
                id: "calc".to_string(),
            },
            AppEntry {
                name: "Firefox".to_string(),
                id: "ff".to_string(),
            },
            AppEntry {
                name: "Notepad".to_string(),
                id: "np".to_string(),
            },
            AppEntry {
                name: "Visual Studio Code".to_string(),
                id: "vscode".to_string(),
            },
        ]
    }

    #[test]
    fn app_search_frecency_and_compact_mode() {
        let apps = sample_apps();
        let mut cfg = LauncherConfig::default();
        let mut frec = FrecencyStore::default();
        let now = 1_750_000_000u64;
        frec.entries.insert(
            "np".to_string(),
            crate::apps::FrecencyEntry {
                count: 5,
                last_used_secs: now - 30,
            },
        );

        let empty_non_compact = query_results("", &cfg, &apps, &frec, now, &[]);
        assert_eq!(empty_non_compact[0].title, "Notepad");
        assert_eq!(empty_non_compact[1].title, "Calculator");
        assert_eq!(empty_non_compact[2].title, "Firefox");

        cfg.compact_mode = true;
        assert!(query_results("", &cfg, &apps, &frec, now, &[]).is_empty());

        let filtered = query_results("code", &cfg, &apps, &frec, now, &[]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].title, "Visual Studio Code");
    }

    #[test]
    fn unified_bangs_default_direct_and_custom_shortcuts() {
        let mut cfg = LauncherConfig::default();
        let apps = sample_apps();
        let frec = FrecencyStore::default();

        // Bare "!" lists !b plus built-in bangs.
        let list_all = query_results("!", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            list_all[0].effect,
            ItemEffect::FillQuery {
                text: "!b ".to_string()
            }
        );
        assert!(list_all.iter().any(|i| i.effect
            == ItemEffect::FillQuery {
                text: "!yt ".to_string()
            }));

        // Partial "!y" matches YouTube first.
        let partial = query_results("!y", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            partial[0].effect,
            ItemEffect::FillQuery {
                text: "!yt ".to_string()
            }
        );

        // Direct !yt, !gh, !wiki without !b.
        let yt = query_results("!yt lofi beats", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            yt[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://www.youtube.com/results?search_query=lofi%20beats".to_string()
            }
        );

        let gh = query_results("!gh windows-rs", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            gh[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://github.com/search?q=windows-rs".to_string()
            }
        );

        let wiki = query_results("!wiki noren", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            wiki[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://en.wikipedia.org/w/index.php?search=noren".to_string()
            }
        );

        // !b with configured engines + direct URL detection.
        let google = query_results("!b rust direct2d", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            google[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://www.google.com/search?q=rust%20direct2d".to_string()
            }
        );

        let direct = query_results("!b github.com", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            direct[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://github.com".to_string()
            }
        );
        assert_eq!(
            direct[1].effect,
            ItemEffect::OpenWebUrl {
                url: "https://www.google.com/search?q=github.com".to_string()
            }
        );

        cfg.web_engine = "brave".to_string();
        let brave = query_results("!b noren", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            brave[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://search.brave.com/search?q=noren".to_string()
            }
        );

        // Custom user shortcuts (search template + direct bookmark + custom !b override).
        cfg.bang_shortcuts = [
            "b | Perplexity | https://www.perplexity.ai/search?q={query}",
            "r | Reddit | https://www.reddit.com/search/?q={query}",
            "mail | Gmail | https://mail.google.com",
        ]
        .join("\n");

        let custom_b = query_results("!b winui3", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            custom_b[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://www.perplexity.ai/search?q=winui3".to_string()
            }
        );

        let reddit = query_results("!r rust", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            reddit[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://www.reddit.com/search/?q=rust".to_string()
            }
        );

        let gmail = query_results("!mail", &cfg, &apps, &frec, 0, &[]);
        assert_eq!(
            gmail[0].effect,
            ItemEffect::OpenWebUrl {
                url: "https://mail.google.com".to_string()
            }
        );
    }

    #[test]
    fn actions_remote_wm_parsing_and_dangerous_confirmation() {
        let wm_payload = json!({
            "module": "wm",
            "actions": [
                {
                    "id": "overview",
                    "label": "Overview",
                    "description": "Toggle workspace overview",
                    "cap": "launcher.actions",
                    "cmd": "overview"
                },
                {
                    "id": "cheatsheet",
                    "label": "Keyboard Shortcuts",
                    "description": "Toggle WM keyboard shortcuts cheatsheet",
                    "cap": "launcher.actions",
                    "cmd": "cheatsheet"
                }
            ]
        });
        let remote = parse_remote_launcher_actions("wm", &wm_payload);
        assert_eq!(remote.len(), 2);

        let cfg = LauncherConfig::default();
        let apps = sample_apps();
        let frec = FrecencyStore::default();

        let over = query_results(">over", &cfg, &apps, &frec, 0, &remote);
        assert_eq!(
            over[0].effect,
            ItemEffect::RemoteNipCommand {
                peer: "wm".to_string(),
                cap: "launcher.actions".to_string(),
                cmd: "overview".to_string()
            }
        );

        let all_actions = query_results(">", &cfg, &apps, &frec, 0, &remote);
        let restart = all_actions
            .iter()
            .find(|i| i.key == "builtin:restart")
            .unwrap();
        let lock = all_actions
            .iter()
            .find(|i| i.key == "builtin:lock")
            .unwrap();

        let mut confirm = ConfirmationState::default();
        assert!(!confirm.arm_or_confirm(restart));
        assert_eq!(confirm.armed_key(), Some("builtin:restart"));
        assert!(confirm.arm_or_confirm(restart));
        assert_eq!(confirm.armed_key(), None);

        assert!(confirm.arm_or_confirm(lock));
    }
}
