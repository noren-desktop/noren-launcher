# noren-launcher

Native Win32 + Direct2D keyboard-driven launcher module for **Noren**.

## Features
- **Application Search:** Enumerates Start Menu apps via COM `shell:AppsFolder` with background `64x64` shell icon caching and frecency ranking (`%LOCALAPPDATA%\Noren\launcher\frecency.json`).
- **Unified `!` Shortcuts:** Type `!` to list or trigger browser/web shortcuts (`!b`, `!yt`, `!gh`, `!ddg`, `!g`, `!wiki`, `!bing`, `!brave`), with `Tab` / `Enter` autocomplete, direct URL detection for `!b`, and customizable shortcuts in Control Center (`>settings` → **Launcher**).
- **`>` Commands:** Built-in `wallpaper`, `settings`, `reload_apps`, `lock`, `sleep`, `hibernate`, `display_off`, plus `logout`, `restart`, and `shutdown` (double-`Enter` confirmation), and live actions from connected Noren modules (`>overview`, `>cheatsheet` when `noren-wm` is running).
- **Wallpaper Carousel (`>wallpaper`):** ORUMI-style horizontal strip carousel rendered in Direct2D with asynchronous WIC `400x225` disk thumbnail caching (supports Built-in or External Command mode).
- **Unified System Tray & Control Center:** Participates in `noren-sdk` Tray Host election (`tray.actions`) and launches Noren Control Center via `--control-center`.

## Configuration
- Config file: `%APPDATA%\Noren\launcher\config.toml`
- Default toggle hotkey: `Alt+Space`

## CLI Flags
- `--control-center [--tab <id>]` — open Noren Control Center
- `--nip-self-test` — run isolated NIP & Unified Tray self-test
