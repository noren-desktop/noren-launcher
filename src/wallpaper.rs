use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::UNIX_EPOCH;

use noren_sdk::storage::atomic_write_text;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
    SPI_GETDESKWALLPAPER, SPI_SETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

pub const THUMB_W: u32 = 400;
pub const THUMB_H: u32 = 225;
pub const THUMB_BYTES: usize = (THUMB_W * THUMB_H * 4) as usize;

const MAX_ITEMS: usize = 400;
const MAX_FILE_BYTES: u64 = 80_000_000;
const MAX_MEM_THUMBS: usize = 32;

const CACHE_MAGIC: &[u8; 4] = b"NWTH";
const CACHE_VERSION: u16 = 1;
const HEADER_LEN: usize = 12;
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallpaperEntry {
    pub name: String,
    pub path: PathBuf,
    pub cache_key: u64,
}

#[derive(Clone, Debug)]
pub struct WallpaperScan {
    pub dir: PathBuf,
    pub items: Vec<WallpaperEntry>,
    pub initial_active: Option<usize>,
    pub error: Option<String>,
}

fn launcher_local_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if !local.is_empty() {
            return PathBuf::from(local).join("Noren").join("launcher");
        }
    }
    std::env::temp_dir().join("noren-launcher")
}

pub fn thumb_cache_dir() -> PathBuf {
    launcher_local_dir().join("thumbs")
}

fn last_wallpaper_path() -> PathBuf {
    launcher_local_dir().join("last_wallpaper.txt")
}

pub fn is_wallpaper_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "bmp" | "webp"
            )
        })
}

pub fn normalize_path_str(p: &str) -> String {
    let backslash = p.trim().replace('/', "\\");
    backslash
        .strip_prefix(r"\\?\")
        .unwrap_or(&backslash)
        .to_lowercase()
}

pub fn current_windows_wallpaper() -> Option<String> {
    let mut buf = [0u16; 520];
    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let path = String::from_utf16_lossy(&buf[..len]);
    (!path.trim().is_empty()).then_some(path)
}

fn read_last_applied_wallpaper() -> Option<String> {
    fs::read_to_string(last_wallpaper_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn resolve_wallpaper_dir(configured_dir: &str) -> Option<PathBuf> {
    let trimmed = configured_dir.trim();
    if !trimmed.is_empty() {
        return Some(PathBuf::from(trimmed));
    }
    unsafe {
        if let Ok(pwstr) = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) {
            let s = pwstr.to_string().ok();
            CoTaskMemFree(Some(pwstr.0.cast::<c_void>()));
            if let Some(path) = s {
                if !path.trim().is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Pictures"))
}

fn compute_cache_key(path: &Path, len: u64, modified_secs: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    let norm = normalize_path_str(&path.to_string_lossy());
    (CACHE_VERSION, THUMB_W, THUMB_H, norm, len, modified_secs).hash(&mut hasher);
    hasher.finish()
}

pub fn scan_wallpaper_folder(configured_dir: &str) -> WallpaperScan {
    scan_wallpaper_folder_with_active(
        configured_dir,
        current_windows_wallpaper().as_deref(),
        read_last_applied_wallpaper().as_deref(),
    )
}

pub fn scan_wallpaper_folder_with_active(
    configured_dir: &str,
    windows_active: Option<&str>,
    last_active: Option<&str>,
) -> WallpaperScan {
    let Some(dir) = resolve_wallpaper_dir(configured_dir) else {
        return WallpaperScan {
            dir: PathBuf::new(),
            items: Vec::new(),
            initial_active: None,
            error: Some("Pictures folder not found".to_string()),
        };
    };

    if !dir.is_absolute() {
        return WallpaperScan {
            dir,
            items: Vec::new(),
            initial_active: None,
            error: Some("Use a full path, e.g. D:\\Wallpapers".to_string()),
        };
    }

    let Ok(entries) = fs::read_dir(&dir) else {
        return WallpaperScan {
            dir,
            items: Vec::new(),
            initial_active: None,
            error: Some("Folder not found or cannot be read".to_string()),
        };
    };

    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_wallpaper_image(&path) {
            continue;
        }
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let len = meta.len();
        if len == 0 || len > MAX_FILE_BYTES {
            continue;
        }
        let secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cache_key = compute_cache_key(&path, len, secs);
        items.push(WallpaperEntry {
            name,
            path,
            cache_key,
        });
    }

    items.sort_by(|a, b| {
        a.path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .cmp(&b.path.file_name().map(|n| n.to_string_lossy().to_lowercase()))
    });
    items.truncate(MAX_ITEMS);

    let normalized: Vec<String> = items
        .iter()
        .map(|i| normalize_path_str(&i.path.to_string_lossy()))
        .collect();
    let find_idx = |candidate: &str| -> Option<usize> {
        let target = normalize_path_str(candidate);
        (!target.is_empty())
            .then(|| normalized.iter().position(|p| *p == target))
            .flatten()
    };

    let initial_active = windows_active
        .and_then(find_idx)
        .or_else(|| last_active.and_then(find_idx));

    WallpaperScan {
        dir,
        items,
        initial_active,
        error: None,
    }
}

pub fn apply_wallpaper(path: &Path) -> Result<(), String> {
    if !path.is_file() || !is_wallpaper_image(path) {
        return Err("Invalid wallpaper file".to_string());
    }
    let path_str = path.to_string_lossy().into_owned();
    let mut wide: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(wide.as_mut_ptr().cast::<c_void>()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
        .map_err(|e| e.to_string())?;
    }
    let _ = atomic_write_text(&last_wallpaper_path(), &path_str);
    Ok(())
}

fn cache_file_path(dir: &Path, cache_key: u64) -> PathBuf {
    dir.join(format!("{cache_key:016x}.bgra"))
}

pub fn read_cached_thumb(path: &Path) -> Option<Arc<[u8; THUMB_BYTES]>> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() != HEADER_LEN + THUMB_BYTES || &bytes[0..4] != CACHE_MAGIC {
        return None;
    }
    let ver = u16::from_le_bytes([bytes[4], bytes[5]]);
    let w = u16::from_le_bytes([bytes[6], bytes[7]]);
    let h = u16::from_le_bytes([bytes[8], bytes[9]]);
    if ver != CACHE_VERSION || w != THUMB_W as u16 || h != THUMB_H as u16 {
        return None;
    }
    let boxed: Box<[u8; THUMB_BYTES]> = bytes[HEADER_LEN..].to_vec().into_boxed_slice().try_into().ok()?;
    Some(Arc::from(boxed))
}

pub fn write_cached_thumb(path: &Path, pixels: &[u8; THUMB_BYTES]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + THUMB_BYTES);
    buf.extend_from_slice(CACHE_MAGIC);
    buf.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    buf.extend_from_slice(&(THUMB_W as u16).to_le_bytes());
    buf.extend_from_slice(&(THUMB_H as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(pixels);

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{seq}.tmp"));
    if fs::write(&tmp, &buf).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

pub unsafe fn decode_wallpaper_thumb_wic(
    factory: &IWICImagingFactory,
    path: &Path,
) -> Option<Arc<[u8; THUMB_BYTES]>> {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let decoder = factory
        .CreateDecoderFromFilename(
            PCWSTR(wide.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
        .ok()?;
    let frame = decoder.GetFrame(0).ok()?;
    let mut src_w = 0u32;
    let mut src_h = 0u32;
    frame.GetSize(&mut src_w, &mut src_h).ok()?;
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let scale = (THUMB_W as f32 / src_w as f32).max(THUMB_H as f32 / src_h as f32);
    let scaled_w = ((src_w as f32 * scale).round() as u32).max(THUMB_W);
    let scaled_h = ((src_h as f32 * scale).round() as u32).max(THUMB_H);

    let scaler = factory.CreateBitmapScaler().ok()?;
    scaler
        .Initialize(&frame, scaled_w, scaled_h, WICBitmapInterpolationModeFant)
        .ok()?;

    let converter = factory.CreateFormatConverter().ok()?;
    converter
        .Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .ok()?;

    let crop = WICRect {
        X: ((scaled_w - THUMB_W) / 2) as i32,
        Y: ((scaled_h - THUMB_H) / 2) as i32,
        Width: THUMB_W as i32,
        Height: THUMB_H as i32,
    };
    let mut arr: Box<[u8; THUMB_BYTES]> = vec![0u8; THUMB_BYTES]
        .into_boxed_slice()
        .try_into()
        .ok()?;
    converter
        .CopyPixels(&crop, THUMB_W * 4, &mut arr[..])
        .ok()?;
    Some(Arc::from(arr))
}

pub fn load_or_decode_thumb(
    cache_dir: &Path,
    factory: &IWICImagingFactory,
    cache_key: u64,
    path: &Path,
) -> Option<Arc<[u8; THUMB_BYTES]>> {
    let target = cache_file_path(cache_dir, cache_key);
    if let Some(cached) = read_cached_thumb(&target) {
        return Some(cached);
    }
    let decoded = unsafe { decode_wallpaper_thumb_wic(factory, path) }?;
    write_cached_thumb(&target, &decoded);
    Some(decoded)
}

struct ThumbMemState {
    cache: HashMap<u64, (u64, Option<Arc<[u8; THUMB_BYTES]>>)>,
    lru_seq: u64,
    pending: HashSet<u64>,
    queue: Vec<(u64, PathBuf)>,
}

struct ThumbShared {
    state: Mutex<ThumbMemState>,
    cv: Condvar,
    notify_hwnd: AtomicIsize,
    notify_msg: u32,
}

/// Bounded asynchronous WIC thumbnail cache with LIFO priority for visible carousel slots.
pub struct ThumbCache {
    shared: Arc<ThumbShared>,
}

impl ThumbCache {
    pub fn new(notify_msg: u32) -> Self {
        let dir = thumb_cache_dir();
        let _ = fs::create_dir_all(&dir);

        let shared = Arc::new(ThumbShared {
            state: Mutex::new(ThumbMemState {
                cache: HashMap::new(),
                lru_seq: 1,
                pending: HashSet::new(),
                queue: Vec::new(),
            }),
            cv: Condvar::new(),
            notify_hwnd: AtomicIsize::new(0),
            notify_msg,
        });

        let worker_shared = Arc::clone(&shared);
        thread::spawn(move || {
            struct ComGuard(bool);
            impl Drop for ComGuard {
                fn drop(&mut self) {
                    if self.0 {
                        unsafe {
                            CoUninitialize();
                        }
                    }
                }
            }
            let _com = ComGuard(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() });
            let factory: Option<IWICImagingFactory> = unsafe {
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()
            };

            loop {
                let job = {
                    let mut st = worker_shared.state.lock().unwrap();
                    while st.queue.is_empty() {
                        st = worker_shared.cv.wait(st).unwrap();
                    }
                    st.queue.pop()
                };
                let Some((cache_key, path)) = job else {
                    continue;
                };

                let decoded = factory
                    .as_ref()
                    .and_then(|f| load_or_decode_thumb(&dir, f, cache_key, &path));
                let has_img = decoded.is_some();

                {
                    let mut st = worker_shared.state.lock().unwrap();
                    st.pending.remove(&cache_key);
                    st.lru_seq = st.lru_seq.wrapping_add(1);
                    let seq = st.lru_seq;
                    st.cache.insert(cache_key, (seq, decoded));

                    // Enforce bounded in-memory budget (MAX_MEM_THUMBS decoded thumbnails).
                    let loaded_count = st.cache.values().filter(|(_, v)| v.is_some()).count();
                    if loaded_count > MAX_MEM_THUMBS {
                        if let Some((oldest_key, _)) = st
                            .cache
                            .iter()
                            .filter(|(_, (_, v))| v.is_some())
                            .min_by_key(|(_, (s, _))| *s)
                            .map(|(k, v)| (*k, v.0))
                        {
                            st.cache.remove(&oldest_key);
                        }
                    }
                }

                if has_img {
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
        });

        Self { shared }
    }

    pub fn set_notify_hwnd(&self, hwnd: HWND) {
        self.shared
            .notify_hwnd
            .store(hwnd.0 as isize, Ordering::Release);
    }

    pub fn get_or_request(&self, entry: &WallpaperEntry) -> Option<Arc<[u8; THUMB_BYTES]>> {
        let mut st = self.shared.state.lock().unwrap();
        st.lru_seq = st.lru_seq.wrapping_add(1);
        let seq = st.lru_seq;
        if let Some((entry_seq, val)) = st.cache.get_mut(&entry.cache_key) {
            *entry_seq = seq;
            return val.clone();
        }
        if st.pending.insert(entry.cache_key) {
            st.queue.push((entry.cache_key, entry.path.clone()));
            self.shared.cv.notify_one();
        }
        None
    }

    pub fn request_only(&self, entry: &WallpaperEntry) {
        let mut st = self.shared.state.lock().unwrap();
        if st.cache.contains_key(&entry.cache_key) {
            return;
        }
        if st.pending.insert(entry.cache_key) {
            st.queue.push((entry.cache_key, entry.path.clone()));
            self.shared.cv.notify_one();
        }
    }

    pub fn clear_memory(&self) {
        let mut st = self.shared.state.lock().unwrap();
        st.cache.clear();
        st.pending.clear();
        st.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_test_bmp(path: &Path, width: u32, height: u32, rgb: (u8, u8, u8)) {
        let row_bytes = ((width * 3 + 3) / 4) * 4;
        let pixel_size = row_bytes * height;
        let file_size = 54 + pixel_size;
        let mut data = vec![0u8; file_size as usize];

        // BITMAPFILEHEADER
        data[0] = b'B';
        data[1] = b'M';
        data[2..6].copy_from_slice(&file_size.to_le_bytes());
        data[10..14].copy_from_slice(&54u32.to_le_bytes());

        // BITMAPINFOHEADER
        data[14..18].copy_from_slice(&40u32.to_le_bytes());
        data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        data[22..26].copy_from_slice(&(height as i32).to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[28..30].copy_from_slice(&24u16.to_le_bytes());
        data[34..38].copy_from_slice(&pixel_size.to_le_bytes());

        for y in 0..height {
            for x in 0..width {
                let off = (54 + y * row_bytes + x * 3) as usize;
                data[off] = rgb.2;
                data[off + 1] = rgb.1;
                data[off + 2] = rgb.0;
            }
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn scan_and_active_matching_works() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("noren_wp_scan_test_{nanos}"));
        fs::create_dir_all(&dir).unwrap();

        let img_a = dir.join("alpha.jpg");
        let img_b = dir.join("beta.png");
        let txt = dir.join("readme.txt");
        fs::write(&img_a, b"jpgdata").unwrap();
        fs::write(&img_b, b"pngdata").unwrap();
        fs::write(&txt, b"ignore").unwrap();

        assert!(is_wallpaper_image(&img_a));
        assert!(is_wallpaper_image(&img_b));
        assert!(!is_wallpaper_image(&txt));

        let scan = scan_wallpaper_folder_with_active(
            &dir.to_string_lossy(),
            Some(&format!("\\\\?\\{}", img_b.display()).replace('\\', "/")),
            None,
        );
        assert!(scan.error.is_none());
        assert_eq!(scan.items.len(), 2);
        assert_eq!(scan.items[0].name, "alpha");
        assert_eq!(scan.items[1].name, "beta");
        assert_eq!(scan.initial_active, Some(1));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wic_decode_and_disk_cache_roundtrip() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("noren_wp_wic_test_{nanos}"));
        let cache_dir = dir.join("cache");
        fs::create_dir_all(&cache_dir).unwrap();

        let bmp = dir.join("solid_blue.bmp");
        write_test_bmp(&bmp, 64, 32, (32, 128, 220));

        let scan = scan_wallpaper_folder_with_active(&dir.to_string_lossy(), None, None);
        assert_eq!(scan.items.len(), 1);
        let entry = &scan.items[0];

        let com_ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        {
            let factory: IWICImagingFactory = unsafe {
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap()
            };

            let first = load_or_decode_thumb(&cache_dir, &factory, entry.cache_key, &entry.path)
                .expect("WIC decode should succeed");
            assert_eq!(first.len(), THUMB_BYTES);
            // PBGRA pixel sample at center: B~220, G~128, R~32, A=255
            let mid = ((THUMB_H / 2 * THUMB_W + THUMB_W / 2) * 4) as usize;
            assert_eq!(first[mid + 3], 255);
            assert!(first[mid] > 200);

            let cache_file = cache_file_path(&cache_dir, entry.cache_key);
            assert!(cache_file.exists());
            let reloaded = read_cached_thumb(&cache_file).expect("cache reload should succeed");
            assert_eq!(&first[..], &reloaded[..]);
        }

        if com_ok {
            unsafe {
                CoUninitialize();
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
