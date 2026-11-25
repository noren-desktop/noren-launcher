use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use noren_sdk::protocol::{
    CAP_LAUNCHER_ACTIONS, CAP_SETTINGS_SCHEMA, CAP_TRAY_ACTIONS,
};
use noren_sdk::storage::install_manifest;
use noren_sdk::tray::{TrayConfig, TrayMenuItem, TrayModulePayload, TrayService};
use noren_sdk::{NipEvent, NipNode, NodeConfig, SdkError};
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::config::build_launcher_manifest;
use crate::search::{parse_remote_launcher_actions, RemoteAction};

pub const WM_APP_NIP_EVENT: u32 = WM_APP + 10;
pub const WM_APP_TRAY_ACTION: u32 = WM_APP + 11;
pub const WM_APP_QUIT: u32 = WM_APP + 12;

pub const TRAY_ACT_TOGGLE: usize = 1;
pub const TRAY_ACT_WALLPAPER: usize = 2;
pub const TRAY_ACT_RELOAD_APPS: usize = 3;
pub const TRAY_ACT_AUTOSTART: usize = 4;

pub fn build_launcher_tray_payload() -> TrayModulePayload {
    TrayModulePayload {
        module: "launcher".to_string(),
        title: "Launcher".to_string(),
        paused: false,
        items: vec![
            TrayMenuItem {
                id: "toggle".to_string(),
                label: "Open Launcher".to_string(),
                checked: false,
                enabled: true,
                separator_before: false,
                open_path: None,
            },
            TrayMenuItem {
                id: "wallpaper".to_string(),
                label: "Wallpaper Carousel".to_string(),
                checked: false,
                enabled: true,
                separator_before: false,
                open_path: None,
            },
            TrayMenuItem {
                id: "reload_apps".to_string(),
                label: "Reload applications".to_string(),
                checked: false,
                enabled: true,
                separator_before: false,
                open_path: None,
            },
            TrayMenuItem {
                id: "autostart".to_string(),
                label: "Run at login".to_string(),
                checked: crate::actions::autostart_enabled(),
                enabled: true,
                separator_before: false,
                open_path: None,
            },
        ],
    }
}

pub struct LauncherNipRuntime {
    node: Arc<NipNode>,
    tray: TrayService,
    queue: Arc<Mutex<VecDeque<NipEvent>>>,
    stop: Arc<AtomicBool>,
    forwarder: Option<JoinHandle<()>>,
}

impl LauncherNipRuntime {
    pub fn start(hwnd_raw: isize) -> Result<Self, SdkError> {
        let manifest = build_launcher_manifest();
        if let Err(e) = install_manifest(&manifest) {
            eprintln!("noren-launcher: failed to install manifest: {e}");
        }

        let tray_payload = build_launcher_tray_payload();
        let mut initial_state = HashMap::new();
        initial_state.insert(
            CAP_SETTINGS_SCHEMA.to_string(),
            serde_json::to_value(&manifest).unwrap_or(Value::Null),
        );
        initial_state.insert(
            CAP_TRAY_ACTIONS.to_string(),
            serde_json::to_value(&tray_payload).unwrap_or(Value::Null),
        );

        let (node, rx) = NipNode::start(NodeConfig {
            module: "launcher".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            provides: vec![
                CAP_SETTINGS_SCHEMA.to_string(),
                CAP_TRAY_ACTIONS.to_string(),
            ],
            wants: vec![
                CAP_LAUNCHER_ACTIONS.to_string(),
                CAP_TRAY_ACTIONS.to_string(),
            ],
            allowlist: None,
            initial_state,
        })?;
        let node = Arc::new(node);

        let tray = TrayService::start(TrayConfig {
            module: "launcher".to_string(),
            tooltip: "noren-launcher".to_string(),
            initial_payload: tray_payload,
            node: Some(Arc::clone(&node)),
            on_local_action: Arc::new(move |id| {
                let code = match id {
                    "toggle" => TRAY_ACT_TOGGLE,
                    "wallpaper" => TRAY_ACT_WALLPAPER,
                    "reload_apps" => TRAY_ACT_RELOAD_APPS,
                    "autostart" => TRAY_ACT_AUTOSTART,
                    _ => return,
                };
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(hwnd_raw as *mut c_void)),
                        WM_APP_TRAY_ACTION,
                        WPARAM(code),
                        LPARAM(0),
                    );
                }
            }),
            on_local_quit: Arc::new(move || unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd_raw as *mut c_void)),
                    WM_APP_QUIT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }),
            on_open_settings: Some(Arc::new(|tab| {
                let _ = noren_sdk::control_center::open_control_center(tab.or(Some("launcher")));
            })),
            headless: false,
        })?;

        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let q_bg = Arc::clone(&queue);
        let stop_bg = Arc::clone(&stop);

        let forwarder = thread::spawn(move || {
            while !stop_bg.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(120)) {
                    Ok(ev) => {
                        if let Ok(mut guard) = q_bg.lock() {
                            guard.push_back(ev);
                        }
                        unsafe {
                            let _ = PostMessageW(
                                Some(HWND(hwnd_raw as *mut c_void)),
                                WM_APP_NIP_EVENT,
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        Ok(Self {
            node,
            tray,
            queue,
            stop,
            forwarder: Some(forwarder),
        })
    }

    pub fn drain_events(&self) -> Vec<NipEvent> {
        self.queue
            .lock()
            .map(|mut g| g.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn sync_tray_state(&self) {
        self.tray.update_local_payload(build_launcher_tray_payload());
    }

    pub fn notify_tray_topology(&self) {
        self.tray.notify_topology_changed();
    }

    pub fn handle_tray_yield(&self, peer_module: &str) -> bool {
        let peer_elevated = self
            .node
            .peers()
            .into_iter()
            .find(|p| p.module == peer_module)
            .is_some_and(|p| p.elevated);
        self.tray.request_yield(peer_elevated)
    }

    pub fn remote_actions(&self) -> Vec<RemoteAction> {
        let mut out = Vec::new();
        for peer in self.node.providers_of(CAP_LAUNCHER_ACTIONS) {
            if let Some(val) = self.node.latest_state(&peer, CAP_LAUNCHER_ACTIONS) {
                out.extend(parse_remote_launcher_actions(&peer, &val));
            }
        }
        out
    }

    pub fn send_remote_action(&self, peer: &str, cap: &str, cmd: &str) {
        let node = Arc::clone(&self.node);
        let peer = peer.to_string();
        let cap = cap.to_string();
        let cmd = cmd.to_string();
        thread::spawn(move || {
            let _ = node.send_cmd(
                &peer,
                &cap,
                &cmd,
                Value::Null,
                Duration::from_millis(800),
            );
        });
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.tray.shutdown();
        self.node.shutdown();
        if let Some(handle) = self.forwarder.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LauncherNipRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    cond()
}

/// Automated Phase 3 NIP + Unified Tray integration self-test (`--nip-self-test`).
pub fn run_nip_self_test() -> Result<(), String> {
    struct EnvGuard {
        dir: std::path::PathBuf,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("NOREN_CONFIG_DIR");
            std::env::remove_var("NOREN_PIPE_PREFIX");
            std::env::remove_var("NOREN_TRAY_HEADLESS");
            std::env::remove_var("NOREN_SIMULATE_ELEVATED");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("noren_launcher_selftest_{pid}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create temp dir failed: {e}"))?;
    let _guard = EnvGuard { dir: dir.clone() };

    let prefix = format!("noren_launcher_test_{pid}.");
    std::env::set_var("NOREN_CONFIG_DIR", &dir);
    std::env::set_var("NOREN_PIPE_PREFIX", &prefix);
    std::env::set_var("NOREN_TRAY_HEADLESS", "1");

    let manifest = build_launcher_manifest();
    install_manifest(&manifest).map_err(|e| format!("install_manifest failed: {e}"))?;
    let loaded = noren_sdk::storage::load_installed_manifests();
    if !loaded.iter().any(|m| m.id == "launcher") {
        return Err("launcher manifest missing after install".into());
    }
    println!("LAUNCHER_NIP_TEST_MANIFEST=PASS");

    // Start simulated elevated WM node + tray.
    std::env::set_var("NOREN_SIMULATE_ELEVATED", "1");
    let mut wm_init = HashMap::new();
    wm_init.insert(
        CAP_LAUNCHER_ACTIONS.to_string(),
        json!({
            "module": "wm",
            "actions": [
                { "id": "overview", "label": "Overview", "description": "Toggle workspace overview", "cap": CAP_LAUNCHER_ACTIONS, "cmd": "overview" },
                { "id": "cheatsheet", "label": "Keyboard Shortcuts", "description": "Toggle WM cheatsheet", "cap": CAP_LAUNCHER_ACTIONS, "cmd": "cheatsheet" }
            ]
        }),
    );
    wm_init.insert(
        CAP_TRAY_ACTIONS.to_string(),
        json!({
            "module": "wm",
            "title": "Window Manager",
            "paused": false,
            "items": [{ "id": "pause", "label": "Pause", "checked": false, "enabled": true, "separator_before": false }]
        }),
    );
    let (wm_node, wm_rx) = NipNode::start(NodeConfig {
        module: "wm".to_string(),
        version: "0.1.0".to_string(),
        provides: vec![
            CAP_LAUNCHER_ACTIONS.to_string(),
            CAP_TRAY_ACTIONS.to_string(),
        ],
        wants: vec![CAP_TRAY_ACTIONS.to_string()],
        allowlist: Some(vec![
            "overview".to_string(),
            "cheatsheet".to_string(),
            "tray.yield".to_string(),
        ]),
        initial_state: wm_init,
    })
    .map_err(|e| format!("wm node start failed: {e}"))?;
    let wm_node = Arc::new(wm_node);
    std::env::remove_var("NOREN_SIMULATE_ELEVATED");

    let wm_tray = TrayService::start(TrayConfig {
        module: "wm".to_string(),
        tooltip: "noren-wm".to_string(),
        initial_payload: TrayModulePayload {
            module: "wm".to_string(),
            title: "Window Manager".to_string(),
            paused: false,
            items: vec![TrayMenuItem {
                id: "pause".to_string(),
                label: "Pause".to_string(),
                checked: false,
                enabled: true,
                separator_before: false,
                open_path: None,
            }],
        },
        node: Some(Arc::clone(&wm_node)),
        on_local_action: Arc::new(|_| {}),
        on_local_quit: Arc::new(|| {}),
        on_open_settings: None,
        headless: true,
    })
    .map_err(|e| format!("wm_tray start failed: {e}"))?;

    // Start standard-level launcher node + tray.
    let mut launcher_init = HashMap::new();
    let launcher_payload = build_launcher_tray_payload();
    launcher_init.insert(
        CAP_SETTINGS_SCHEMA.to_string(),
        serde_json::to_value(&manifest).unwrap(),
    );
    launcher_init.insert(
        CAP_TRAY_ACTIONS.to_string(),
        serde_json::to_value(&launcher_payload).unwrap(),
    );
    let (launcher_node, launcher_rx) = NipNode::start(NodeConfig {
        module: "launcher".to_string(),
        version: "0.1.0".to_string(),
        provides: vec![
            CAP_SETTINGS_SCHEMA.to_string(),
            CAP_TRAY_ACTIONS.to_string(),
        ],
        wants: vec![
            CAP_LAUNCHER_ACTIONS.to_string(),
            CAP_TRAY_ACTIONS.to_string(),
        ],
        allowlist: None,
        initial_state: launcher_init,
    })
    .map_err(|e| format!("launcher node start failed: {e}"))?;
    let launcher_node = Arc::new(launcher_node);

    let launcher_tray = TrayService::start(TrayConfig {
        module: "launcher".to_string(),
        tooltip: "noren-launcher".to_string(),
        initial_payload: launcher_payload,
        node: Some(Arc::clone(&launcher_node)),
        on_local_action: Arc::new(|_| {}),
        on_local_quit: Arc::new(|| {}),
        on_open_settings: None,
        headless: true,
    })
    .map_err(|e| format!("launcher_tray start failed: {e}"))?;

    let overview_called = Arc::new(AtomicBool::new(false));
    let overview_called_bg = Arc::clone(&overview_called);
    let wm_stop = Arc::new(AtomicBool::new(false));
    let wm_stop_bg = Arc::clone(&wm_stop);
    let wm_node_bg = Arc::clone(&wm_node);
    let wm_tray_bg = Arc::new(wm_tray);
    let wm_tray_thread = Arc::clone(&wm_tray_bg);
    let wm_loop = thread::spawn(move || {
        while !wm_stop_bg.load(Ordering::Acquire) {
            match wm_rx.recv_timeout(Duration::from_millis(40)) {
                Ok(NipEvent::PeerConnected(_) | NipEvent::PeerDisconnected { .. }) => {
                    wm_tray_thread.notify_topology_changed();
                }
                Ok(NipEvent::Command {
                    from, name, reply, ..
                }) => match name.as_str() {
                    "overview" => {
                        overview_called_bg.store(true, Ordering::Release);
                        let _ = reply.ok(json!({ "overview": true }));
                    }
                    "tray.yield" => {
                        let peer_elev = wm_node_bg
                            .peers()
                            .into_iter()
                            .find(|p| p.module == from)
                            .is_some_and(|p| p.elevated);
                        let yielded = wm_tray_thread.request_yield(peer_elev);
                        let _ = reply.ok(json!({ "yielded": yielded }));
                    }
                    other => {
                        let _ = reply.err(format!("unexpected cmd {other}"));
                    }
                },
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    let connected_and_actions = wait_until(Duration::from_secs(5), || {
        while let Ok(ev) = launcher_rx.try_recv() {
            if matches!(
                ev,
                NipEvent::PeerConnected(_)
                    | NipEvent::StateReceived { .. }
                    | NipEvent::EventReceived { .. }
            ) {
                launcher_tray.notify_topology_changed();
            }
        }
        launcher_node.is_peer_connected("wm")
            && launcher_node
                .latest_state("wm", CAP_LAUNCHER_ACTIONS)
                .is_some()
    });
    if !connected_and_actions {
        wm_stop.store(true, Ordering::Release);
        let _ = wm_loop.join();
        return Err("launcher did not receive wm launcher.actions".into());
    }

    let wm_snap = launcher_node
        .latest_state("wm", CAP_LAUNCHER_ACTIONS)
        .unwrap();
    let remote = parse_remote_launcher_actions("wm", &wm_snap);
    let results = crate::search::query_results(
        ">over",
        &crate::config::LauncherConfig::default(),
        &[],
        &crate::apps::FrecencyStore::default(),
        0,
        &remote,
    );
    if results.first().map(|i| i.title.as_str()) != Some("Overview") {
        wm_stop.store(true, Ordering::Release);
        let _ = wm_loop.join();
        return Err(format!("expected >over -> Overview, got {results:?}"));
    }

    launcher_node
        .send_cmd(
            "wm",
            CAP_LAUNCHER_ACTIONS,
            "overview",
            Value::Null,
            Duration::from_secs(3),
        )
        .map_err(|e| format!("overview cmd failed: {e}"))?;
    if !overview_called.load(Ordering::Acquire) {
        wm_stop.store(true, Ordering::Release);
        let _ = wm_loop.join();
        return Err("wm overview handler was not invoked".into());
    }
    println!("LAUNCHER_NIP_TEST_WM_ACTIONS=PASS");

    let tray_ok = wait_until(Duration::from_secs(5), || {
        while let Ok(ev) = launcher_rx.try_recv() {
            if matches!(
                ev,
                NipEvent::PeerConnected(_)
                    | NipEvent::StateReceived { .. }
                    | NipEvent::EventReceived { .. }
            ) {
                launcher_tray.notify_topology_changed();
            }
        }
        let snap = launcher_tray.menu_snapshot();
        launcher_tray.is_host()
            && !wm_tray_bg.is_host()
            && !snap.standalone_mode
            && snap.modules.len() == 2
            && snap.quit_all_label.as_deref() == Some("Quit All")
    });
    if !tray_ok {
        wm_stop.store(true, Ordering::Release);
        let _ = wm_loop.join();
        return Err("unified tray handoff or snapshot failed".into());
    }
    println!("LAUNCHER_NIP_TEST_UNIFIED_TRAY=PASS");

    // Disconnect WM -> remote actions disappear from launcher's `>` list.
    wm_stop.store(true, Ordering::Release);
    wm_tray_bg.shutdown();
    wm_node.shutdown();
    let _ = wm_loop.join();

    let wm_gone = wait_until(Duration::from_secs(4), || {
        while let Ok(ev) = launcher_rx.try_recv() {
            if matches!(ev, NipEvent::PeerDisconnected { .. }) {
                launcher_tray.notify_topology_changed();
            }
        }
        !launcher_node.is_peer_connected("wm")
            && launcher_node.providers_of(CAP_LAUNCHER_ACTIONS).is_empty()
    });
    if !wm_gone {
        return Err("launcher did not drop wm capabilities after disconnect".into());
    }
    println!("LAUNCHER_NIP_TEST_WM_DISCONNECT_FALLBACK=PASS");

    launcher_tray.shutdown();
    launcher_node.shutdown();
    println!("LAUNCHER_NIP_SELF_TEST=ALL_PASS");
    Ok(())
}
