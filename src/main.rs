#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod apps;
mod config;
mod icons;
mod nip;
mod search;
mod ui;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

fn acquire_single_instance_or_wake() -> Option<windows::Win32::Foundation::HANDLE> {
    let sid = noren_sdk::security::current_user_sid().unwrap_or_else(|_| "default".to_string());
    let name = format!("Local\\noren-launcher-single-{sid}");
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }.ok()?;
    let err = unsafe { GetLastError() };
    if err == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
            if let Ok(hwnd) = FindWindowW(w!("NorenLauncherWnd"), None) {
                if hwnd != HWND::default() {
                    let _ = PostMessageW(Some(hwnd), ui::WM_APP_TOGGLE, WPARAM(0), LPARAM(0));
                }
            }
        }
        return None;
    }
    Some(handle)
}

fn main() {
    if std::env::args().any(|a| a == "--nip-self-test") {
        if let Err(e) = nip::run_nip_self_test() {
            eprintln!("LAUNCHER_NIP_SELF_TEST=FAIL ({e})");
            std::process::exit(1);
        }
        return;
    }

    if let Some(res) = noren_sdk::control_center::maybe_run_control_center_from_args() {
        if let Err(e) = res {
            eprintln!("noren-launcher control center error: {e}");
            std::process::exit(1);
        }
        return;
    }

    let Some(single_handle) = acquire_single_instance_or_wake() else {
        return;
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let res = ui::run();
    unsafe {
        let _ = CloseHandle(single_handle);
    }
    if let Err(e) = res {
        eprintln!("noren-launcher error: {e}");
        std::process::exit(1);
    }
}
