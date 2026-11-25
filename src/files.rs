use std::collections::{HashSet, VecDeque};
use std::ffi::c_void;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, MAX_PATH, WPARAM};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};
use windows::Win32::System::Search::{IQueryParser, IQueryParserManager, QueryParserManager};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, ISearchFolderItemFactory, IShellItem, SearchFolderItemFactory,
    ShellExecuteW, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SW_HIDE, SW_SHOWMINNOACTIVE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DRIVE_FIXED: u32 = 3;
const MAX_FILE_RESULTS: usize = 28;
const MAX_FILE_RESULTS_U32: u32 = 28;
const INDEX_MAX_DEPTH: usize = 14;
const INDEX_MAX_ITEMS: usize = 300_000;
const INDEX_TTL_SECS: u64 = 300;
const EVERYTHING_REQUEST_FILE_NAME: u32 = 0x0000_0001;
const EVERYTHING_REQUEST_PATH: u32 = 0x0000_0002;
const EVERYTHING_PORTABLE_ZIP_URL: &str = "https://www.voidtools.com/Everything-1.4.1.1032.x64.zip";
const EVERYTHING_SDK_ZIP_URL: &str = "https://www.voidtools.com/Everything-SDK.zip";

static EVERYTHING_DLL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub parent_dir: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EverythingState {
    Ready,
    Starting,
    Installing,
    NotInstalled,
    SdkMissing(PathBuf),
    InstallFailed(String),
}

#[derive(Clone, Debug)]
pub struct FileSearchSnapshot {
    pub query: String,
    pub backend: String,
    pub loading: bool,
    pub everything_state: EverythingState,
    pub entries: Vec<FileEntry>,
}

impl Default for FileSearchSnapshot {
    fn default() -> Self {
        Self {
            query: String::new(),
            backend: "windows".to_string(),
            loading: false,
            everything_state: EverythingState::NotInstalled,
            entries: Vec::new(),
        }
    }
}

struct ComGuard(bool);

impl ComGuard {
    fn init_sta() -> Self {
        let ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        Self(ok)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn take_pwstr(pwstr: PWSTR) -> String {
    if pwstr.0.is_null() {
        return String::new();
    }
    let s = String::from_utf16_lossy(pwstr.as_wide()).trim().to_string();
    CoTaskMemFree(Some(pwstr.0 as _));
    s
}

fn noren_everything_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local)
                .join("Noren")
                .join("tools")
                .join("everything");
        }
    }
    std::env::temp_dir().join("noren-everything")
}

fn entry_from_path(full: PathBuf) -> Option<FileEntry> {
    let full_str = full.to_string_lossy().into_owned();
    if full_str.is_empty() {
        return None;
    }
    let meta = fs::symlink_metadata(&full).ok()?;
    let is_dir = meta.is_dir();
    let name = full
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| full_str.clone());
    let parent_dir = full
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(FileEntry {
        name,
        path: full_str,
        parent_dir,
        is_dir,
    })
}

/// Checks whether voidtools Everything IPC window is currently active.
pub fn is_everything_running() -> bool {
    unsafe {
        let h1 = FindWindowW(w!("EVERYTHING_TASKBAR_NOTIFICATION"), None);
        if let Ok(h) = h1 {
            if !h.0.is_null() {
                return true;
            }
        }
        let h2 = FindWindowW(w!("EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)"), None);
        if let Ok(h) = h2 {
            if !h.0.is_null() {
                return true;
            }
        }
        false
    }
}

fn read_registry_string(root: windows::Win32::System::Registry::HKEY, subkey: &str, name: &str) -> Option<String> {
    let subkey_w = to_wide_null(subkey);
    let name_w = to_wide_null(name);
    let mut size: u32 = 0;
    let res = unsafe {
        RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if res.is_err() || size < 2 || size > 4096 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize / 2) + 1];
    let res2 = unsafe {
        RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if res2.is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn search_exe_on_path(file_names: &[&str]) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        for name in file_names {
            let cand = dir.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Auto-detects an existing `Everything.exe` / `Everything64.exe` on the system
/// (custom path, Noren portable folder, standard install dirs, Scoop/Chocolatey, Registry, PATH).
pub fn detect_everything_exe(custom_path: &str) -> Option<PathBuf> {
    let trimmed = custom_path.trim().trim_matches('"');
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_file() {
            return Some(p);
        }
        let in_dir = p.join("Everything.exe");
        if in_dir.is_file() {
            return Some(in_dir);
        }
    }

    let mut candidates = Vec::new();
    let noren_dir = noren_everything_dir();
    candidates.push(noren_dir.join("Everything.exe"));
    candidates.push(noren_dir.join("Everything64.exe"));

    if let Some(pf) = std::env::var_os("ProgramFiles") {
        let pf = PathBuf::from(pf);
        candidates.push(pf.join("Everything").join("Everything.exe"));
        candidates.push(pf.join("Everything").join("Everything64.exe"));
        candidates.push(pf.join("Everything 1.5a").join("Everything64.exe"));
        candidates.push(pf.join("Everything 1.5a").join("Everything.exe"));
    }
    if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
        let pf86 = PathBuf::from(pf86);
        candidates.push(pf86.join("Everything").join("Everything.exe"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        candidates.push(local.join("Programs").join("Everything").join("Everything.exe"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let profile = PathBuf::from(profile);
        candidates.push(profile.join("scoop").join("apps").join("everything").join("current").join("Everything.exe"));
        candidates.push(profile.join("scoop").join("apps").join("everything-alpha").join("current").join("Everything64.exe"));
    }
    candidates.push(PathBuf::from(r"C:\ProgramData\chocolatey\lib\Everything\tools\Everything.exe"));

    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for subkey in [
            r"SOFTWARE\voidtools\Everything",
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Everything",
        ] {
            for val_name in ["InstallLocation", "ExePath"] {
                if let Some(raw) = read_registry_string(root, subkey, val_name) {
                    let clean = raw.trim().trim_matches('"');
                    let p = PathBuf::from(clean);
                    if p.is_file() {
                        candidates.push(p);
                    } else {
                        candidates.push(p.join("Everything.exe"));
                        candidates.push(p.join("Everything64.exe"));
                    }
                }
            }
        }
    }

    for cand in candidates {
        if cand.is_file() {
            return Some(cand);
        }
    }

    search_exe_on_path(&["Everything64.exe", "Everything.exe"])
}

fn detect_everything_dll(exe_hint: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let noren_dir = noren_everything_dir();
    candidates.push(noren_dir.join("Everything64.dll"));
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            candidates.push(dir.join("Everything64.dll"));
        }
    }
    if let Some(exe) = exe_hint {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Everything64.dll"));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn detect_es_cli(exe_hint: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let noren_dir = noren_everything_dir();
    candidates.push(noren_dir.join("es.exe"));
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            candidates.push(dir.join("es.exe"));
        }
    }
    if let Some(exe) = exe_hint {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("es.exe"));
        }
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    search_exe_on_path(&["es.exe"])
}

/// Launches Everything silently in the background (`-startup`).
pub fn start_everything_background(exe: &Path) -> bool {
    let exe_str = exe.to_string_lossy();
    let dir_str = exe
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let exe_w = to_wide_null(&exe_str);
    let dir_w = to_wide_null(&dir_str);
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(exe_w.as_ptr()),
            w!("-startup"),
            PCWSTR(dir_w.as_ptr()),
            SW_SHOWMINNOACTIVE,
        )
    };
    (res.0 as isize) > 32
}

fn ps_escape_single(s: &Path) -> String {
    s.to_string_lossy().replace('\'', "''")
}

fn download_and_extract_everything_sdk(dest_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dest_dir).map_err(|e| format!("create dir failed: {e}"))?;
    let zip_path = dest_dir.join("Everything-SDK.zip");
    let extract_dir = dest_dir.join("_sdk_tmp");
    let script = format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
         Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile '{zip}'; \
         if (Test-Path '{ext}') {{ Remove-Item -Recurse -Force '{ext}' }}; \
         Expand-Archive -Force -Path '{zip}' -DestinationPath '{ext}'; \
         $dll = Get-ChildItem -Path '{ext}' -Filter 'Everything64.dll' -Recurse | Select-Object -First 1; \
         if ($null -eq $dll) {{ throw 'Everything64.dll missing in archive' }}; \
         Copy-Item -Force $dll.FullName '{dst}'",
        url = EVERYTHING_SDK_ZIP_URL,
        zip = ps_escape_single(&zip_path),
        ext = ps_escape_single(&extract_dir),
        dst = ps_escape_single(&dest_dir.join("Everything64.dll")),
    );

    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("powershell failed: {e}"))?;

    let _ = fs::remove_file(&zip_path);
    let _ = fs::remove_dir_all(&extract_dir);

    let dll_path = dest_dir.join("Everything64.dll");
    if out.status.success() && dll_path.is_file() {
        Ok(dll_path)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "Failed to download Everything-SDK.zip".to_string()
        } else {
            stderr
        })
    }
}

fn download_and_install_portable_everything(dest_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dest_dir).map_err(|e| format!("create dir failed: {e}"))?;
    let zip_path = dest_dir.join("Everything-portable.zip");
    let script = format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; \
         Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile '{zip}'; \
         Expand-Archive -Force -Path '{zip}' -DestinationPath '{dst}'",
        url = EVERYTHING_PORTABLE_ZIP_URL,
        zip = ps_escape_single(&zip_path),
        dst = ps_escape_single(dest_dir),
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("powershell failed: {e}"))?;
    let _ = fs::remove_file(&zip_path);

    let exe = dest_dir.join("Everything.exe");
    if !out.status.success() || !exe.is_file() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "Failed to download portable Everything archive".to_string()
        } else {
            stderr
        });
    }

    let ini = dest_dir.join("Everything.ini");
    if !ini.exists() {
        let _ = fs::write(
            &ini,
            "[Everything]\r\nrun_in_background=1\r\napp_data=1\r\nauto_include_fixed_volumes=1\r\nauto_include_fixed_fat_volumes=1\r\nauto_include_fixed_refs_volumes=1\r\n",
        );
    }

    let exe_w = to_wide_null(&exe.to_string_lossy());
    let dir_w = to_wide_null(&dest_dir.to_string_lossy());
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(exe_w.as_ptr()),
            w!("-install-service"),
            PCWSTR(dir_w.as_ptr()),
            SW_HIDE,
        );
    }
    Ok(exe)
}

fn try_winget_install_everything() -> bool {
    let Ok(out) = Command::new("winget")
        .args([
            "install",
            "-e",
            "--id",
            "voidtools.Everything",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    out.status.success()
}

struct EverythingDll {
    _module: windows::Win32::Foundation::HMODULE,
    set_search_w: unsafe extern "system" fn(PCWSTR),
    set_max: unsafe extern "system" fn(u32),
    set_request_flags: unsafe extern "system" fn(u32),
    query_w: unsafe extern "system" fn(BOOL) -> BOOL,
    get_num_results: unsafe extern "system" fn() -> u32,
    is_folder_result: unsafe extern "system" fn(u32) -> BOOL,
    get_result_full_path_name_w: unsafe extern "system" fn(u32, PWSTR, u32) -> u32,
    clean_up: Option<unsafe extern "system" fn()>,
}

impl EverythingDll {
    unsafe fn load(dll_path: &Path) -> Result<Self, String> {
        let wide = to_wide_null(&dll_path.to_string_lossy());
        let module = LoadLibraryW(PCWSTR(wide.as_ptr()))
            .map_err(|e| format!("LoadLibraryW failed for {}: {e}", dll_path.display()))?;

        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let ptr = GetProcAddress(module, windows::core::PCSTR(concat!($name, "\0").as_ptr()))
                    .ok_or_else(|| format!("Missing Everything SDK symbol {}", $name))?;
                std::mem::transmute::<_, $ty>(ptr)
            }};
        }

        let clean_up = GetProcAddress(module, windows::core::s!("Everything_CleanUp"))
            .map(|p| std::mem::transmute::<_, unsafe extern "system" fn()>(p));

        Ok(Self {
            _module: module,
            set_search_w: sym!("Everything_SetSearchW", unsafe extern "system" fn(PCWSTR)),
            set_max: sym!("Everything_SetMax", unsafe extern "system" fn(u32)),
            set_request_flags: sym!("Everything_SetRequestFlags", unsafe extern "system" fn(u32)),
            query_w: sym!("Everything_QueryW", unsafe extern "system" fn(BOOL) -> BOOL),
            get_num_results: sym!("Everything_GetNumResults", unsafe extern "system" fn() -> u32),
            is_folder_result: sym!(
                "Everything_IsFolderResult",
                unsafe extern "system" fn(u32) -> BOOL
            ),
            get_result_full_path_name_w: sym!(
                "Everything_GetResultFullPathNameW",
                unsafe extern "system" fn(u32, PWSTR, u32) -> u32
            ),
            clean_up,
        })
    }

    fn query(&self, term: &str, max_results: u32) -> Option<Vec<FileEntry>> {
        let _guard = EVERYTHING_DLL_LOCK.lock().ok()?;
        let wide = to_wide_null(term);
        unsafe {
            (self.set_search_w)(PCWSTR(wide.as_ptr()));
            (self.set_max)(max_results);
            (self.set_request_flags)(EVERYTHING_REQUEST_FILE_NAME | EVERYTHING_REQUEST_PATH);
            if !(self.query_w)(BOOL(1)).as_bool() {
                if let Some(cleanup) = self.clean_up {
                    cleanup();
                }
                return None;
            }
            let count = (self.get_num_results)().min(max_results);
            let mut out = Vec::with_capacity(count as usize);
            let mut buf = vec![0u16; (MAX_PATH as usize) * 4];
            for idx in 0..count {
                let len = (self.get_result_full_path_name_w)(
                    idx,
                    PWSTR(buf.as_mut_ptr()),
                    buf.len() as u32,
                ) as usize;
                if len == 0 {
                    continue;
                }
                let actual_len = len.min(buf.len());
                let full_str = String::from_utf16_lossy(&buf[..actual_len])
                    .trim_end_matches('\0')
                    .to_string();
                if full_str.is_empty() {
                    continue;
                }
                let is_dir = (self.is_folder_result)(idx).as_bool();
                let p = PathBuf::from(&full_str);
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| full_str.clone());
                let parent_dir = p
                    .parent()
                    .map(|pp| pp.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push(FileEntry {
                    name,
                    path: full_str,
                    parent_dir,
                    is_dir,
                });
            }
            if let Some(cleanup) = self.clean_up {
                cleanup();
            }
            Some(out)
        }
    }
}

fn query_everything_via_es(es_exe: &Path, term: &str, max_results: u32) -> Option<Vec<FileEntry>> {
    let max_str = max_results.to_string();
    let out = Command::new(es_exe)
        .args(["-n", &max_str, term])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut entries = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(entry) = entry_from_path(PathBuf::from(trimmed)) {
            entries.push(entry);
        }
    }
    Some(entries)
}

/// Parses direct filesystem paths (`C:\...`, `D:/...`, `~\...`).
fn looks_like_fs_path(term: &str) -> bool {
    let t = term.trim();
    if t.starts_with("\\\\") || t.starts_with("~/") || t.starts_with("~\\") {
        return true;
    }
    let bytes = t.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn query_direct_fs_path(term: &str, max_results: usize) -> Vec<FileEntry> {
    let trimmed = term.trim();
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            PathBuf::from(home).join(rest).to_string_lossy().into_owned()
        } else {
            trimmed.to_string()
        }
    } else {
        trimmed.to_string()
    };

    let normalized = expanded.replace('/', "\\");
    let raw_path = PathBuf::from(&normalized);

    let (dir_to_read, name_prefix) = if normalized.ends_with('\\') || raw_path.is_dir() {
        (raw_path, String::new())
    } else {
        let parent = raw_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("\\"));
        let pref = raw_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        (parent, pref)
    };

    let Ok(rd) = fs::read_dir(&dir_to_read) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in rd.flatten() {
        let p = item.path();
        let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name_prefix.is_empty() && !fname.to_lowercase().contains(&name_prefix) {
            continue;
        }
        if let Some(entry) = entry_from_path(p) {
            entries.push(entry);
            if entries.len() >= max_results * 2 {
                break;
            }
        }
    }
    entries.sort_by(|a, b| {
        let a_starts = !name_prefix.is_empty() && a.name.to_lowercase().starts_with(&name_prefix);
        let b_starts = !name_prefix.is_empty() && b.name.to_lowercase().starts_with(&name_prefix);
        b_starts
            .cmp(&a_starts)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries.truncate(max_results);
    entries
}

/// Builds a safe Advanced Query Syntax (AQS) string for Windows Search `SystemIndex`.
pub fn build_windows_search_aqs(term: &str) -> String {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Preserve explicit AQS operators (e.g. `ext:pdf`, `kind:music`).
    if trimmed.contains(':') {
        return trimmed.to_string();
    }
    trimmed
        .split_whitespace()
        .map(|tok| {
            let escaped = tok.replace('"', "");
            format!("System.FileName:~\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn query_windows_search_com(
    term: &str,
    seq: u64,
    latest_seq: &AtomicU64,
    max_results: usize,
) -> Result<Vec<FileEntry>, String> {
    let aqs = build_windows_search_aqs(term);
    if aqs.is_empty() {
        return Ok(Vec::new());
    }

    let _com = ComGuard::init_sta();
    unsafe {
        let mgr: IQueryParserManager = CoCreateInstance(&QueryParserManager, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("QueryParserManager failed: {e}"))?;
        let parser: IQueryParser = mgr
            .CreateLoadedParser(w!("SystemIndex"), 0x0409)
            .map_err(|e| format!("CreateLoadedParser failed: {e}"))?;
        mgr.InitializeOptions(true, true, &parser)
            .map_err(|e| format!("InitializeOptions failed: {e}"))?;

        let aqs_w = to_wide_null(&aqs);
        let solution = parser
            .Parse(PCWSTR(aqs_w.as_ptr()), None)
            .map_err(|e| format!("Parse AQS failed: {e}"))?;
        let mut cond = None;
        solution
            .GetQuery(Some(&mut cond), None)
            .map_err(|e| format!("GetQuery failed: {e}"))?;
        let Some(condition) = cond else {
            return Ok(Vec::new());
        };

        let factory: ISearchFolderItemFactory =
            CoCreateInstance(&SearchFolderItemFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("SearchFolderItemFactory failed: {e}"))?;
        factory
            .SetDisplayName(w!("Noren File Search"))
            .map_err(|e| format!("SetDisplayName failed: {e}"))?;
        factory
            .SetCondition(&condition)
            .map_err(|e| format!("SetCondition failed: {e}"))?;

        let folder: IShellItem = factory
            .GetShellItem()
            .map_err(|e| format!("GetShellItem failed: {e}"))?;
        let enum_items: IEnumShellItems = folder
            .BindToHandler(None, &BHID_EnumItems)
            .map_err(|e| format!("BindToHandler failed: {e}"))?;

        let mut results = Vec::with_capacity(max_results);
        let mut seen = HashSet::new();
        let mut batch: [Option<IShellItem>; 1] = [None];

        loop {
            if latest_seq.load(Ordering::Acquire) != seq {
                break;
            }
            let mut fetched = 0u32;
            let hr = enum_items.Next(&mut batch, Some(&mut fetched));
            if hr.is_err() || fetched == 0 {
                break;
            }
            let Some(item) = batch[0].take() else {
                break;
            };
            let path_str = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .ok()
                .map(|p| take_pwstr(p))
                .unwrap_or_default();
            if path_str.is_empty() || !seen.insert(path_str.to_lowercase()) {
                continue;
            }
            if let Some(entry) = entry_from_path(PathBuf::from(&path_str)) {
                results.push(entry);
                if results.len() >= max_results {
                    break;
                }
            }
        }

        // Prioritize prefix matches on filename first, then folders, then A-Z.
        let q_lower = term.trim().to_lowercase();
        results.sort_by(|a, b| {
            let a_exact = a.name.to_lowercase() == q_lower;
            let b_exact = b.name.to_lowercase() == q_lower;
            let a_pref = a.name.to_lowercase().starts_with(&q_lower);
            let b_pref = b.name.to_lowercase().starts_with(&q_lower);
            b_exact
                .cmp(&a_exact)
                .then_with(|| b_pref.cmp(&a_pref))
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(results)
    }
}

/// Fast bounded fallback scan of common user folders if the WSearch service is unavailable.
fn query_shallow_user_folders(
    term: &str,
    seq: u64,
    latest_seq: &AtomicU64,
    max_results: usize,
) -> Vec<FileEntry> {
    let tokens: Vec<String> = term
        .trim()
        .to_lowercase()
        .split_whitespace()
        .map(ToString::to_string)
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let Some(profile) = std::env::var_os("USERPROFILE") else {
        return Vec::new();
    };
    let root = PathBuf::from(profile);
    let mut stack = vec![
        (root.join("Desktop"), 0usize),
        (root.join("Downloads"), 0usize),
        (root.join("Documents"), 0usize),
        (root.join("Pictures"), 0usize),
    ];

    let mut out = Vec::new();
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if latest_seq.load(Ordering::Acquire) != seq || out.len() >= max_results || visited > 2500 {
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for item in rd.flatten() {
            visited += 1;
            if latest_seq.load(Ordering::Acquire) != seq || visited > 2500 {
                break;
            }
            let p = item.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let name_lower = name.to_lowercase();
            if tokens.iter().all(|t| name_lower.contains(t)) {
                if let Some(entry) = entry_from_path(p.clone()) {
                    out.push(entry);
                    if out.len() >= max_results {
                        break;
                    }
                }
            }
            if depth < 2 && p.is_dir() && !name.eq_ignore_ascii_case("node_modules") {
                stack.push((p, depth + 1));
            }
        }
    }
    out
}

fn os_drive_letter() -> char {
    std::env::var("SystemRoot")
        .ok()
        .and_then(|s| s.chars().next())
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('C')
}

fn should_skip_os_root_folder(name_lower: &str) -> bool {
    name_lower.starts_with('$')
        || matches!(
            name_lower,
            "windows"
                | "program files"
                | "program files (x86)"
                | "programdata"
                | "users"
                | "perflogs"
                | "recovery"
                | "system volume information"
                | "documents and settings"
                | "config.msi"
                | "msocache"
                | "intel"
                | "amd"
                | "nvidia"
                | "onegettemp"
        )
}

fn should_skip_dir_name(name: &str) -> bool {
    if name.starts_with('$') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "system volume information"
            | "recovery"
            | "appdata"
            | "node_modules"
            | "target"
            | ".git"
            | ".hg"
            | ".svn"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".cargo"
            | ".rustup"
            | ".gradle"
            | ".next"
            | ".nuxt"
            | ".cache"
            | ".vs"
            | ".idea"
    )
}

fn parse_scope_entries(custom_scope: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in custom_scope.split([',', ';']) {
        let t = raw.trim().trim_matches('"');
        if t.is_empty() {
            continue;
        }
        let expanded = if t.len() == 1 && t.as_bytes()[0].is_ascii_alphabetic() {
            format!("{}:\\", t.to_ascii_uppercase())
        } else if t.len() == 2 && t.as_bytes()[0].is_ascii_alphabetic() && t.as_bytes()[1] == b':'
        {
            format!("{}:\\", (t.as_bytes()[0] as char).to_ascii_uppercase())
        } else {
            t.replace('/', "\\")
        };
        let pb = PathBuf::from(&expanded);
        let key = pb.to_string_lossy().to_lowercase();
        if pb.is_dir() && seen.insert(key) {
            out.push(pb);
        }
    }
    out
}

/// Discovers search roots across all fixed partitions (`C:`, `D:`, `E:`, …),
/// or parses a user-specified comma/semicolon-separated scope list.
pub fn enumerate_search_roots(custom_scope: &str) -> Vec<PathBuf> {
    let custom = custom_scope.trim();
    if !custom.is_empty() {
        return parse_scope_entries(custom);
    }

    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    let mut push_root = |p: PathBuf| {
        let key = p.to_string_lossy().to_lowercase();
        if p.is_dir() && seen.insert(key) {
            roots.push(p);
        }
    };

    let os_letter = os_drive_letter();
    let mask = unsafe { GetLogicalDrives() };
    for bit in 0..26u32 {
        if (mask & (1u32 << bit)) == 0 {
            continue;
        }
        let letter = (b'A' + bit as u8) as char;
        let drive_str = format!("{letter}:\\");
        let drive_w = to_wide_null(&drive_str);
        let dtype = unsafe { GetDriveTypeW(PCWSTR(drive_w.as_ptr())) };
        if dtype != DRIVE_FIXED {
            continue;
        }

        if letter == os_letter {
            if let Some(profile) = std::env::var_os("USERPROFILE") {
                push_root(PathBuf::from(profile));
            }
            if let Ok(rd) = fs::read_dir(&drive_str) {
                for entry in rd.flatten() {
                    if let Ok(ft) = entry.file_type() {
                        if ft.is_dir() && !ft.is_symlink() {
                            if let Some(name) = entry.file_name().to_str() {
                                if !should_skip_os_root_folder(&name.to_ascii_lowercase()) {
                                    push_root(entry.path());
                                }
                            }
                        }
                    }
                }
            }
        } else {
            push_root(PathBuf::from(drive_str));
        }
    }

    drop(push_root);
    if roots.is_empty() {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let pb = PathBuf::from(profile);
            if pb.is_dir() {
                roots.push(pb);
            }
        }
    }
    roots
}

fn contains_ascii_or_unicode_ci(haystack: &str, needle_lower: &str) -> bool {
    if haystack.is_ascii() && needle_lower.is_ascii() {
        let h = haystack.as_bytes();
        let n = needle_lower.as_bytes();
        if n.is_empty() {
            return true;
        }
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle_lower)
}

struct IndexedItem {
    dir_idx: u32,
    name: Box<str>,
    is_dir: bool,
}

struct CompactFileIndex {
    scope_key: String,
    built_at: Instant,
    dirs: Vec<String>,
    items: Vec<IndexedItem>,
}

impl CompactFileIndex {
    fn build_bfs(scope_key: String, roots: &[PathBuf], cancel: &AtomicBool) -> Self {
        let mut dirs: Vec<String> = Vec::with_capacity(2048);
        let mut items: Vec<IndexedItem> = Vec::with_capacity(16384);
        let mut queue: VecDeque<(PathBuf, usize)> = roots.iter().cloned().map(|r| (r, 0)).collect();
        let mut visited_dirs = HashSet::new();

        while let Some((dir_path, depth)) = queue.pop_front() {
            if cancel.load(Ordering::Acquire) || items.len() >= INDEX_MAX_ITEMS {
                break;
            }
            let dir_str = dir_path.to_string_lossy().into_owned();
            if !visited_dirs.insert(dir_str.to_lowercase()) {
                continue;
            }
            let Ok(rd) = fs::read_dir(&dir_path) else {
                continue;
            };
            let Ok(dir_idx) = u32::try_from(dirs.len()) else {
                break;
            };
            dirs.push(dir_str);

            for entry in rd.flatten() {
                if items.len() >= INDEX_MAX_ITEMS {
                    break;
                }
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                if ft.is_symlink() {
                    continue;
                }
                let fname_os = entry.file_name();
                let Some(fname) = fname_os.to_str() else {
                    continue;
                };
                if fname.is_empty() {
                    continue;
                }
                let is_dir = ft.is_dir();
                if is_dir && should_skip_dir_name(fname) {
                    continue;
                }
                items.push(IndexedItem {
                    dir_idx,
                    name: fname.into(),
                    is_dir,
                });
                if is_dir && depth < INDEX_MAX_DEPTH {
                    queue.push_back((entry.path(), depth + 1));
                }
            }
        }

        Self {
            scope_key,
            built_at: Instant::now(),
            dirs,
            items,
        }
    }

    fn search(&self, term: &str, max_results: usize) -> Vec<FileEntry> {
        let tokens: Vec<String> = term
            .trim()
            .to_lowercase()
            .split_whitespace()
            .map(ToString::to_string)
            .collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        let q_lower = term.trim().to_lowercase();
        let mut hits: Vec<(u32, usize)> = Vec::new();

        for (idx, item) in self.items.iter().enumerate() {
            if !tokens
                .iter()
                .all(|tok| contains_ascii_or_unicode_ci(&item.name, tok))
            {
                continue;
            }
            let name_lower = item.name.to_lowercase();
            let base = if name_lower == q_lower {
                100u32
            } else if name_lower.starts_with(&q_lower) {
                85
            } else if name_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w.starts_with(&tokens[0]))
            {
                70
            } else {
                50
            };
            let score = base + if item.is_dir { 3 } else { 0 };
            hits.push((score, idx));
        }

        hits.sort_by(|(sa, ia), (sb, ib)| {
            let a = &self.items[*ia];
            let b = &self.items[*ib];
            sb.cmp(sa)
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        hits.truncate(max_results);

        hits.into_iter()
            .map(|(_, idx)| {
                let item = &self.items[idx];
                let parent_dir = &self.dirs[item.dir_idx as usize];
                let path = Path::new(parent_dir)
                    .join(&*item.name)
                    .to_string_lossy()
                    .into_owned();
                FileEntry {
                    name: item.name.to_string(),
                    path,
                    parent_dir: parent_dir.clone(),
                    is_dir: item.is_dir,
                }
            })
            .collect()
    }
}

fn scope_prefix_for_everything(custom_scope: &str, term: &str) -> String {
    let trimmed = custom_scope.trim();
    if trimmed.is_empty() {
        return term.to_string();
    }
    let parts: Vec<String> = trimmed
        .split([',', ';'])
        .filter_map(|p| {
            let s = p.trim().trim_matches('"');
            if s.is_empty() {
                None
            } else if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() {
                Some(format!("\"{}:\\\\\"", s.to_ascii_uppercase()))
            } else if s.len() == 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':' {
                Some(format!("\"{}\\\\\"", s.to_ascii_uppercase()))
            } else {
                Some(format!("\"{}\"", s.replace('/', "\\")))
            }
        })
        .collect();
    if parts.is_empty() {
        term.to_string()
    } else if parts.len() == 1 {
        format!("{} {term}", parts[0])
    } else {
        format!("<{}> {term}", parts.join("|"))
    }
}

fn merge_and_rank_entries(term: &str, primary: Vec<FileEntry>, secondary: Vec<FileEntry>) -> Vec<FileEntry> {
    let q_lower = term.trim().to_lowercase();
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(primary.len() + secondary.len());
    for entry in primary.into_iter().chain(secondary) {
        if seen.insert(entry.path.to_lowercase()) {
            merged.push(entry);
        }
    }
    merged.sort_by(|a, b| {
        let a_lower = a.name.to_lowercase();
        let b_lower = b.name.to_lowercase();
        let a_rank = if a_lower == q_lower {
            3u8
        } else if a_lower.starts_with(&q_lower) {
            2
        } else {
            1
        };
        let b_rank = if b_lower == q_lower {
            3u8
        } else if b_lower.starts_with(&q_lower) {
            2
        } else {
            1
        };
        b_rank
            .cmp(&a_rank)
            .then_with(|| b.is_dir.cmp(&a.is_dir))
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a_lower.cmp(&b_lower))
    });
    merged.truncate(MAX_FILE_RESULTS);
    merged
}

fn query_windows_search_all(
    term: &str,
    seq: u64,
    latest_seq: &AtomicU64,
    index: Option<&Arc<CompactFileIndex>>,
    custom_scope: &str,
) -> Vec<FileEntry> {
    if looks_like_fs_path(term) {
        let direct = query_direct_fs_path(term, MAX_FILE_RESULTS);
        if !direct.is_empty() {
            return direct;
        }
    }

    let idx_hits = index
        .map(|idx| idx.search(term, MAX_FILE_RESULTS))
        .unwrap_or_default();

    let scope_empty = custom_scope.trim().is_empty();
    let com_hits = if scope_empty {
        query_windows_search_com(term, seq, latest_seq, MAX_FILE_RESULTS).unwrap_or_default()
    } else {
        Vec::new()
    };

    let merged = merge_and_rank_entries(term, idx_hits, com_hits);
    if !merged.is_empty() || !scope_empty || index.is_some() {
        return merged;
    }

    if latest_seq.load(Ordering::Acquire) != seq {
        Vec::new()
    } else {
        query_shallow_user_folders(term, seq, latest_seq, MAX_FILE_RESULTS)
    }
}

struct SearchReq {
    seq: u64,
    query: String,
    backend: String,
    everything_path: String,
    drives_scope: String,
}

struct FileShared {
    latest_seq: AtomicU64,
    snapshot: Mutex<FileSearchSnapshot>,
    notify_hwnd: AtomicIsize,
    notify_msg: u32,
    installing: AtomicBool,
    index_building: AtomicBool,
    index_cancel: AtomicBool,
    index: Mutex<Option<Arc<CompactFileIndex>>>,
    everything_dll: Mutex<Option<Arc<EverythingDll>>>,
}

unsafe impl Send for FileShared {}
unsafe impl Sync for FileShared {}

impl FileShared {
    fn notify(&self) {
        let raw = self.notify_hwnd.load(Ordering::Acquire);
        if raw != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(raw as *mut c_void)),
                    self.notify_msg,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    fn ensure_index_async(self: &Arc<Self>, drives_scope: &str, force: bool) {
        let scope_key = drives_scope.trim().to_lowercase();
        let needs_rebuild = {
            let guard = self.index.lock().unwrap();
            match guard.as_ref() {
                None => true,
                Some(idx) => {
                    force
                        || idx.scope_key != scope_key
                        || idx.built_at.elapsed() >= Duration::from_secs(INDEX_TTL_SECS)
                }
            }
        };
        if !needs_rebuild {
            return;
        }
        if force {
            self.index_cancel.store(true, Ordering::Release);
        }
        if self
            .index_building
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let shared = Arc::clone(self);
        let scope_owned = drives_scope.to_string();
        thread::spawn(move || {
            shared.index_cancel.store(false, Ordering::Release);
            let roots = enumerate_search_roots(&scope_owned);
            let built = Arc::new(CompactFileIndex::build_bfs(
                scope_owned.trim().to_lowercase(),
                &roots,
                &shared.index_cancel,
            ));
            *shared.index.lock().unwrap() = Some(built);
            shared.index_building.store(false, Ordering::Release);
            {
                let mut snap = shared.snapshot.lock().unwrap();
                snap.query.clear();
            }
            shared.notify();
        });
    }

    fn load_dll(&self, exe_hint: Option<&Path>) -> Option<Arc<EverythingDll>> {
        if let Some(existing) = self.everything_dll.lock().unwrap().as_ref() {
            return Some(Arc::clone(existing));
        }
        let dll_path = detect_everything_dll(exe_hint)?;
        let loaded = unsafe { EverythingDll::load(&dll_path) }.ok()?;
        let arc = Arc::new(loaded);
        *self.everything_dll.lock().unwrap() = Some(Arc::clone(&arc));
        Some(arc)
    }

    fn current_everything_state(&self, custom_path: &str) -> EverythingState {
        if self.installing.load(Ordering::Acquire) {
            return EverythingState::Installing;
        }
        if let Some(ref prev) = Some(self.snapshot.lock().unwrap().everything_state.clone()) {
            if matches!(prev, EverythingState::InstallFailed(_)) {
                // Keep failure message until user triggers setup or Everything starts externally.
                if !is_everything_running() {
                    return prev.clone();
                }
            }
        }
        let detected = detect_everything_exe(custom_path);
        if is_everything_running() {
            if detect_everything_dll(detected.as_deref()).is_some()
                || detect_es_cli(detected.as_deref()).is_some()
            {
                return EverythingState::Ready;
            }
            if let Some(p) = detected {
                return EverythingState::SdkMissing(p);
            }
            return EverythingState::SdkMissing(noren_everything_dir().join("Everything.exe"));
        }
        if detected.is_some() {
            EverythingState::Starting
        } else {
            EverythingState::NotInstalled
        }
    }
}

/// Asynchronous file search engine supporting Windows Search Index (`SystemIndex`)
/// and Voidtools Everything (system-installed or Noren-managed).
pub struct FileSearchEngine {
    shared: Arc<FileShared>,
    tx: mpsc::Sender<SearchReq>,
}

impl FileSearchEngine {
    pub fn new(notify_msg: u32) -> Self {
        let shared = Arc::new(FileShared {
            latest_seq: AtomicU64::new(0),
            snapshot: Mutex::new(FileSearchSnapshot::default()),
            notify_hwnd: AtomicIsize::new(0),
            notify_msg,
            installing: AtomicBool::new(false),
            index_building: AtomicBool::new(false),
            index_cancel: AtomicBool::new(false),
            index: Mutex::new(None),
            everything_dll: Mutex::new(None),
        });

        let (tx, rx) = mpsc::channel::<SearchReq>();
        let worker_shared = Arc::clone(&shared);
        thread::spawn(move || {
            while let Ok(mut req) = rx.recv() {
                // Coalesce rapid keystrokes.
                while let Ok(newer) = rx.try_recv() {
                    req = newer;
                }
                let debounce = if req.backend == "everything" {
                    Duration::from_millis(25)
                } else {
                    Duration::from_millis(65)
                };
                thread::sleep(debounce);
                while let Ok(newer) = rx.try_recv() {
                    req = newer;
                }
                if worker_shared.latest_seq.load(Ordering::Acquire) != req.seq {
                    continue;
                }

                let mut ev_state = worker_shared.current_everything_state(&req.everything_path);

                // If Everything backend is selected and installed on disk but stopped, start it silently.
                if req.backend == "everything" && matches!(ev_state, EverythingState::Starting) {
                    if let Some(exe) = detect_everything_exe(&req.everything_path) {
                        {
                            let mut snap = worker_shared.snapshot.lock().unwrap();
                            snap.everything_state = EverythingState::Starting;
                        }
                        worker_shared.notify();
                        if start_everything_background(&exe) {
                            let deadline = Instant::now() + Duration::from_millis(2500);
                            while Instant::now() < deadline {
                                if is_everything_running() {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(80));
                            }
                        }
                        ev_state = worker_shared.current_everything_state(&req.everything_path);
                    }
                }

                // If Everything is running on the user's system but Everything64.dll/es.exe is missing,
                // fetch the lightweight SDK DLL (~200 KB) automatically once into Noren's tool folder.
                if req.backend == "everything" && matches!(ev_state, EverythingState::SdkMissing(_)) {
                    let _ = download_and_extract_everything_sdk(&noren_everything_dir());
                    ev_state = worker_shared.current_everything_state(&req.everything_path);
                }

                if req.backend != "everything" || !matches!(ev_state, EverythingState::Ready) {
                    worker_shared.ensure_index_async(&req.drives_scope, false);
                }

                if req.query.trim().is_empty() {
                    let mut snap = worker_shared.snapshot.lock().unwrap();
                    snap.query = String::new();
                    snap.backend = req.backend;
                    snap.loading = false;
                    snap.everything_state = ev_state;
                    snap.entries.clear();
                    drop(snap);
                    worker_shared.notify();
                    continue;
                }

                let mut entries = None;
                if req.backend == "everything" && matches!(ev_state, EverythingState::Ready) {
                    let scoped_q = scope_prefix_for_everything(&req.drives_scope, &req.query);
                    let exe_hint = detect_everything_exe(&req.everything_path);
                    if let Some(dll) = worker_shared.load_dll(exe_hint.as_deref()) {
                        entries = dll.query(&scoped_q, MAX_FILE_RESULTS_U32);
                    }
                    if entries.is_none() {
                        if let Some(es) = detect_es_cli(exe_hint.as_deref()) {
                            entries = query_everything_via_es(&es, &scoped_q, MAX_FILE_RESULTS_U32);
                        }
                    }
                }

                // Fall back seamlessly to multi-partition + Windows Search if Everything isn't ready yet.
                let final_entries = entries.unwrap_or_else(|| {
                    let idx_guard = worker_shared.index.lock().unwrap().clone();
                    query_windows_search_all(
                        &req.query,
                        req.seq,
                        &worker_shared.latest_seq,
                        idx_guard.as_ref(),
                        &req.drives_scope,
                    )
                });

                if worker_shared.latest_seq.load(Ordering::Acquire) != req.seq {
                    continue;
                }

                {
                    let mut snap = worker_shared.snapshot.lock().unwrap();
                    snap.query = req.query;
                    snap.backend = req.backend;
                    snap.loading = false;
                    snap.everything_state = ev_state;
                    snap.entries = final_entries;
                }
                worker_shared.notify();
            }
        });

        Self { shared, tx }
    }

    pub fn set_notify_hwnd(&self, hwnd: HWND) {
        self.shared
            .notify_hwnd
            .store(hwnd.0 as isize, Ordering::Release);
    }

    pub fn snapshot(&self) -> FileSearchSnapshot {
        self.shared.snapshot.lock().unwrap().clone()
    }

    pub fn ensure_index(&self, drives_scope: &str) {
        self.shared.ensure_index_async(drives_scope, false);
    }

    pub fn trigger_rescan(&self, drives_scope: &str) {
        self.shared.ensure_index_async(drives_scope, true);
    }

    pub fn request_search(
        &self,
        term: &str,
        backend: &str,
        everything_path: &str,
        drives_scope: &str,
    ) {
        let trimmed = term.trim();
        let next_ev = self.shared.current_everything_state(everything_path);
        {
            let snap = self.shared.snapshot.lock().unwrap();
            if snap.query == trimmed
                && snap.backend == backend
                && !snap.loading
                && snap.everything_state == next_ev
            {
                return;
            }
        }

        let seq = self.shared.latest_seq.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let mut snap = self.shared.snapshot.lock().unwrap();
            snap.backend = backend.to_string();
            snap.everything_state = next_ev;
            if trimmed.is_empty() {
                snap.query.clear();
                snap.loading = false;
                snap.entries.clear();
            } else {
                snap.query = trimmed.to_string();
                snap.loading = true;
            }
        }

        let _ = self.tx.send(SearchReq {
            seq,
            query: trimmed.to_string(),
            backend: backend.to_string(),
            everything_path: everything_path.to_string(),
            drives_scope: drives_scope.to_string(),
        });
    }

    pub fn trigger_setup_everything(&self, custom_path: &str) -> String {
        if self
            .shared
            .installing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return "Everything setup is already in progress.".to_string();
        }

        {
            let mut snap = self.shared.snapshot.lock().unwrap();
            snap.everything_state = EverythingState::Installing;
        }
        self.shared.notify();

        let shared = Arc::clone(&self.shared);
        let custom = custom_path.to_string();
        thread::spawn(move || {
            let outcome: Result<(), String> = (|| {
                let mut exe = detect_everything_exe(&custom);
                if exe.is_none() && !is_everything_running() {
                    if try_winget_install_everything() {
                        exe = detect_everything_exe(&custom);
                    }
                }
                if exe.is_none() && !is_everything_running() {
                    let installed = download_and_install_portable_everything(&noren_everything_dir())?;
                    exe = Some(installed);
                }

                if detect_everything_dll(exe.as_deref()).is_none()
                    && detect_es_cli(exe.as_deref()).is_none()
                {
                    let _ = download_and_extract_everything_sdk(&noren_everything_dir())?;
                }

                if !is_everything_running() {
                    if let Some(ref exe_path) = exe {
                        if !start_everything_background(exe_path) {
                            return Err("Failed to launch Everything.exe -startup".to_string());
                        }
                        let deadline = Instant::now() + Duration::from_secs(6);
                        while Instant::now() < deadline {
                            if is_everything_running() {
                                break;
                            }
                            thread::sleep(Duration::from_millis(120));
                        }
                    }
                }

                if is_everything_running()
                    && (detect_everything_dll(exe.as_deref()).is_some()
                        || detect_es_cli(exe.as_deref()).is_some())
                {
                    Ok(())
                } else {
                    Err("Everything did not finish starting; open Everything once to initialize its index.".to_string())
                }
            })();

            let next_state = match outcome {
                Ok(()) => EverythingState::Ready,
                Err(err) => EverythingState::InstallFailed(err),
            };
            {
                let mut snap = shared.snapshot.lock().unwrap();
                snap.everything_state = next_state;
                snap.query.clear();
            }
            shared.installing.store(false, Ordering::Release);
            shared.notify();
        });

        "Started Everything detection/installation in background.".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn builds_windows_search_aqs_safely() {
        assert_eq!(
            build_windows_search_aqs("quarterly report"),
            "System.FileName:~\"quarterly\" AND System.FileName:~\"report\""
        );
        assert_eq!(build_windows_search_aqs("ext:pdf"), "ext:pdf");
        assert_eq!(build_windows_search_aqs("   "), "");
    }

    #[test]
    fn direct_fs_path_lists_matching_files() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("noren_files_test_{nanos}"));
        fs::create_dir_all(dir.join("SubFolder")).unwrap();
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        fs::write(dir.join("other.md"), "world").unwrap();

        let query_dir = format!("{}\\", dir.display());
        let all = query_direct_fs_path(&query_dir, 10);
        assert_eq!(all.len(), 3);
        assert!(all[0].is_dir);
        assert_eq!(all[0].name, "SubFolder");

        let query_pref = format!("{}\\not", dir.display());
        let filtered = query_direct_fs_path(&query_pref, 10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "notes.txt");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compact_index_searches_multiple_partitions_and_skips_noise() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("noren_multipart_idx_{nanos}"));
        let root_c = base.join("C_Drive");
        let root_d = base.join("D_Drive");
        fs::create_dir_all(root_c.join("Docs")).unwrap();
        fs::create_dir_all(root_d.join("Media")).unwrap();
        fs::create_dir_all(root_d.join("node_modules")).unwrap();
        fs::create_dir_all(root_d.join("target")).unwrap();

        fs::write(root_c.join("Docs").join("budget_2026.xlsx"), "c").unwrap();
        fs::write(root_d.join("Media").join("budget_notes.md"), "d").unwrap();
        fs::write(root_d.join("Media").join("holiday_budget.jpg"), "d2").unwrap();
        fs::write(root_d.join("node_modules").join("budget_ignored.js"), "x").unwrap();
        fs::write(root_d.join("target").join("budget_ignored.exe"), "x").unwrap();

        let cancel = AtomicBool::new(false);
        let idx = CompactFileIndex::build_bfs(String::new(), &[root_c, root_d], &cancel);
        let hits = idx.search("budget", 10);

        let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names.len(), 3, "got: {names:?}");
        assert!(names.contains(&"budget_2026.xlsx"));
        assert!(names.contains(&"budget_notes.md"));
        assert_eq!(hits[2].name, "holiday_budget.jpg");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn size_of_ptr_and_dll_constants_sane() {
        assert!(std::mem::size_of::<usize>() >= 4);
        assert_eq!(EVERYTHING_REQUEST_FILE_NAME | EVERYTHING_REQUEST_PATH, 3);
    }
}
