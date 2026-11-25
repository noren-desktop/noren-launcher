use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, HWND_BROADCAST, SW_SHOWNORMAL, WM_SYSCOMMAND,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SC_MONITORPOWER: usize = 0xF170;

const RUN_SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("noren-launcher");

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn spawn_hidden(program: &str, args: &[&str]) -> Result<(), String> {
    Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn run_power_action(id: &str) -> Result<(), String> {
    match id {
        "lock" => spawn_hidden("rundll32.exe", &["user32.dll,LockWorkStation"]),
        "sleep" => spawn_hidden(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; \
                 [System.Windows.Forms.Application]::SetSuspendState(\
                 [System.Windows.Forms.PowerState]::Suspend, $false, $false)",
            ],
        ),
        "hibernate" => spawn_hidden("shutdown.exe", &["/h"]),
        "shutdown" => spawn_hidden("shutdown.exe", &["/s", "/t", "0"]),
        "restart" => spawn_hidden("shutdown.exe", &["/r", "/t", "0"]),
        "logout" => spawn_hidden("shutdown.exe", &["/l"]),
        "display_off" => {
            thread::spawn(|| {
                thread::sleep(Duration::from_millis(600));
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND_BROADCAST),
                        WM_SYSCOMMAND,
                        WPARAM(SC_MONITORPOWER),
                        LPARAM(2),
                    );
                }
            });
            Ok(())
        }
        other => Err(format!("unknown power action '{other}'")),
    }
}

pub fn run_external_wallpaper_command(cmd: &str) -> Result<(), String> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Err("Wallpaper command is empty".to_string());
    }
    spawn_hidden("cmd.exe", &["/C", trimmed])
}

pub fn open_url_in_browser(url: &str) -> Result<(), String> {
    let wide = to_wide_null(url);
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        return Err(format!("ShellExecuteW failed opening {url}"));
    }
    Ok(())
}

pub fn open_file_or_folder(path: &str) -> Result<(), String> {
    let wide = to_wide_null(path);
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as isize) <= 32 {
        return Err(format!("ShellExecuteW failed opening {path}"));
    }
    Ok(())
}

pub fn reveal_in_explorer(path: &str) -> Result<(), String> {
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed launching explorer.exe: {e}"))
}

pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
        .is_ok()
    }
}

pub fn set_autostart(on: bool) -> bool {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else {
                return false;
            };
            let data = to_wide_null(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_SUBKEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(data.as_ptr() as *const c_void),
                (data.len() * std::mem::size_of::<u16>()) as u32,
            )
            .is_ok()
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_SUBKEY, RUN_VALUE).is_ok()
        }
    }
}
