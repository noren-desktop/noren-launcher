use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::hash::{Hash, Hasher};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, RGBQUAD,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_BIGGERSIZEOK,
    SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const ICON_DIM: u32 = 64;
pub const ICON_BYTES: usize = (ICON_DIM * ICON_DIM * 4) as usize;

const CACHE_MAGIC: &[u8; 4] = b"NICO";
const CACHE_VERSION: u16 = 1;
const HEADER_LEN: usize = 8;
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn icon_cache_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local)
                .join("Noren")
                .join("launcher")
                .join("icons");
        }
    }
    std::env::temp_dir().join("noren-launcher-icons")
}

fn hash_app_id(app_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    (CACHE_VERSION, ICON_DIM, app_id).hash(&mut hasher);
    hasher.finish()
}

fn cache_file_path(dir: &Path, app_id: &str) -> PathBuf {
    dir.join(format!("{:016x}.bgra", hash_app_id(app_id)))
}

fn read_cached_icon(path: &Path) -> Option<Arc<[u8; ICON_BYTES]>> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() != HEADER_LEN + ICON_BYTES {
        return None;
    }
    if &bytes[0..4] != CACHE_MAGIC {
        return None;
    }
    let ver = u16::from_le_bytes([bytes[4], bytes[5]]);
    let dim = u16::from_le_bytes([bytes[6], bytes[7]]);
    if ver != CACHE_VERSION || dim != ICON_DIM as u16 {
        return None;
    }
    let mut arr = [0u8; ICON_BYTES];
    arr.copy_from_slice(&bytes[HEADER_LEN..]);
    Some(Arc::new(arr))
}

fn write_cached_icon(path: &Path, pixels: &[u8; ICON_BYTES]) {
    let mut buf = Vec::with_capacity(HEADER_LEN + ICON_BYTES);
    buf.extend_from_slice(CACHE_MAGIC);
    buf.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    buf.extend_from_slice(&(ICON_DIM as u16).to_le_bytes());
    buf.extend_from_slice(pixels);

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{seq}.tmp"));
    if fs::write(&tmp, &buf).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

unsafe fn read_hbitmap_pbgra(hbitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut bm: BITMAP = std::mem::zeroed();
    let got = GetObjectW(
        hbitmap.into(),
        size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut BITMAP as *mut c_void),
    );
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 || bm.bmWidth > 512 || bm.bmHeight > 512 {
        return None;
    }
    let w = bm.bmWidth as u32;
    let h = bm.bmHeight as u32;

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD::default(); 1],
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    let hdc = GetDC(None);
    let rows = GetDIBits(
        hdc,
        hbitmap,
        0,
        h,
        Some(px.as_mut_ptr() as *mut c_void),
        &mut bmi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(None, hdc);
    if rows == 0 {
        return None;
    }

    let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
    for p in px.chunks_exact_mut(4) {
        if !has_alpha {
            p[3] = 255;
        } else {
            let a = p[3];
            p[0] = p[0].min(a);
            p[1] = p[1].min(a);
            p[2] = p[2].min(a);
        }
    }
    Some((w, h, px))
}

fn resample_pbgra_to_64(src_w: u32, src_h: u32, src: &[u8]) -> [u8; ICON_BYTES] {
    if src_w == ICON_DIM && src_h == ICON_DIM && src.len() == ICON_BYTES {
        let mut out = [0u8; ICON_BYTES];
        out.copy_from_slice(src);
        return out;
    }
    let mut out = [0u8; ICON_BYTES];
    let max_x = (src_w.saturating_sub(1)) as f32;
    let max_y = (src_h.saturating_sub(1)) as f32;

    for dy in 0..ICON_DIM {
        let sy = if ICON_DIM > 1 {
            (dy as f32) * max_y / ((ICON_DIM - 1) as f32)
        } else {
            0.0
        };
        let y0 = sy.floor() as u32;
        let y1 = (y0 + 1).min(src_h - 1);
        let fy = sy - y0 as f32;

        for dx in 0..ICON_DIM {
            let sx = if ICON_DIM > 1 {
                (dx as f32) * max_x / ((ICON_DIM - 1) as f32)
            } else {
                0.0
            };
            let x0 = sx.floor() as u32;
            let x1 = (x0 + 1).min(src_w - 1);
            let fx = sx - x0 as f32;

            let i00 = ((y0 * src_w + x0) * 4) as usize;
            let i10 = ((y0 * src_w + x1) * 4) as usize;
            let i01 = ((y1 * src_w + x0) * 4) as usize;
            let i11 = ((y1 * src_w + x1) * 4) as usize;
            let dst = ((dy * ICON_DIM + dx) * 4) as usize;

            for c in 0..4 {
                let top = src[i00 + c] as f32 * (1.0 - fx) + src[i10 + c] as f32 * fx;
                let bot = src[i01 + c] as f32 * (1.0 - fx) + src[i11 + c] as f32 * fx;
                out[dst + c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

unsafe fn extract_shell_icon_64(app_id: &str) -> Option<Arc<[u8; ICON_BYTES]>> {
    let parsing = format!("shell:AppsFolder\\{app_id}");
    let wide: Vec<u16> = parsing.encode_utf16().chain(std::iter::once(0)).collect();
    let factory: IShellItemImageFactory =
        SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
    let size = SIZE {
        cx: ICON_DIM as i32,
        cy: ICON_DIM as i32,
    };
    let hbitmap = factory
        .GetImage(size, SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_BIGGERSIZEOK.0))
        .or_else(|_| factory.GetImage(size, SIIGBF_BIGGERSIZEOK))
        .ok()?;
    if hbitmap.0.is_null() {
        return None;
    }
    let res = read_hbitmap_pbgra(hbitmap).map(|(w, h, px)| Arc::new(resample_pbgra_to_64(w, h, &px)));
    let _ = DeleteObject(hbitmap.into());
    res
}

struct IconShared {
    cache: Mutex<HashMap<String, Option<Arc<[u8; ICON_BYTES]>>>>,
    pending: Mutex<HashSet<String>>,
    notify_hwnd: AtomicIsize,
    notify_msg: u32,
}

/// Asynchronous shell icon extractor with memory + disk (`64x64` PBGRA) cache.
pub struct IconCache {
    shared: Arc<IconShared>,
    tx: mpsc::Sender<String>,
}

impl IconCache {
    pub fn new(notify_msg: u32) -> Self {
        let dir = icon_cache_dir();
        let _ = fs::create_dir_all(&dir);
        let shared = Arc::new(IconShared {
            cache: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashSet::new()),
            notify_hwnd: AtomicIsize::new(0),
            notify_msg,
        });

        let (tx, rx) = mpsc::channel::<String>();
        let worker_shared = Arc::clone(&shared);
        thread::spawn(move || {
            let com_ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
            while let Ok(app_id) = rx.recv() {
                let path = cache_file_path(&dir, &app_id);
                let extracted = read_cached_icon(&path).or_else(|| {
                    let img = unsafe { extract_shell_icon_64(&app_id) };
                    if let Some(ref px) = img {
                        write_cached_icon(&path, px);
                    }
                    img
                });
                let has_image = extracted.is_some();
                worker_shared
                    .cache
                    .lock()
                    .unwrap()
                    .insert(app_id.clone(), extracted);
                worker_shared.pending.lock().unwrap().remove(&app_id);
                if has_image {
                    let raw = worker_shared.notify_hwnd.load(Ordering::Acquire);
                    if raw != 0 {
                        unsafe {
                            let _ = PostMessageW(
                                Some(HWND(raw as *mut c_void)),
                                worker_shared.notify_msg,
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                    }
                }
            }
            if com_ok {
                unsafe {
                    CoUninitialize();
                }
            }
        });

        Self { shared, tx }
    }

    pub fn set_notify_hwnd(&self, hwnd: HWND) {
        self.shared
            .notify_hwnd
            .store(hwnd.0 as isize, Ordering::Release);
    }

    pub fn get_or_request(&self, app_id: &str) -> Option<Arc<[u8; ICON_BYTES]>> {
        if let Some(entry) = self.shared.cache.lock().unwrap().get(app_id) {
            return entry.clone();
        }
        let mut pending = self.shared.pending.lock().unwrap();
        if pending.insert(app_id.to_string()) {
            let _ = self.tx.send(app_id.to_string());
        }
        None
    }

    pub fn prewarm(&self, app_ids: &[String]) {
        for id in app_ids.iter().take(24) {
            let _ = self.get_or_request(id);
        }
    }
}
