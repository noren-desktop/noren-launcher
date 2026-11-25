# Changelog

## v0.1.0 — Phase 3: Launcher

- Native Win32 + Direct2D / DirectWrite / DWM Mica & Acrylic launcher window positioned at 25% from the work area top edge (`Alt+Space` default).
- Start Menu application search (`shell:AppsFolder`), background shell icon extraction (`64x64` PBGRA disk cache), frecency ranking, and Compact Mode toggle.
- Unified `!` shortcuts (`!b`, `!yt`, `!gh`, `!ddg`, `!g`, `!wiki`, `!bing`, `!brave`) customizable via `FieldKind::ShortcutList` in Noren Control Center.
- Built-in `>` system actions (`wallpaper`, `settings`, `reload_apps`, `lock`, `sleep`, `hibernate`, `display_off`, `logout`, `restart`, `shutdown` with double-Enter confirmation) and live NIP `launcher.actions` integration (`>overview`, `>cheatsheet` from `noren-wm`).
- ORUMI-style horizontal strip Wallpaper Carousel (`>wallpaper`) with asynchronous WIC `400x225` disk thumbnail caching and Built-in / External Command modes.
- Integrated `noren-sdk` Unified System Tray (`tray.actions`) and `--nip-self-test`.
