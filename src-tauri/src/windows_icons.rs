//! Native ICO loading preserves Windows alpha antialiasing and size selection.
//! Keep tray glyphs separate from the larger dark-backed application artwork.
use std::path::Path;
use tauri::{Manager, Runtime};
use windows_sys::Win32::{
    System::{
        LibraryLoader::GetModuleHandleW,
        Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
    },
    UI::{
        HiDpi::GetDpiForWindow,
        WindowsAndMessaging::{
            LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_SHARED, WM_SETICON,
        },
    },
};

fn light_taskbar() -> bool {
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "SystemUsesLightTheme\0".encode_utf16().collect();
    let mut value = 0u32;
    let mut bytes = 4u32;
    // Read-only OS theme preference. Dark glyph for a light notification area.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut bytes,
        ) == 0
            && value != 0
    }
}

pub fn apply_tray<R: Runtime>(
    tray: &tauri::tray::TrayIcon<R>,
    cache: &Path,
    scale: f64,
) -> Result<(), String> {
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let (name, bytes): (&str, &[u8]) = if light_taskbar() {
        ("tray-black.ico", include_bytes!("../icons/tray/black.ico"))
    } else {
        ("tray-white.ico", include_bytes!("../icons/tray/white.ico"))
    };
    let file = cache.join(name);
    // Fixed application asset; no browser-supplied file paths or images.
    if std::fs::read(&file).ok().as_deref() != Some(bytes) {
        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    }
    let size = (16.0 * scale).round().clamp(16.0, 64.0) as u32;
    tray.with_inner_tray_icon(move |inner| {
        let icon =
            tray_icon::Icon::from_path(file, Some((size, size))).map_err(|e| e.to_string())?;
        inner.set_icon(Some(icon)).map_err(|e| e.to_string())
    })
    .map_err(|e| e.to_string())?
}

pub fn apply_window<R: Runtime>(window: &tauri::WebviewWindow<R>) {
    let Ok(hwnd) = window.hwnd() else { return };
    // Shared handles are owned/cached by Windows; they must not be destroyed.
    unsafe {
        let dpi = GetDpiForWindow(hwnd.0).max(96);
        for (kind, logical_size) in [(ICON_SMALL, 16), (ICON_BIG, 32)] {
            let size = (logical_size * dpi / 96) as i32;
            let icon = LoadImageW(
                GetModuleHandleW(std::ptr::null()),
                32512usize as *const u16,
                IMAGE_ICON,
                size,
                size,
                LR_SHARED,
            );
            if !icon.is_null() {
                SendMessageW(hwnd.0, WM_SETICON, kind as usize, icon as isize);
            }
        }
    }
}

pub fn refresh<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("dendrite") {
        apply_window(&window);
    }
    if let (Some(tray), Ok(cache)) = (app.tray_by_id(super::TRAY_ID), app.path().app_cache_dir()) {
        let scale = app
            .primary_monitor()
            .ok()
            .flatten()
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        if apply_tray(&tray, &cache.join("icons"), scale).is_err() {
            log::warn!("Could not refresh native desktop icon");
        }
    }
}
