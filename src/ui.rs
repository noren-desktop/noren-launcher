use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Instant;

use noren_sdk::protocol::CAP_LAUNCHER_ACTIONS;
use noren_sdk::schema::FieldValue;
use noren_sdk::theme::{load_theme, ResolvedTheme, ThemeMaterial};
use noren_sdk::NipEvent;
use serde_json::json;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1BitmapBrush, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush, D2D1_BITMAP_BRUSH_PROPERTIES, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_BRUSH_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, OpenClipboard,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, RegisterHotKey, UnregisterHotKey, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowPos,
    ShowWindow, SystemParametersInfoW, TranslateMessage, HWND_TOPMOST, IDC_ARROW, MSG,
    SPI_GETWORKAREA, SWP_NOACTIVATE, SW_HIDE, SW_SHOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CHAR, WM_DESTROY, WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_numerics::Matrix3x2;

use crate::actions::{
    open_file_or_folder, open_url_in_browser, reveal_in_explorer, run_external_wallpaper_command,
    run_power_action, set_autostart,
};
use crate::apps::{load_apps, launch_app, now_unix_secs, AppEntry, FrecencyStore};
use crate::config::{build_launcher_manifest, parse_hotkey_chord, LauncherConfig};
use crate::files::FileSearchEngine;
use crate::icons::{IconCache, ICON_BYTES, ICON_DIM};
use crate::nip::{
    LauncherNipRuntime, TRAY_ACT_AUTOSTART, TRAY_ACT_RELOAD_APPS, TRAY_ACT_TOGGLE,
    TRAY_ACT_WALLPAPER, WM_APP_NIP_EVENT, WM_APP_QUIT, WM_APP_TRAY_ACTION,
};
use crate::search::{
    detect_mode, extract_file_search_term, query_results, ConfirmationState, ItemEffect,
    RemoteAction, SearchItem, SearchMode,
};
use crate::wallpaper::{
    apply_wallpaper, scan_wallpaper_folder, ThumbCache, WallpaperScan, THUMB_BYTES, THUMB_H,
    THUMB_W,
};

pub const WM_APP_TOGGLE: u32 = WM_APP + 1;
pub const WM_APP_APPS_LOADED: u32 = WM_APP + 2;
pub const WM_APP_ICON_READY: u32 = WM_APP + 3;
pub const WM_APP_WALLPAPERS_LOADED: u32 = WM_APP + 4;
pub const WM_APP_THUMB_READY: u32 = WM_APP + 5;
pub const WM_APP_FILE_RESULTS: u32 = WM_APP + 6;

const HOTKEY_TOGGLE_ID: i32 = 1;
const TIMER_CAROUSEL_ANIM: usize = 2;
const CF_UNICODETEXT_ID: u32 = 13;

const SEARCH_BAR_H: f32 = 56.0;
const ROW_H: f32 = 50.0;
const LIST_PAD_Y: f32 = 8.0;

pub const CAROUSEL_RADIUS: usize = 5;
pub const CAROUSEL_CENTER_W: f32 = 320.0;
pub const CAROUSEL_STRIP_W: f32 = 56.0;
pub const CAROUSEL_SLOT_H: f32 = 180.0;
pub const CAROUSEL_GAP: f32 = 8.0;
pub const CAROUSEL_PANEL_H: f32 = 248.0;
const CAROUSEL_ANIM_MS: f32 = 220.0;
const WHEEL_COOLDOWN_MS: u128 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Search,
    Wallpaper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelGeometry {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub show_list: bool,
    pub visible_rows: usize,
}

pub fn carousel_radius_for(count: usize) -> usize {
    if count <= 1 {
        0
    } else {
        CAROUSEL_RADIUS.min((count - 1) / 2)
    }
}

pub fn carousel_panel_width(count: usize) -> f32 {
    if count == 0 {
        520.0
    } else {
        (CAROUSEL_CENTER_W
            + 2.0 * (carousel_radius_for(count) as f32) * (CAROUSEL_STRIP_W + CAROUSEL_GAP)
            + 64.0)
            .max(380.0)
    }
}

/// Returns `(center_x, width, active_t)` in logical pixels for a slot at continuous offset `d`.
pub fn carousel_slot_layout(d: f32) -> (f32, f32, f32) {
    let first_x = CAROUSEL_CENTER_W * 0.5 + CAROUSEL_GAP + CAROUSEL_STRIP_W * 0.5;
    if d.abs() < 1.0 {
        let t = 1.0 - d.abs();
        let w = CAROUSEL_STRIP_W + (CAROUSEL_CENTER_W - CAROUSEL_STRIP_W) * t;
        let cx = d * first_x;
        (cx, w, t)
    } else {
        let sign = if d >= 0.0 { 1.0 } else { -1.0 };
        let cx = sign * (first_x + (d.abs() - 1.0) * (CAROUSEL_STRIP_W + CAROUSEL_GAP));
        (cx, CAROUSEL_STRIP_W, 0.0)
    }
}

/// Computes the Launcher position (centered horizontally, 25% down from work area top) and height.
pub fn compute_panel_geometry(
    work: RECT,
    dpi_scale: f32,
    panel_width: f32,
    max_visible_rows: usize,
    compact_mode: bool,
    query: &str,
    result_count: usize,
    view: View,
    wallpaper_count: usize,
) -> PanelGeometry {
    let work_w = (work.right - work.left).max(320);
    let work_h = (work.bottom - work.top).max(240);
    let y = work.top + work_h / 4;

    if view == View::Wallpaper {
        let w = (carousel_panel_width(wallpaper_count) * dpi_scale)
            .round()
            .max(320.0) as i32;
        let w = w.min(work_w - 32);
        let x = work.left + (work_w - w) / 2;
        return PanelGeometry {
            x,
            y,
            w,
            h: (CAROUSEL_PANEL_H * dpi_scale).round() as i32,
            show_list: false,
            visible_rows: 0,
        };
    }

    let w = (panel_width * dpi_scale).round().max(320.0) as i32;
    let w = w.min(work_w - 32);
    let x = work.left + (work_w - w) / 2;

    let bar_h = (SEARCH_BAR_H * dpi_scale).round() as i32;
    if compact_mode && query.trim().is_empty() {
        return PanelGeometry {
            x,
            y,
            w,
            h: bar_h,
            show_list: false,
            visible_rows: 0,
        };
    }

    let visible_rows = result_count.max(1).min(max_visible_rows.max(1));
    let list_h = (visible_rows as f32 * ROW_H * dpi_scale).round() as i32
        + (LIST_PAD_Y * 2.0 * dpi_scale).round() as i32
        + 1;
    PanelGeometry {
        x,
        y,
        w,
        h: bar_h + list_h,
        show_list: true,
        visible_rows,
    }
}

struct D2dResources {
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    target: Option<ID2D1HwndRenderTarget>,
    bitmaps: HashMap<String, ID2D1Bitmap>,
    thumb_bitmaps: HashMap<u64, ID2D1Bitmap>,
    cached_scale_bits: u32,
    fmt_input: Option<IDWriteTextFormat>,
    fmt_title: Option<IDWriteTextFormat>,
    fmt_sub: Option<IDWriteTextFormat>,
    fmt_title_center: Option<IDWriteTextFormat>,
    fmt_sub_center: Option<IDWriteTextFormat>,
    fmt_glyph: Option<IDWriteTextFormat>,
    fmt_letter: Option<IDWriteTextFormat>,
}

impl D2dResources {
    fn new() -> Result<Self, String> {
        let d2d_factory: ID2D1Factory = unsafe {
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("D2D1CreateFactory failed: {e}"))?
        };
        let dwrite_factory: IDWriteFactory = unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                .map_err(|e| format!("DWriteCreateFactory failed: {e}"))?
        };
        Ok(Self {
            d2d_factory,
            dwrite_factory,
            target: None,
            bitmaps: HashMap::new(),
            thumb_bitmaps: HashMap::new(),
            cached_scale_bits: 0,
            fmt_input: None,
            fmt_title: None,
            fmt_sub: None,
            fmt_title_center: None,
            fmt_sub_center: None,
            fmt_glyph: None,
            fmt_letter: None,
        })
    }

    fn ensure_formats(&mut self, scale: f32) {
        let bits = scale.to_bits();
        if self.cached_scale_bits == bits && self.fmt_input.is_some() {
            return;
        }
        self.cached_scale_bits = bits;
        let mk = |family: PCWSTR, weight, size: f32, center: bool| -> Option<IDWriteTextFormat> {
            unsafe {
                let fmt = self
                    .dwrite_factory
                    .CreateTextFormat(
                        family,
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size * scale,
                        w!("en-us"),
                    )
                    .ok()?;
                let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = fmt.SetTextAlignment(if center {
                    DWRITE_TEXT_ALIGNMENT_CENTER
                } else {
                    DWRITE_TEXT_ALIGNMENT_LEADING
                });
                let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                Some(fmt)
            }
        };

        self.fmt_input = mk(w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_NORMAL, 16.0, false);
        self.fmt_title = mk(w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.0, false);
        self.fmt_sub = mk(w!("Segoe UI Variable Small"), DWRITE_FONT_WEIGHT_NORMAL, 11.5, false);
        self.fmt_title_center =
            mk(w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.0, true);
        self.fmt_sub_center =
            mk(w!("Segoe UI Variable Small"), DWRITE_FONT_WEIGHT_NORMAL, 11.5, true);
        self.fmt_glyph = mk(w!("Segoe Fluent Icons"), DWRITE_FONT_WEIGHT_NORMAL, 16.0, true);
        self.fmt_letter = mk(w!("Segoe UI Variable Text"), DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.0, true);
    }

    fn ensure_target(&mut self, hwnd: HWND, width: u32, height: u32) -> Option<ID2D1HwndRenderTarget> {
        if let Some(ref rt) = self.target {
            let _ = unsafe {
                rt.Resize(&D2D_SIZE_U {
                    width: width.max(1),
                    height: height.max(1),
                })
            };
            return Some(rt.clone());
        }

        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U {
                width: width.max(1),
                height: height.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        match unsafe { self.d2d_factory.CreateHwndRenderTarget(&props, &hwnd_props) } {
            Ok(rt) => {
                self.target = Some(rt.clone());
                Some(rt)
            }
            Err(_) => None,
        }
    }

    fn drop_target(&mut self) {
        self.bitmaps.clear();
        self.thumb_bitmaps.clear();
        self.target = None;
    }

    fn bitmap_for_app(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        app_id: &str,
        pixels: &[u8; ICON_BYTES],
    ) -> Option<ID2D1Bitmap> {
        if let Some(bmp) = self.bitmaps.get(app_id) {
            return Some(bmp.clone());
        }
        let bmp_props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bmp = unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U {
                    width: ICON_DIM,
                    height: ICON_DIM,
                },
                Some(pixels.as_ptr() as *const c_void),
                ICON_DIM * 4,
                &bmp_props,
            )
            .ok()?
        };
        self.bitmaps.insert(app_id.to_string(), bmp.clone());
        Some(bmp)
    }

    fn bitmap_for_thumb(
        &mut self,
        rt: &ID2D1HwndRenderTarget,
        cache_key: u64,
        pixels: &[u8; THUMB_BYTES],
    ) -> Option<ID2D1Bitmap> {
        if let Some(bmp) = self.thumb_bitmaps.get(&cache_key) {
            return Some(bmp.clone());
        }
        let bmp_props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bmp = unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U {
                    width: THUMB_W,
                    height: THUMB_H,
                },
                Some(pixels.as_ptr() as *const c_void),
                THUMB_W * 4,
                &bmp_props,
            )
            .ok()?
        };
        if self.thumb_bitmaps.len() >= 40 {
            if let Some(&first_key) = self.thumb_bitmaps.keys().next() {
                self.thumb_bitmaps.remove(&first_key);
            }
        }
        self.thumb_bitmaps.insert(cache_key, bmp.clone());
        Some(bmp)
    }
}

struct WallpaperState {
    loading: bool,
    scan: Option<WallpaperScan>,
    virtual_idx: i64,
    anim_start_val: f32,
    anim_target_val: f32,
    anim_current: f32,
    anim_started_at: Instant,
    anim_active: bool,
    hovered_v: Option<i64>,
    last_wheel_at: Instant,
    from_search: bool,
}

impl Default for WallpaperState {
    fn default() -> Self {
        Self {
            loading: false,
            scan: None,
            virtual_idx: 0,
            anim_start_val: 0.0,
            anim_target_val: 0.0,
            anim_current: 0.0,
            anim_started_at: Instant::now(),
            anim_active: false,
            hovered_v: None,
            last_wheel_at: Instant::now(),
            from_search: false,
        }
    }
}

struct AppState {
    hwnd: HWND,
    cfg: LauncherConfig,
    theme: ResolvedTheme,
    apps: Vec<AppEntry>,
    apps_loaded: bool,
    frecency: FrecencyStore,
    icons: IconCache,
    thumbs: ThumbCache,
    files: FileSearchEngine,
    wp: WallpaperState,
    nip: Option<LauncherNipRuntime>,
    remote_actions: Vec<RemoteAction>,
    view: View,
    query: String,
    cursor: usize,
    selected: usize,
    scroll_offset: usize,
    results: Vec<SearchItem>,
    confirm: ConfirmationState,
    visible: bool,
    opened_at: Instant,
    hotkey_registered: bool,
    d2d: D2dResources,
}

static PENDING_APPS: Mutex<Option<Vec<AppEntry>>> = Mutex::new(None);
static PENDING_WALLPAPERS: Mutex<Option<WallpaperScan>> = Mutex::new(None);
static STATE_PTR: Mutex<usize> = Mutex::new(0);

fn d2d_color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rgb_to_d2d(rgb: (u8, u8, u8), a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: rgb.0 as f32 / 255.0,
        g: rgb.1 as f32 / 255.0,
        b: rgb.2 as f32 / 255.0,
        a,
    }
}

fn enable_native_dark_menus() {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else {
            return;
        };
        type SetPreferredAppModeFn = unsafe extern "system" fn(i32) -> i32;
        type FlushMenuThemesFn = unsafe extern "system" fn();
        if let Some(f) = GetProcAddress(ux, windows::core::PCSTR(135usize as *const u8)) {
            let set_mode: SetPreferredAppModeFn = std::mem::transmute(f);
            let _ = set_mode(1);
        }
        if let Some(f) = GetProcAddress(ux, windows::core::PCSTR(136usize as *const u8)) {
            let flush: FlushMenuThemesFn = std::mem::transmute(f);
            flush();
        }
    }
}

unsafe fn apply_dwm_style(hwnd: HWND, theme: ResolvedTheme) {
    let dark: u32 = if theme.is_dark { 1 } else { 0 };
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        (&dark as *const u32).cast::<c_void>(),
        4,
    );
    let corner: u32 = DWMWCP_ROUND.0 as u32;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        (&corner as *const u32).cast::<c_void>(),
        4,
    );
    let margins = if theme.transparency_enabled {
        MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        }
    } else {
        MARGINS::default()
    };
    let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    let backdrop: u32 = if !theme.transparency_enabled {
        1
    } else {
        match theme.material {
            ThemeMaterial::Mica => 2,
            ThemeMaterial::Acrylic => 3,
        }
    };
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        (&backdrop as *const u32).cast::<c_void>(),
        4,
    );
}

unsafe fn work_area() -> RECT {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    let _ = SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        Some((&mut rc as *mut RECT).cast::<c_void>()),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    );
    rc
}

fn spawn_apps_loader(hwnd: HWND) {
    let raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        let loaded = load_apps();
        *PENDING_APPS.lock().unwrap() = Some(loaded);
        unsafe {
            let _ = PostMessageW(
                Some(HWND(raw as *mut c_void)),
                WM_APP_APPS_LOADED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
}

fn spawn_wallpaper_scanner(hwnd: HWND, configured_dir: String) {
    let raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        let scan = scan_wallpaper_folder(&configured_dir);
        *PENDING_WALLPAPERS.lock().unwrap() = Some(scan);
        unsafe {
            let _ = PostMessageW(
                Some(HWND(raw as *mut c_void)),
                WM_APP_WALLPAPERS_LOADED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
}

fn read_clipboard_text(hwnd: HWND) -> Option<String> {
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return None;
        }
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT_ID).ok()?;
            let ptr = GlobalLock(HGLOBAL(handle.0)) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
                if len > 4096 {
                    break;
                }
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let text = String::from_utf16_lossy(slice)
                .replace(['\r', '\n'], " ");
            let _ = GlobalUnlock(HGLOBAL(handle.0));
            Some(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

impl AppState {
    fn register_configured_hotkey(&mut self) {
        unsafe {
            if self.hotkey_registered {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_TOGGLE_ID);
                self.hotkey_registered = false;
            }
            if let Some((mods, vk)) = parse_hotkey_chord(&self.cfg.hotkey) {
                if RegisterHotKey(Some(self.hwnd), HOTKEY_TOGGLE_ID, mods, vk).is_ok() {
                    self.hotkey_registered = true;
                }
            }
        }
    }

    fn refresh_results(&mut self) {
        let now = now_unix_secs();
        if let Some(term) = extract_file_search_term(&self.query) {
            self.files.request_search(
                term,
                &self.cfg.file_search_backend,
                &self.cfg.everything_path,
                &self.cfg.file_search_drives,
            );
        } else if detect_mode(&self.query) == SearchMode::Bangs
            && self.cfg.file_search_backend != "everything"
        {
            self.files.ensure_index(&self.cfg.file_search_drives);
        }
        let file_snap = self.files.snapshot();
        self.results = query_results(
            &self.query,
            &self.cfg,
            &self.apps,
            &self.frecency,
            now,
            &self.remote_actions,
            Some(&file_snap),
        );
        if self.results.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
        } else {
            if self.selected >= self.results.len() {
                self.selected = self.results.len() - 1;
            }
            let max_rows = self.cfg.max_visible_rows.max(1);
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            } else if self.selected >= self.scroll_offset + max_rows {
                self.scroll_offset = self.selected + 1 - max_rows;
            }
        }
        self.confirm.clear();
    }

    fn dpi_scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }

    fn current_geometry(&self) -> PanelGeometry {
        let work = unsafe { work_area() };
        let wp_count = self
            .wp
            .scan
            .as_ref()
            .map(|s| s.items.len())
            .unwrap_or(0);
        compute_panel_geometry(
            work,
            self.dpi_scale(),
            self.cfg.panel_width,
            self.cfg.max_visible_rows,
            self.cfg.compact_mode,
            &self.query,
            self.results.len(),
            self.view,
            wp_count,
        )
    }

    fn update_window_bounds(&mut self) {
        let geom = self.current_geometry();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                geom.x,
                geom.y,
                geom.w,
                geom.h,
                SWP_NOACTIVATE,
            );
        }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn open_launcher(&mut self, initial_view: View) {
        self.view = initial_view;
        self.query.clear();
        self.cursor = 0;
        self.selected = 0;
        self.scroll_offset = 0;
        self.confirm.clear();
        if let Some(ref nip) = self.nip {
            self.remote_actions = nip.remote_actions();
        }
        self.refresh_results();
        if initial_view == View::Wallpaper {
            self.start_wallpaper_session(false);
        } else {
            self.update_window_bounds();
        }
        self.visible = true;
        self.opened_at = Instant::now();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.invalidate();
    }

    fn hide_launcher(&mut self) {
        self.visible = false;
        self.view = View::Search;
        self.confirm.clear();
        self.wp.anim_active = false;
        self.wp.hovered_v = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CAROUSEL_ANIM);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.d2d.thumb_bitmaps.clear();
        self.thumbs.clear_memory();
    }

    fn toggle_launcher(&mut self) {
        if self.visible {
            self.hide_launcher();
        } else {
            self.open_launcher(View::Search);
        }
    }

    fn open_wallpaper_view(&mut self, from_search: bool) {
        if self.cfg.wallpaper_mode == "external" {
            let cmd = self.cfg.wallpaper_command.clone();
            self.hide_launcher();
            let _ = run_external_wallpaper_command(&cmd);
        } else {
            self.start_wallpaper_session(from_search);
            if !self.visible {
                self.visible = true;
                self.opened_at = Instant::now();
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_SHOW);
                    let _ = SetForegroundWindow(self.hwnd);
                }
            }
        }
    }

    fn start_wallpaper_session(&mut self, from_search: bool) {
        self.view = View::Wallpaper;
        self.wp.from_search = from_search;
        self.wp.loading = true;
        self.wp.scan = None;
        self.wp.hovered_v = None;
        self.wp.anim_active = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CAROUSEL_ANIM);
        }
        self.update_window_bounds();
        self.invalidate();
        spawn_wallpaper_scanner(self.hwnd, self.cfg.wallpaper_dir.clone());
    }

    fn preload_carousel_thumbs(&self) {
        let Some(ref scan) = self.wp.scan else {
            return;
        };
        let count = scan.items.len();
        if count == 0 {
            return;
        }
        let radius = carousel_radius_for(count) as i64;
        let reach = radius + 3;
        // Request outer slots first and center-most slots last (LIFO stack pops center first).
        for dist in (1..=reach).rev() {
            for sign in [-1i64, 1i64] {
                let idx = (self.wp.virtual_idx + sign * dist).rem_euclid(count as i64) as usize;
                self.thumbs.request_only(&scan.items[idx]);
            }
        }
        let center_idx = self.wp.virtual_idx.rem_euclid(count as i64) as usize;
        self.thumbs.request_only(&scan.items[center_idx]);
    }

    fn step_carousel(&mut self, delta: i64) {
        let count = self.wp.scan.as_ref().map(|s| s.items.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let target = self.wp.virtual_idx + delta;
        self.jump_carousel(target);
    }

    fn jump_carousel(&mut self, target_virtual: i64) {
        let count = self.wp.scan.as_ref().map(|s| s.items.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        self.wp.virtual_idx = target_virtual;
        self.wp.anim_start_val = self.wp.anim_current;
        self.wp.anim_target_val = target_virtual as f32;
        self.wp.anim_started_at = Instant::now();
        self.wp.anim_active = true;
        self.preload_carousel_thumbs();
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_CAROUSEL_ANIM, 16, None);
        }
        self.invalidate();
    }

    fn apply_active_wallpaper(&mut self) {
        let Some(ref scan) = self.wp.scan else {
            return;
        };
        let count = scan.items.len();
        if count == 0 {
            return;
        }
        let idx = self.wp.virtual_idx.rem_euclid(count as i64) as usize;
        let path = scan.items[idx].path.clone();
        self.hide_launcher();
        let _ = apply_wallpaper(&path);
    }

    fn carousel_visible_slots(&self, panel_w: f32, scale: f32) -> Vec<(i64, usize, D2D_RECT_F, f32)> {
        let Some(ref scan) = self.wp.scan else {
            return Vec::new();
        };
        let count = scan.items.len();
        if count == 0 {
            return Vec::new();
        }
        let radius = carousel_radius_for(count) as i64;
        let min_v = if count == 1 {
            0
        } else {
            self.wp.anim_current.floor() as i64 - radius - 1
        };
        let max_v = if count == 1 {
            0
        } else {
            self.wp.anim_current.ceil() as i64 + radius + 1
        };

        let top = 18.0 * scale;
        let slot_h = CAROUSEL_SLOT_H * scale;
        let clip_left = 14.0 * scale;
        let clip_right = panel_w - 14.0 * scale;

        let mut slots = Vec::new();
        for v in min_v..=max_v {
            let d = v as f32 - self.wp.anim_current;
            if count > 1 && d.abs() > (radius as f32 + 0.99) {
                continue;
            }
            let (cx, sw, active_t) = carousel_slot_layout(d);
            let sw_px = sw * scale;
            let left = panel_w * 0.5 + cx * scale - sw_px * 0.5;
            let right = left + sw_px;
            if right < clip_left || left > clip_right {
                continue;
            }
            let actual = v.rem_euclid(count as i64) as usize;
            let rect = D2D_RECT_F {
                left,
                top,
                right,
                bottom: top + slot_h,
            };
            slots.push((v, actual, rect, active_t));
        }

        // Back-to-front so the center card renders on top of side strips.
        slots.sort_by(|a, b| {
            let da = (a.0 as f32 - self.wp.anim_current).abs();
            let db = (b.0 as f32 - self.wp.anim_current).abs();
            db.total_cmp(&da)
        });
        slots
    }

    fn hit_test_carousel(&self, x: f32, y: f32) -> Option<i64> {
        let mut rc = RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut rc);
        }
        let w = (rc.right - rc.left).max(1) as f32;
        let scale = self.dpi_scale();
        let slots = self.carousel_visible_slots(w, scale);
        // Check front-most (center) first.
        for (v, _, rect, _) in slots.into_iter().rev() {
            if x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom {
                return Some(v);
            }
        }
        None
    }

    fn execute_selected(&mut self, ctrl_held: bool) {
        let Some(item) = self.results.get(self.selected).cloned() else {
            return;
        };
        if !self.confirm.arm_or_confirm(&item) {
            self.invalidate();
            return;
        }
        match item.effect {
            ItemEffect::FillQuery { text } => {
                self.query = text;
                self.cursor = self.query.len();
                self.selected = 0;
                self.scroll_offset = 0;
                self.refresh_results();
                self.update_window_bounds();
                self.invalidate();
            }
            ItemEffect::LaunchApp { id } => {
                self.frecency.record_launch(&id, now_unix_secs());
                self.hide_launcher();
                let _ = launch_app(&id);
            }
            ItemEffect::OpenWebUrl { url } => {
                self.hide_launcher();
                let _ = open_url_in_browser(&url);
            }
            ItemEffect::OpenFile { path } => {
                self.hide_launcher();
                if ctrl_held {
                    let _ = reveal_in_explorer(&path);
                } else {
                    let _ = open_file_or_folder(&path);
                }
            }
            ItemEffect::SetupEverything => {
                self.files
                    .trigger_setup_everything(&self.cfg.everything_path);
                self.refresh_results();
                self.update_window_bounds();
                self.invalidate();
            }
            ItemEffect::OpenSettings => {
                self.hide_launcher();
                let _ = noren_sdk::control_center::open_control_center(Some("launcher"));
            }
            ItemEffect::OpenWallpaper => {
                self.open_wallpaper_view(true);
            }
            ItemEffect::ReloadApps => {
                self.hide_launcher();
                spawn_apps_loader(self.hwnd);
                self.files.trigger_rescan(&self.cfg.file_search_drives);
            }
            ItemEffect::PowerCommand { id } => {
                self.hide_launcher();
                let _ = run_power_action(&id);
            }
            ItemEffect::RemoteNipCommand { peer, cap, cmd } => {
                self.hide_launcher();
                if let Some(ref nip) = self.nip {
                    nip.send_remote_action(&peer, &cap, &cmd);
                }
            }
        }
    }

    fn insert_text(&mut self, text: &str) {
        if self.view != View::Search || text.is_empty() {
            return;
        }
        self.query.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.selected = 0;
        self.scroll_offset = 0;
        self.refresh_results();
        self.update_window_bounds();
        self.invalidate();
    }

    fn handle_backspace(&mut self, ctrl_held: bool) {
        if self.view != View::Search || self.cursor == 0 {
            return;
        }
        if ctrl_held {
            let before = &self.query[..self.cursor];
            let trimmed_end = before.trim_end().len();
            let mut new_pos = 0;
            for (idx, ch) in before[..trimmed_end].char_indices() {
                if ch.is_whitespace() {
                    new_pos = idx + ch.len_utf8();
                }
            }
            self.query.drain(new_pos..self.cursor);
            self.cursor = new_pos;
        } else if let Some((idx, _)) = self.query[..self.cursor].char_indices().next_back() {
            self.query.remove(idx);
            self.cursor = idx;
        }
        self.selected = 0;
        self.scroll_offset = 0;
        self.refresh_results();
        self.update_window_bounds();
        self.invalidate();
    }

    fn handle_delete(&mut self) {
        if self.view != View::Search || self.cursor >= self.query.len() {
            return;
        }
        self.query.remove(self.cursor);
        self.selected = 0;
        self.scroll_offset = 0;
        self.refresh_results();
        self.update_window_bounds();
        self.invalidate();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let len = self.results.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        if next != self.selected {
            self.selected = next;
            let max_rows = self.cfg.max_visible_rows.max(1);
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            } else if self.selected >= self.scroll_offset + max_rows {
                self.scroll_offset = self.selected + 1 - max_rows;
            }
            self.confirm.clear();
            self.invalidate();
        }
    }

    fn handle_nip_events(&mut self) {
        let events = match self.nip.as_ref() {
            Some(nip) => nip.drain_events(),
            None => return,
        };
        let mut need_remote_refresh = false;
        let mut need_tray_refresh = false;

        for ev in events {
            match ev {
                NipEvent::PeerConnected(_) | NipEvent::PeerDisconnected { .. } => {
                    need_remote_refresh = true;
                    need_tray_refresh = true;
                }
                NipEvent::StateReceived { cap, .. } => {
                    if cap == CAP_LAUNCHER_ACTIONS {
                        need_remote_refresh = true;
                    }
                    need_tray_refresh = true;
                }
                NipEvent::EventReceived { name, .. } => {
                    if name == "tray.released" {
                        need_tray_refresh = true;
                    }
                }
                NipEvent::ThemeChanged(cfg) => {
                    self.theme = cfg.resolve();
                    unsafe {
                        apply_dwm_style(self.hwnd, self.theme);
                    }
                    self.invalidate();
                }
                NipEvent::Command {
                    from,
                    name,
                    data,
                    reply,
                    ..
                } => match name.as_str() {
                    "toggle" => {
                        let _ = reply.ok(json!({ "ok": true }));
                        self.toggle_launcher();
                    }
                    "reload_apps" => {
                        let _ = reply.ok(json!({ "reloading": true }));
                        spawn_apps_loader(self.hwnd);
                        self.files.trigger_rescan(&self.cfg.file_search_drives);
                    }
                    "rescan_files" => {
                        self.files.trigger_rescan(&self.cfg.file_search_drives);
                        let _ = reply.ok(json!({ "rescanning": true }));
                    }
                    "setup_everything" => {
                        let status = self
                            .files
                            .trigger_setup_everything(&self.cfg.everything_path);
                        self.refresh_results();
                        if self.visible {
                            self.update_window_bounds();
                            self.invalidate();
                        }
                        let _ = reply.ok(json!({ "status": status }));
                    }
                    "config.apply" => {
                        let res = serde_json::from_value::<HashMap<String, FieldValue>>(data)
                            .map_err(|e| format!("invalid config payload: {e}"))
                            .and_then(|updates| {
                                let manifest = build_launcher_manifest();
                                LauncherConfig::apply_updates(&manifest, updates)
                            });
                        match res {
                            Ok(new_cfg) => {
                                if new_cfg.autostart != self.cfg.autostart {
                                    let _ = set_autostart(new_cfg.autostart);
                                }
                                let hotkey_changed = new_cfg.hotkey != self.cfg.hotkey;
                                self.cfg = new_cfg;
                                if hotkey_changed {
                                    self.register_configured_hotkey();
                                }
                                if let Some(ref nip) = self.nip {
                                    nip.sync_tray_state();
                                }
                                if self.cfg.file_search_backend == "everything" {
                                    if !crate::files::is_everything_running() {
                                        if let Some(exe) =
                                            crate::files::detect_everything_exe(&self.cfg.everything_path)
                                        {
                                            let _ = crate::files::start_everything_background(&exe);
                                        }
                                    }
                                }
                                self.refresh_results();
                                if self.visible {
                                    self.update_window_bounds();
                                    self.invalidate();
                                }
                                let _ = reply.ok(json!({ "saved": true }));
                            }
                            Err(e) => {
                                let _ = reply.err(e);
                            }
                        }
                    }
                    "tray.invoke" => {
                        let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        match id {
                            "toggle" => {
                                let _ = reply.ok(json!({ "ok": true }));
                                self.open_launcher(View::Search);
                            }
                            "wallpaper" => {
                                let _ = reply.ok(json!({ "ok": true }));
                                self.open_wallpaper_view(false);
                            }
                            "reload_apps" => {
                                let _ = reply.ok(json!({ "reloading": true }));
                                spawn_apps_loader(self.hwnd);
                            }
                            "autostart" => {
                                let next = !crate::actions::autostart_enabled();
                                let ok = set_autostart(next);
                                self.cfg.autostart = next;
                                if let Some(ref nip) = self.nip {
                                    nip.sync_tray_state();
                                }
                                let _ = reply.ok(json!({ "autostart": next, "ok": ok }));
                            }
                            other => {
                                let _ = reply.err(format!("unsupported launcher tray item '{other}'"));
                            }
                        }
                    }
                    "tray.quit" => {
                        let _ = reply.ok(json!({ "quitting": true }));
                        unsafe {
                            PostQuitMessage(0);
                        }
                    }
                    "tray.yield" => {
                        let yielded = self
                            .nip
                            .as_ref()
                            .is_some_and(|n| n.handle_tray_yield(&from));
                        let _ = reply.ok(json!({ "yielded": yielded }));
                    }
                    other => {
                        let _ = reply.err(format!("unsupported launcher command '{other}'"));
                    }
                },
            }
        }

        if need_tray_refresh {
            if let Some(ref nip) = self.nip {
                nip.notify_tray_topology();
            }
        }
        if need_remote_refresh {
            if let Some(ref nip) = self.nip {
                self.remote_actions = nip.remote_actions();
            }
            if self.visible {
                self.refresh_results();
                self.update_window_bounds();
                self.invalidate();
            }
        }
    }

    fn paint(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut rc);
        }
        let w = (rc.right - rc.left).max(1) as f32;
        let h = (rc.bottom - rc.top).max(1) as f32;
        let scale = self.dpi_scale();
        self.d2d.ensure_formats(scale);

        let Some(rt) = self.d2d.ensure_target(self.hwnd, w as u32, h as u32) else {
            return;
        };

        let is_dark = self.theme.is_dark;
        let bg = if self.theme.transparency_enabled {
            if is_dark {
                d2d_color(0.10, 0.11, 0.13, 0.64)
            } else {
                d2d_color(0.96, 0.96, 0.97, 0.74)
            }
        } else if is_dark {
            d2d_color(0.12, 0.13, 0.15, 1.0)
        } else {
            d2d_color(0.96, 0.96, 0.97, 1.0)
        };
        let text_col = if is_dark {
            d2d_color(0.95, 0.96, 0.98, 1.0)
        } else {
            d2d_color(0.10, 0.11, 0.13, 1.0)
        };
        let muted_col = if is_dark {
            d2d_color(0.65, 0.68, 0.73, 1.0)
        } else {
            d2d_color(0.42, 0.44, 0.49, 1.0)
        };
        let accent_col = rgb_to_d2d(self.theme.accent_rgb, 1.0);
        let divider_col = if is_dark {
            d2d_color(1.0, 1.0, 1.0, 0.10)
        } else {
            d2d_color(0.0, 0.0, 0.0, 0.09)
        };
        let sel_bg = if is_dark {
            d2d_color(1.0, 1.0, 1.0, 0.09)
        } else {
            d2d_color(0.0, 0.0, 0.0, 0.06)
        };
        let tile_bg = if is_dark {
            d2d_color(1.0, 1.0, 1.0, 0.08)
        } else {
            d2d_color(0.0, 0.0, 0.0, 0.05)
        };
        let danger_bg = d2d_color(0.88, 0.25, 0.25, 0.22);
        let danger_fg = if is_dark {
            d2d_color(1.0, 0.56, 0.56, 1.0)
        } else {
            d2d_color(0.75, 0.15, 0.15, 1.0)
        };

        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&bg));
        }

        let brush_text: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&text_col, None).ok() };
        let brush_muted: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&muted_col, None).ok() };
        let brush_accent: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&accent_col, None).ok() };
        let brush_div: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&divider_col, None).ok() };
        let brush_sel: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&sel_bg, None).ok() };
        let brush_tile: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&tile_bg, None).ok() };
        let brush_danger_bg: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&danger_bg, None).ok() };
        let brush_danger_fg: Option<ID2D1SolidColorBrush> =
            unsafe { rt.CreateSolidColorBrush(&danger_fg, None).ok() };

        if let (
            Some(b_text),
            Some(b_muted),
            Some(b_accent),
            Some(b_div),
            Some(b_sel),
            Some(b_tile),
            Some(b_dbg),
            Some(b_dfg),
            Some(fmt_input),
            Some(fmt_title),
            Some(fmt_sub),
            Some(fmt_title_center),
            Some(fmt_sub_center),
            Some(fmt_glyph),
            Some(fmt_letter),
        ) = (
            brush_text,
            brush_muted,
            brush_accent,
            brush_div,
            brush_sel,
            brush_tile,
            brush_danger_bg,
            brush_danger_fg,
            self.d2d.fmt_input.clone(),
            self.d2d.fmt_title.clone(),
            self.d2d.fmt_sub.clone(),
            self.d2d.fmt_title_center.clone(),
            self.d2d.fmt_sub_center.clone(),
            self.d2d.fmt_glyph.clone(),
            self.d2d.fmt_letter.clone(),
        ) {
            if self.view == View::Wallpaper {
                if self.wp.loading {
                    let msg_w: Vec<u16> = "Loading wallpapers…".encode_utf16().collect();
                    let rect = D2D_RECT_F {
                        left: 24.0 * scale,
                        top: 20.0 * scale,
                        right: w - 24.0 * scale,
                        bottom: h - 20.0 * scale,
                    };
                    unsafe {
                        rt.DrawText(
                            &msg_w,
                            &fmt_title_center,
                            &rect,
                            &b_muted,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                } else if self
                    .wp
                    .scan
                    .as_ref()
                    .is_none_or(|s| s.error.is_some() || s.items.is_empty())
                {
                    let (headline, dir_str) = match self.wp.scan.as_ref() {
                        Some(scan) => (
                            scan.error
                                .clone()
                                .unwrap_or_else(|| "No wallpaper images found".to_string()),
                            scan.dir.to_string_lossy().into_owned(),
                        ),
                        None => ("Failed to read wallpaper folder".to_string(), String::new()),
                    };
                    let h_w: Vec<u16> = headline.encode_utf16().collect();
                    let d_w: Vec<u16> = dir_str.encode_utf16().collect();
                    let hint_w: Vec<u16> = "Set Wallpaper Folder in >settings → Launcher · Esc to go back"
                        .encode_utf16()
                        .collect();
                    let r1 = D2D_RECT_F {
                        left: 24.0 * scale,
                        top: 72.0 * scale,
                        right: w - 24.0 * scale,
                        bottom: 102.0 * scale,
                    };
                    let r2 = D2D_RECT_F {
                        left: 24.0 * scale,
                        top: 106.0 * scale,
                        right: w - 24.0 * scale,
                        bottom: 130.0 * scale,
                    };
                    let r3 = D2D_RECT_F {
                        left: 24.0 * scale,
                        top: 136.0 * scale,
                        right: w - 24.0 * scale,
                        bottom: 162.0 * scale,
                    };
                    unsafe {
                        rt.DrawText(
                            &h_w,
                            &fmt_title_center,
                            &r1,
                            &b_text,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                        if !d_w.is_empty() {
                            rt.DrawText(
                                &d_w,
                                &fmt_sub_center,
                                &r2,
                                &b_muted,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                        rt.DrawText(
                            &hint_w,
                            &fmt_sub_center,
                            &r3,
                            &b_muted,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                } else {
                    let slots = self.carousel_visible_slots(w, scale);
                    let scan = self.wp.scan.as_ref().unwrap();
                    let count = scan.items.len();

                    for (v, actual_idx, slot_rect, active_t) in slots {
                        let entry = &scan.items[actual_idx];
                        let corner = 10.0 * scale;
                        let slot_rr = D2D1_ROUNDED_RECT {
                            rect: slot_rect,
                            radiusX: corner,
                            radiusY: corner,
                        };

                        let mut drew_thumb = false;
                        if let Some(px) = self.thumbs.get_or_request(entry) {
                            if let Some(bmp) = self.d2d.bitmap_for_thumb(&rt, entry.cache_key, &px) {
                                let sw = (slot_rect.right - slot_rect.left).max(1.0);
                                let sh = (slot_rect.bottom - slot_rect.top).max(1.0);
                                let s = (sw / THUMB_W as f32).max(sh / THUMB_H as f32);
                                let tx = slot_rect.left + (sw - THUMB_W as f32 * s) * 0.5;
                                let ty = slot_rect.top + (sh - THUMB_H as f32 * s) * 0.5;
                                let bmp_props = D2D1_BITMAP_BRUSH_PROPERTIES {
                                    extendModeX: D2D1_EXTEND_MODE_CLAMP,
                                    extendModeY: D2D1_EXTEND_MODE_CLAMP,
                                    interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                };
                                let brush_props = D2D1_BRUSH_PROPERTIES {
                                    opacity: 1.0,
                                    transform: Matrix3x2 {
                                        M11: s,
                                        M12: 0.0,
                                        M21: 0.0,
                                        M22: s,
                                        M31: tx,
                                        M32: ty,
                                    },
                                };
                                if let Ok(bmp_brush) = unsafe {
                                    rt.CreateBitmapBrush(&bmp, Some(&bmp_props), Some(&brush_props))
                                } {
                                    let b: ID2D1BitmapBrush = bmp_brush;
                                    unsafe {
                                        rt.FillRoundedRectangle(&slot_rr, &b);
                                    }
                                    drew_thumb = true;
                                }
                            }
                        }

                        if !drew_thumb {
                            let gw: Vec<u16> = "\u{E8B9}".encode_utf16().collect();
                            unsafe {
                                rt.FillRoundedRectangle(&slot_rr, &b_tile);
                                rt.DrawText(
                                    &gw,
                                    &fmt_glyph,
                                    &slot_rect,
                                    &b_muted,
                                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                    DWRITE_MEASURING_MODE_NATURAL,
                                );
                            }
                        }

                        let base_dim = if self.wp.hovered_v == Some(v) {
                            0.18
                        } else {
                            0.42
                        };
                        let dim_alpha = base_dim * (1.0 - active_t);
                        if dim_alpha > 0.01 {
                            let dim_col = d2d_color(0.0, 0.0, 0.0, dim_alpha);
                            if let Ok(b_dim) = unsafe { rt.CreateSolidColorBrush(&dim_col, None) } {
                                unsafe {
                                    rt.FillRoundedRectangle(&slot_rr, &b_dim);
                                }
                            }
                        }

                        if active_t > 0.05 {
                            let stroke = 2.0 * scale;
                            let half = stroke * 0.5;
                            let border_rr = D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: slot_rect.left + half,
                                    top: slot_rect.top + half,
                                    right: slot_rect.right - half,
                                    bottom: slot_rect.bottom - half,
                                },
                                radiusX: (corner - half).max(1.0),
                                radiusY: (corner - half).max(1.0),
                            };
                            let acc = rgb_to_d2d(self.theme.accent_rgb, active_t);
                            if let Ok(b_acc) = unsafe { rt.CreateSolidColorBrush(&acc, None) } {
                                unsafe {
                                    rt.DrawRoundedRectangle(&border_rr, &b_acc, stroke, None);
                                }
                            }
                        }
                    }

                    let active_idx = self.wp.virtual_idx.rem_euclid(count as i64) as usize;
                    let title = if scan.initial_active == Some(active_idx) {
                        format!("{} · current wallpaper", scan.items[active_idx].name)
                    } else {
                        scan.items[active_idx].name.clone()
                    };
                    let hint = format!(
                        "{} / {}  ·  ←/→ browse  ·  Enter apply  ·  Esc back",
                        active_idx + 1,
                        count
                    );
                    let title_w: Vec<u16> = title.encode_utf16().collect();
                    let hint_w: Vec<u16> = hint.encode_utf16().collect();
                    let title_rect = D2D_RECT_F {
                        left: 20.0 * scale,
                        top: 203.0 * scale,
                        right: w - 20.0 * scale,
                        bottom: 223.0 * scale,
                    };
                    let hint_rect = D2D_RECT_F {
                        left: 20.0 * scale,
                        top: 223.0 * scale,
                        right: w - 20.0 * scale,
                        bottom: 242.0 * scale,
                    };
                    unsafe {
                        rt.DrawText(
                            &title_w,
                            &fmt_title_center,
                            &title_rect,
                            &b_text,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                        rt.DrawText(
                            &hint_w,
                            &fmt_sub_center,
                            &hint_rect,
                            &b_muted,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                }
            } else {
                let bar_h = SEARCH_BAR_H * scale;
                let mode = detect_mode(&self.query);
                let (bar_glyph, bar_brush) = match mode {
                    SearchMode::Apps => ("\u{E721}", &b_muted),
                    SearchMode::Actions => ("\u{E76C}", &b_accent),
                    SearchMode::Bangs => ("\u{E774}", &b_accent),
                };
                let bar_glyph_w: Vec<u16> = bar_glyph.encode_utf16().collect();
                let icon_rect = D2D_RECT_F {
                    left: 14.0 * scale,
                    top: 0.0,
                    right: 46.0 * scale,
                    bottom: bar_h,
                };
                unsafe {
                    rt.DrawText(
                        &bar_glyph_w,
                        &fmt_glyph,
                        &icon_rect,
                        bar_brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }

                let text_left = 50.0 * scale;
                let text_right = w - 20.0 * scale;
                let text_rect = D2D_RECT_F {
                    left: text_left,
                    top: 0.0,
                    right: text_right,
                    bottom: bar_h,
                };

                if self.query.is_empty() {
                    let ph: Vec<u16> = "Search apps, > commands, or ! shortcuts (!b, !yt, …)…"
                        .encode_utf16()
                        .collect();
                    unsafe {
                        rt.DrawText(
                            &ph,
                            &fmt_input,
                            &text_rect,
                            &b_muted,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                    // Caret at start
                    let caret = D2D_RECT_F {
                        left: text_left,
                        top: 15.0 * scale,
                        right: text_left + (1.5 * scale).max(1.0),
                        bottom: bar_h - 15.0 * scale,
                    };
                    unsafe {
                        rt.FillRectangle(&caret, &b_accent);
                    }
                } else {
                    let q_w: Vec<u16> = self.query.encode_utf16().collect();
                    let utf16_cursor = self.query[..self.cursor.min(self.query.len())]
                        .encode_utf16()
                        .count() as u32;
                    let mut caret_x = 0.0f32;
                    let mut caret_y = 0.0f32;
                    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                    if let Ok(layout) = unsafe {
                        self.d2d.dwrite_factory.CreateTextLayout(
                            &q_w,
                            &fmt_input,
                            (text_right - text_left).max(10.0),
                            bar_h,
                        )
                    } {
                        unsafe {
                            let _ = layout.HitTestTextPosition(
                                utf16_cursor,
                                false,
                                &mut caret_x,
                                &mut caret_y,
                                &mut metrics,
                            );
                        }
                    }
                    unsafe {
                        rt.DrawText(
                            &q_w,
                            &fmt_input,
                            &text_rect,
                            &b_text,
                            D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            DWRITE_MEASURING_MODE_NATURAL,
                        );
                    }
                    let cx = (text_left + caret_x).min(text_right - 2.0 * scale);
                    let caret = D2D_RECT_F {
                        left: cx,
                        top: 15.0 * scale,
                        right: cx + (1.5 * scale).max(1.0),
                        bottom: bar_h - 15.0 * scale,
                    };
                    unsafe {
                        rt.FillRectangle(&caret, &b_accent);
                    }
                }

                let geom = self.current_geometry();
                if geom.show_list {
                    let div_rect = D2D_RECT_F {
                        left: 0.0,
                        top: bar_h,
                        right: w,
                        bottom: bar_h + 1.0,
                    };
                    unsafe {
                        rt.FillRectangle(&div_rect, &b_div);
                    }

                    let list_top = bar_h + 1.0 + LIST_PAD_Y * scale;
                    let row_h = ROW_H * scale;
                    if self.results.is_empty() {
                        let empty_msg = if !self.apps_loaded && mode == SearchMode::Apps {
                            "Loading applications…"
                        } else {
                            "No matching results"
                        };
                        let ew: Vec<u16> = empty_msg.encode_utf16().collect();
                        let erect = D2D_RECT_F {
                            left: 22.0 * scale,
                            top: list_top,
                            right: w - 22.0 * scale,
                            bottom: list_top + row_h,
                        };
                        unsafe {
                            rt.DrawText(
                                &ew,
                                &fmt_title,
                                &erect,
                                &b_muted,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    } else {
                        let end = (self.scroll_offset + geom.visible_rows).min(self.results.len());
                        let items_slice = self.results[self.scroll_offset..end].to_vec();
                        for (slot, item) in items_slice.iter().enumerate() {
                            let idx = self.scroll_offset + slot;
                            let ry = list_top + (slot as f32) * row_h;
                            let row_rect = D2D_RECT_F {
                                left: 10.0 * scale,
                                top: ry + 2.0 * scale,
                                right: w - 14.0 * scale,
                                bottom: ry + row_h - 2.0 * scale,
                            };

                            if idx == self.selected {
                                let rr = D2D1_ROUNDED_RECT {
                                    rect: row_rect,
                                    radiusX: 6.0 * scale,
                                    radiusY: 6.0 * scale,
                                };
                                unsafe {
                                    rt.FillRoundedRectangle(&rr, &b_sel);
                                }
                                let pill = D2D1_ROUNDED_RECT {
                                    rect: D2D_RECT_F {
                                        left: 11.0 * scale,
                                        top: ry + 13.0 * scale,
                                        right: 14.0 * scale,
                                        bottom: ry + row_h - 13.0 * scale,
                                    },
                                    radiusX: 1.5 * scale,
                                    radiusY: 1.5 * scale,
                                };
                                unsafe {
                                    rt.FillRoundedRectangle(&pill, &b_accent);
                                }
                            }

                            let icon_size = 32.0 * scale;
                            let ix = 22.0 * scale;
                            let iy = ry + (row_h - icon_size) * 0.5;
                            let icon_dest = D2D_RECT_F {
                                left: ix,
                                top: iy,
                                right: ix + icon_size,
                                bottom: iy + icon_size,
                            };

                            let mut drew_bitmap = false;
                            if let Some(ref app_id) = item.app_id {
                                if let Some(px) = self.icons.get_or_request(app_id) {
                                    if let Some(bmp) = self.d2d.bitmap_for_app(&rt, app_id, &px) {
                                        unsafe {
                                            rt.DrawBitmap(
                                                &bmp,
                                                Some(&icon_dest),
                                                1.0,
                                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                                None,
                                            );
                                        }
                                        drew_bitmap = true;
                                    }
                                }
                            }

                            if !drew_bitmap {
                                let tile_rr = D2D1_ROUNDED_RECT {
                                    rect: icon_dest,
                                    radiusX: 6.0 * scale,
                                    radiusY: 6.0 * scale,
                                };
                                let tile_brush = if item.dangerous { &b_dbg } else { &b_tile };
                                let glyph_brush = if item.dangerous { &b_dfg } else { &b_text };
                                let is_app_letter = item.app_id.as_deref().is_some_and(|id| {
                                    !id.starts_with("ext:") && !id.starts_with("path:")
                                });
                                let glyph_fmt = if is_app_letter {
                                    &fmt_letter
                                } else {
                                    &fmt_glyph
                                };
                                let mut gbuf = [0u16; 2];
                                let gw = item.glyph.encode_utf16(&mut gbuf);
                                unsafe {
                                    rt.FillRoundedRectangle(&tile_rr, tile_brush);
                                    rt.DrawText(
                                        gw,
                                        glyph_fmt,
                                        &icon_dest,
                                        glyph_brush,
                                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                        DWRITE_MEASURING_MODE_NATURAL,
                                    );
                                }
                            }

                            let tx = ix + icon_size + 12.0 * scale;
                            let tr = w - 22.0 * scale;
                            let armed = self.confirm.armed_key() == Some(item.key.as_str());
                            let sub_text = if armed {
                                Some("Press Enter again to confirm")
                            } else {
                                item.subtitle.as_deref()
                            };
                            let title_w: Vec<u16> = item.title.encode_utf16().collect();

                            if let Some(sub) = sub_text {
                                let title_rect = D2D_RECT_F {
                                    left: tx,
                                    top: ry + 5.0 * scale,
                                    right: tr,
                                    bottom: ry + 26.0 * scale,
                                };
                                let sub_rect = D2D_RECT_F {
                                    left: tx,
                                    top: ry + 25.0 * scale,
                                    right: tr,
                                    bottom: ry + row_h - 4.0 * scale,
                                };
                                let sub_w: Vec<u16> = sub.encode_utf16().collect();
                                let sub_brush = if armed { &b_dfg } else { &b_muted };
                                unsafe {
                                    rt.DrawText(
                                        &title_w,
                                        &fmt_title,
                                        &title_rect,
                                        &b_text,
                                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                        DWRITE_MEASURING_MODE_NATURAL,
                                    );
                                    rt.DrawText(
                                        &sub_w,
                                        &fmt_sub,
                                        &sub_rect,
                                        sub_brush,
                                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                        DWRITE_MEASURING_MODE_NATURAL,
                                    );
                                }
                            } else {
                                let title_rect = D2D_RECT_F {
                                    left: tx,
                                    top: ry,
                                    right: tr,
                                    bottom: ry + row_h,
                                };
                                unsafe {
                                    rt.DrawText(
                                        &title_w,
                                        &fmt_title,
                                        &title_rect,
                                        &b_text,
                                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                        DWRITE_MEASURING_MODE_NATURAL,
                                    );
                                }
                            }
                        }

                        if self.results.len() > geom.visible_rows {
                            let track_h = geom.visible_rows as f32 * row_h;
                            let thumb_h = (track_h
                                * (geom.visible_rows as f32 / self.results.len() as f32))
                                .max(24.0 * scale);
                            let max_scroll = (self.results.len() - geom.visible_rows).max(1) as f32;
                            let thumb_y = list_top
                                + (track_h - thumb_h) * (self.scroll_offset as f32 / max_scroll);
                            let sb = D2D1_ROUNDED_RECT {
                                rect: D2D_RECT_F {
                                    left: w - 6.0 * scale,
                                    top: thumb_y,
                                    right: w - 3.0 * scale,
                                    bottom: thumb_y + thumb_h,
                                },
                                radiusX: 1.5 * scale,
                                radiusY: 1.5 * scale,
                            };
                            unsafe {
                                rt.FillRoundedRectangle(&sb, &b_div);
                            }
                        }
                    }
                }
            }
        }

        if unsafe { rt.EndDraw(None, None) }.is_err() {
            self.d2d.drop_target();
        }
    }
}

unsafe fn state_mut<'a>() -> Option<&'a mut AppState> {
    let ptr = *STATE_PTR.lock().unwrap();
    if ptr == 0 {
        None
    } else {
        Some(&mut *(ptr as *mut AppState))
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if let Some(st) = state_mut() {
                let width = (lparam.0 & 0xFFFF) as u32;
                let height = ((lparam.0 >> 16) & 0xFFFF) as u32;
                if width > 0 && height > 0 {
                    if let Some(ref rt) = st.d2d.target {
                        let _ = rt.Resize(&D2D_SIZE_U { width, height });
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(st) = state_mut() {
                st.paint();
            }
            let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_TOGGLE_ID {
                if let Some(st) = state_mut() {
                    st.toggle_launcher();
                }
            }
            LRESULT(0)
        }
        WM_APP_TOGGLE => {
            if let Some(st) = state_mut() {
                st.open_launcher(View::Search);
            }
            LRESULT(0)
        }
        WM_APP_APPS_LOADED => {
            if let Some(loaded) = PENDING_APPS.lock().unwrap().take() {
                if let Some(st) = state_mut() {
                    st.apps = loaded;
                    st.apps_loaded = true;
                    st.refresh_results();
                    let top_ids: Vec<String> = st
                        .results
                        .iter()
                        .filter_map(|r| r.app_id.clone())
                        .take(24)
                        .collect();
                    st.icons.prewarm(&top_ids);
                    if st.visible {
                        st.update_window_bounds();
                        st.invalidate();
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_ICON_READY | WM_APP_THUMB_READY => {
            if let Some(st) = state_mut() {
                if st.visible {
                    st.invalidate();
                }
            }
            LRESULT(0)
        }
        WM_APP_FILE_RESULTS => {
            if let Some(st) = state_mut() {
                st.refresh_results();
                let icon_ids: Vec<String> = st
                    .results
                    .iter()
                    .filter_map(|r| r.app_id.clone())
                    .take(16)
                    .collect();
                st.icons.prewarm(&icon_ids);
                if st.visible && st.view == View::Search {
                    st.update_window_bounds();
                    st.invalidate();
                }
            }
            LRESULT(0)
        }
        WM_APP_WALLPAPERS_LOADED => {
            if let Some(scan) = PENDING_WALLPAPERS.lock().unwrap().take() {
                if let Some(st) = state_mut() {
                    st.wp.loading = false;
                    let start = scan.initial_active.unwrap_or(0) as i64;
                    let has_items = !scan.items.is_empty();
                    st.wp.scan = Some(scan);
                    if has_items {
                        st.wp.virtual_idx = start;
                        st.wp.anim_current = start as f32;
                        st.wp.anim_start_val = start as f32;
                        st.wp.anim_target_val = start as f32;
                        st.wp.anim_active = false;
                        st.preload_carousel_thumbs();
                    }
                    if st.visible && st.view == View::Wallpaper {
                        st.update_window_bounds();
                        st.invalidate();
                    }
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_CAROUSEL_ANIM {
                if let Some(st) = state_mut() {
                    if st.wp.anim_active {
                        let elapsed = st.wp.anim_started_at.elapsed().as_secs_f32() * 1000.0;
                        let t = (elapsed / CAROUSEL_ANIM_MS).clamp(0.0, 1.0);
                        let ease = 1.0 - (1.0 - t).powi(3);
                        st.wp.anim_current = st.wp.anim_start_val
                            + (st.wp.anim_target_val - st.wp.anim_start_val) * ease;
                        if t >= 1.0 {
                            st.wp.anim_current = st.wp.anim_target_val;
                            st.wp.anim_active = false;
                            let _ = KillTimer(Some(hwnd), TIMER_CAROUSEL_ANIM);
                        }
                        st.invalidate();
                    } else {
                        let _ = KillTimer(Some(hwnd), TIMER_CAROUSEL_ANIM);
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_NIP_EVENT => {
            if let Some(st) = state_mut() {
                st.handle_nip_events();
            }
            LRESULT(0)
        }
        WM_APP_TRAY_ACTION => {
            if let Some(st) = state_mut() {
                match wparam.0 {
                    TRAY_ACT_TOGGLE => st.open_launcher(View::Search),
                    TRAY_ACT_WALLPAPER => {
                        st.open_wallpaper_view(false);
                    }
                    TRAY_ACT_RELOAD_APPS => spawn_apps_loader(st.hwnd),
                    TRAY_ACT_AUTOSTART => {
                        let next = !crate::actions::autostart_enabled();
                        let _ = set_autostart(next);
                        st.cfg.autostart = next;
                        if let Some(ref nip) = st.nip {
                            nip.sync_tray_state();
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_APP_QUIT => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            if let Some(st) = state_mut() {
                st.theme = load_theme().resolve();
                apply_dwm_style(hwnd, st.theme);
                if st.visible {
                    st.invalidate();
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let active = (wparam.0 & 0xFFFF) as u32;
            if active == WA_INACTIVE {
                if let Some(st) = state_mut() {
                    if st.visible && st.opened_at.elapsed().as_millis() > 180 {
                        st.hide_launcher();
                    }
                }
            }
            LRESULT(0)
        }
        WM_SYSCHAR => LRESULT(0),
        WM_SYSKEYDOWN | WM_SYSKEYUP => {
            if wparam.0 as u16 == VK_MENU.0 {
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            if let Some(st) = state_mut() {
                let vk = wparam.0 as u16;
                let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                let _ = shift;

                if st.view == View::Wallpaper {
                    let count = st.wp.scan.as_ref().map(|s| s.items.len()).unwrap_or(0);
                    let radius = carousel_radius_for(count).max(1) as i64;
                    match vk {
                        x if x == VK_ESCAPE.0 => {
                            st.wp.anim_active = false;
                            let _ = KillTimer(Some(hwnd), TIMER_CAROUSEL_ANIM);
                            if st.wp.from_search {
                                st.view = View::Search;
                                st.update_window_bounds();
                                st.invalidate();
                            } else {
                                st.hide_launcher();
                            }
                        }
                        x if x == VK_LEFT.0 => st.step_carousel(-1),
                        x if x == VK_RIGHT.0 => st.step_carousel(1),
                        x if x == VK_PRIOR.0 => st.step_carousel(-radius),
                        x if x == VK_NEXT.0 => st.step_carousel(radius),
                        x if x == VK_HOME.0 => st.jump_carousel(0),
                        x if x == VK_END.0 => {
                            if count > 0 {
                                st.jump_carousel((count - 1) as i64);
                            }
                        }
                        x if x == VK_RETURN.0 => st.apply_active_wallpaper(),
                        _ => {}
                    }
                    return LRESULT(0);
                }

                match vk {
                    x if x == VK_ESCAPE.0 => st.hide_launcher(),
                    x if x == VK_RETURN.0 => {
                        if st.view == View::Search {
                            st.execute_selected(ctrl);
                        }
                    }
                    x if x == VK_TAB.0 => {
                        if st.view == View::Search {
                            if let Some(ItemEffect::FillQuery { text }) =
                                st.results.get(st.selected).map(|i| i.effect.clone())
                            {
                                st.query = text;
                                st.cursor = st.query.len();
                                st.selected = 0;
                                st.scroll_offset = 0;
                                st.refresh_results();
                                st.update_window_bounds();
                                st.invalidate();
                            }
                        }
                    }
                    x if x == VK_UP.0 => st.move_selection(-1),
                    x if x == VK_DOWN.0 => st.move_selection(1),
                    x if x == VK_PRIOR.0 => st.move_selection(-(st.cfg.max_visible_rows as isize)),
                    x if x == VK_NEXT.0 => st.move_selection(st.cfg.max_visible_rows as isize),
                    x if x == VK_BACK.0 => st.handle_backspace(ctrl),
                    x if x == VK_DELETE.0 => st.handle_delete(),
                    x if x == VK_HOME.0 => {
                        st.cursor = 0;
                        st.invalidate();
                    }
                    x if x == VK_END.0 => {
                        st.cursor = st.query.len();
                        st.invalidate();
                    }
                    x if x == VK_LEFT.0 => {
                        if st.cursor > 0 {
                            if let Some((idx, _)) = st.query[..st.cursor].char_indices().next_back()
                            {
                                st.cursor = idx;
                                st.invalidate();
                            }
                        }
                    }
                    x if x == VK_RIGHT.0 => {
                        if st.cursor < st.query.len() {
                            if let Some(ch) = st.query[st.cursor..].chars().next() {
                                st.cursor += ch.len_utf8();
                                st.invalidate();
                            }
                        }
                    }
                    x if ctrl && x == b'A' as u16 => {
                        st.query.clear();
                        st.cursor = 0;
                        st.selected = 0;
                        st.scroll_offset = 0;
                        st.refresh_results();
                        st.update_window_bounds();
                        st.invalidate();
                    }
                    x if ctrl && x == b'V' as u16 => {
                        if let Some(clip) = read_clipboard_text(hwnd) {
                            st.insert_text(&clip);
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(st) = state_mut() {
                if let Some(ch) = char::from_u32(wparam.0 as u32) {
                    if !ch.is_control() {
                        let mut buf = [0u8; 4];
                        let s = ch.encode_utf8(&mut buf);
                        st.insert_text(s);
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(st) = state_mut() {
                let x = (lparam.0 & 0xFFFF) as i16 as f32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
                if st.view == View::Wallpaper {
                    let hit = st.hit_test_carousel(x, y);
                    if hit != st.wp.hovered_v {
                        st.wp.hovered_v = hit;
                        st.invalidate();
                    }
                } else if st.view == View::Search && !st.results.is_empty() {
                    let scale = st.dpi_scale();
                    let list_top = SEARCH_BAR_H * scale + 1.0 + LIST_PAD_Y * scale;
                    if y >= list_top {
                        let slot = ((y - list_top) / (ROW_H * scale)) as usize;
                        let idx = st.scroll_offset + slot;
                        if idx < st.results.len() && idx != st.selected {
                            st.selected = idx;
                            st.confirm.clear();
                            st.invalidate();
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(st) = state_mut() {
                let x = (lparam.0 & 0xFFFF) as i16 as f32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
                if st.view == View::Wallpaper {
                    if let Some(v) = st.hit_test_carousel(x, y) {
                        if v == st.wp.virtual_idx {
                            st.apply_active_wallpaper();
                        } else {
                            st.jump_carousel(v);
                        }
                    }
                } else if st.view == View::Search && !st.results.is_empty() {
                    let scale = st.dpi_scale();
                    let list_top = SEARCH_BAR_H * scale + 1.0 + LIST_PAD_Y * scale;
                    if y >= list_top {
                        let slot = ((y - list_top) / (ROW_H * scale)) as usize;
                        let idx = st.scroll_offset + slot;
                        if idx < st.results.len() {
                            st.selected = idx;
                            let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                            st.execute_selected(ctrl);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(st) = state_mut() {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
                if st.view == View::Wallpaper {
                    if st.wp.last_wheel_at.elapsed().as_millis() >= WHEEL_COOLDOWN_MS {
                        st.wp.last_wheel_at = Instant::now();
                        if delta > 0 {
                            st.step_carousel(-1);
                        } else if delta < 0 {
                            st.step_carousel(1);
                        }
                    }
                } else if delta > 0 {
                    st.move_selection(-1);
                } else if delta < 0 {
                    st.move_selection(1);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn run() -> Result<(), String> {
    enable_native_dark_menus();

    let hinst: HINSTANCE = unsafe {
        GetModuleHandleW(None)
            .map_err(|e| format!("GetModuleHandleW failed: {e}"))?
            .into()
    };
    let class_name = w!("NorenLauncherWnd");
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err("RegisterClassW failed".to_string());
        }
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class_name,
            w!("Noren Launcher"),
            WS_POPUP,
            0,
            0,
            640,
            56,
            None,
            None,
            Some(hinst),
            None,
        )
        .map_err(|e| format!("CreateWindowExW failed: {e}"))?
    };

    let manifest = build_launcher_manifest();
    if let Err(e) = noren_sdk::storage::install_manifest(&manifest) {
        eprintln!("noren-launcher: failed to install manifest: {e}");
    }
    let cfg = LauncherConfig::load_or_default(&manifest);
    let theme = load_theme().resolve();
    unsafe {
        apply_dwm_style(hwnd, theme);
    }

    let icons = IconCache::new(WM_APP_ICON_READY);
    icons.set_notify_hwnd(hwnd);
    let thumbs = ThumbCache::new(WM_APP_THUMB_READY);
    thumbs.set_notify_hwnd(hwnd);
    let files = FileSearchEngine::new(WM_APP_FILE_RESULTS);
    files.set_notify_hwnd(hwnd);

    let nip = LauncherNipRuntime::start(hwnd.0 as isize).ok();
    let remote_actions = nip
        .as_ref()
        .map(|n| n.remote_actions())
        .unwrap_or_default();

    let d2d = D2dResources::new()?;
    let mut state = Box::new(AppState {
        hwnd,
        cfg,
        theme,
        apps: Vec::new(),
        apps_loaded: false,
        frecency: FrecencyStore::load(),
        icons,
        thumbs,
        files,
        wp: WallpaperState::default(),
        nip,
        remote_actions,
        view: View::Search,
        query: String::new(),
        cursor: 0,
        selected: 0,
        scroll_offset: 0,
        results: Vec::new(),
        confirm: ConfirmationState::default(),
        visible: false,
        opened_at: Instant::now(),
        hotkey_registered: false,
        d2d,
    });
    state.register_configured_hotkey();
    state.refresh_results();
    state.update_window_bounds();

    let raw_ptr = Box::into_raw(state);
    *STATE_PTR.lock().unwrap() = raw_ptr as usize;

    spawn_apps_loader(hwnd);

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    *STATE_PTR.lock().unwrap() = 0;
    let mut boxed = unsafe { Box::from_raw(raw_ptr) };
    if boxed.hotkey_registered {
        unsafe {
            let _ = UnregisterHotKey(Some(boxed.hwnd), HOTKEY_TOGGLE_ID);
        }
    }
    if let Some(mut nip) = boxed.nip.take() {
        nip.shutdown();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_geometry_positions_at_25_percent_and_scales_rows() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let geom = compute_panel_geometry(work, 1.0, 640.0, 8, false, "", 12, View::Search, 0);
        assert_eq!(geom.x, 640);
        assert_eq!(geom.y, 260);
        assert_eq!(geom.w, 640);
        assert!(geom.show_list);
        assert_eq!(geom.visible_rows, 8);
        assert_eq!(geom.h, 56 + 1 + 16 + 8 * 50);

        let compact = compute_panel_geometry(work, 1.0, 640.0, 8, true, "", 12, View::Search, 0);
        assert!(!compact.show_list);
        assert_eq!(compact.h, 56);

        let wp_geom =
            compute_panel_geometry(work, 1.0, 640.0, 8, false, ">wallpaper", 1, View::Wallpaper, 12);
        assert!(!wp_geom.show_list);
        assert_eq!(wp_geom.w, 1024);
        assert_eq!(wp_geom.h, 248);
        assert_eq!(wp_geom.y, 260);
    }

    #[test]
    fn carousel_layout_morphs_smoothly_and_adapts_width() {
        assert_eq!(carousel_radius_for(1), 0);
        assert_eq!(carousel_radius_for(3), 1);
        assert_eq!(carousel_radius_for(20), 5);
        assert_eq!(carousel_panel_width(1), 384.0);
        assert_eq!(carousel_panel_width(11), 1024.0);

        let (cx0, w0, t0) = carousel_slot_layout(0.0);
        assert_eq!(cx0, 0.0);
        assert_eq!(w0, CAROUSEL_CENTER_W);
        assert_eq!(t0, 1.0);

        let (cx1, w1, t1) = carousel_slot_layout(1.0);
        assert_eq!(cx1, 196.0);
        assert_eq!(w1, CAROUSEL_STRIP_W);
        assert_eq!(t1, 0.0);

        let (cxm, wm, tm) = carousel_slot_layout(0.5);
        assert_eq!(cxm, 98.0);
        assert_eq!(wm, 188.0);
        assert_eq!(tm, 0.5);
    }
}
