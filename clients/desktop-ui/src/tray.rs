// SPDX-License-Identifier: Apache-2.0

//! System tray icon with a four-item menu, plus the platform glue that hides and restores the
//! main window.
//!
//! The tray exists on Windows and macOS (`tray-icon`); other platforms build without it and
//! [`Tray::new`] reports that. Menu and click events are turned into [`Action`]s that the app
//! applies to the window; what each action means for the launcher's state lives in
//! `Proto::on_tray_action`.

// Without a tray (neither Windows nor macOS) only the tests use the pure helpers below.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use crate::host_mgr::{HostStatus, MISSING_TEXT};
use gpui::{App, Window};

/// The Carbon Precision Lock-Bolt icon, compiled in: no file has to sit next to the executable.
#[cfg(not(target_os = "macos"))]
const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon/skylatch-256.png");

/// macOS menu bar template image: 36x36, pure black on transparent. The system tints it for
/// light and dark menu bars and wallpapers.
#[cfg(target_os = "macos")]
const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon/skylatch-tray-template.png");

#[cfg(test)]
const TEMPLATE_PNG: &[u8] = include_bytes!("../../../assets/icon/skylatch-tray-template.png");
#[cfg(test)]
const COLOUR_PNG: &[u8] = include_bytes!("../../../assets/icon/skylatch-256.png");

pub const ID_OPEN: &str = "skylatch.open";
pub const ID_HOST: &str = "skylatch.host";
pub const ID_ADD: &str = "skylatch.add-device";
pub const ID_QUIT: &str = "skylatch.quit";

/// What the user asked for through the tray.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Show and focus the main window.
    Open,
    /// Switch hosting this machine on or off.
    ToggleHost,
    /// Show the window with the pairing-code dialog.
    AddDevice,
    /// Stop the local agent and exit.
    Quit,
}

pub fn action_for_menu_id(id: &str) -> Option<Action> {
    match id {
        ID_OPEN => Some(Action::Open),
        ID_HOST => Some(Action::ToggleHost),
        ID_ADD => Some(Action::AddDevice),
        ID_QUIT => Some(Action::Quit),
        _ => None,
    }
}

/// Text of the hosting menu item.
pub fn host_label(enabled: bool, status: &HostStatus) -> String {
    match (enabled, status) {
        (false, _) => "本机被控：已暂停".to_string(),
        (true, HostStatus::Missing(_)) => format!("本机被控：{MISSING_TEXT}"),
        (true, _) => "本机被控：运行中".to_string(),
    }
}

/// Decodes the embedded tray icon to RGBA8: `(pixels, width, height)`.
pub fn icon_rgba() -> Result<(Vec<u8>, u32, u32), String> {
    decode_png(ICON_PNG)
}

fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err("unsupported icon bit depth".into());
    }
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("unexpanded palette".into()),
    };
    Ok((rgba, info.width, info.height))
}

// ───────────────────────────── the tray icon ─────────────────────────────

#[cfg(any(windows, target_os = "macos"))]
pub use imp::Tray;

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use super::{Action, ID_ADD, ID_HOST, ID_OPEN, ID_QUIT, action_for_menu_id, icon_rgba};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    /// The icon and its menu. Create it on the main thread once the event loop runs; dropping it
    /// removes the icon.
    pub struct Tray {
        _icon: TrayIcon,
        host_item: MenuItem,
        host_text: String,
    }

    impl Tray {
        pub fn new(host_text: &str) -> Result<Tray, String> {
            let (rgba, w, h) = icon_rgba()?;
            let icon = Icon::from_rgba(rgba, w, h).map_err(|e| e.to_string())?;
            let open = MenuItem::with_id(ID_OPEN, "打开 Skylatch", true, None);
            let host = MenuItem::with_id(ID_HOST, host_text, true, None);
            let add = MenuItem::with_id(ID_ADD, "+ 添加设备", true, None);
            let quit = MenuItem::with_id(ID_QUIT, "退出 Skylatch", true, None);
            let menu = Menu::new();
            menu.append_items(&[&open, &host, &add, &PredefinedMenuItem::separator(), &quit])
                .map_err(|e| e.to_string())?;
            let builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Skylatch");
            // macOS: a template image, which the menu bar draws in the system's monochrome
            // (the replacement of the deprecated `with_icon_as_template(true)`)
            #[cfg(target_os = "macos")]
            let builder = builder.with_icon_templated(icon);
            #[cfg(not(target_os = "macos"))]
            let builder = builder.with_icon(icon);
            let tray = builder.build().map_err(|e| e.to_string())?;
            Ok(Tray {
                _icon: tray,
                host_item: host,
                host_text: host_text.to_string(),
            })
        }

        /// Keeps the hosting item's text current.
        pub fn set_host_text(&mut self, text: &str) {
            if self.host_text != text {
                self.host_item.set_text(text);
                self.host_text = text.to_string();
            }
        }

        /// Everything the user did since the last call. A left click, a double click and the
        /// "open" menu item all mean the same thing, so repeated opens collapse into one.
        pub fn drain(&self) -> Vec<Action> {
            let mut actions = Vec::new();
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if let Some(a) = action_for_menu_id(event.id().as_ref()) {
                    actions.push(a);
                }
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                let open = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if open {
                    actions.push(Action::Open);
                }
            }
            actions.dedup_by(|a, b| *a == Action::Open && *b == Action::Open);
            actions
        }
    }
}

/// The platform without a tray: [`Tray::new`] always fails and the launcher behaves as before.
#[cfg(not(any(windows, target_os = "macos")))]
pub struct Tray;

#[cfg(not(any(windows, target_os = "macos")))]
impl Tray {
    pub fn new(_host_text: &str) -> Result<Tray, String> {
        Err("system tray is not supported on this platform".into())
    }

    pub fn set_host_text(&mut self, _text: &str) {}

    pub fn drain(&self) -> Vec<Action> {
        Vec::new()
    }
}

// ───────────────────────────── main window show / hide ─────────────────────────────

#[cfg(windows)]
mod win {
    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    type Hwnd = *mut core::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn ShowWindow(hwnd: Hwnd, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: Hwnd) -> i32;
        fn IsIconic(hwnd: Hwnd) -> i32;
    }
    const SW_HIDE: i32 = 0;
    const SW_SHOW: i32 = 5;
    const SW_RESTORE: i32 = 9;

    fn hwnd(window: &Window) -> Option<Hwnd> {
        // `Window` has its own `window_handle()` (a gpui handle): name the trait explicitly
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get() as Hwnd),
            _ => None,
        }
    }

    /// Takes the window out of the taskbar and the screen; the process keeps running.
    pub fn hide(window: &Window) -> bool {
        let Some(hwnd) = hwnd(window) else {
            return false;
        };
        // SAFETY: `hwnd` is this process's own live top-level window.
        unsafe { ShowWindow(hwnd, SW_HIDE) };
        true
    }

    /// Shows a hidden or minimised window again and brings it to the front.
    pub fn show(window: &Window) -> bool {
        let Some(hwnd) = hwnd(window) else {
            return false;
        };
        // SAFETY: as above. A minimised window is restored, a hidden one shown at its last
        // size (a maximised one stays maximised).
        unsafe {
            let cmd = if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            ShowWindow(hwnd, cmd);
            SetForegroundWindow(hwnd);
        }
        true
    }
}

/// Hides the main window to the tray. Returns `false` when the platform cannot do it, in which
/// case the caller should close normally.
pub fn hide_main_window(window: &Window, cx: &mut App) -> bool {
    #[cfg(windows)]
    {
        let _ = cx;
        win::hide(window)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        cx.hide();
        true
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (window, cx);
        false
    }
}

/// Shows and focuses the main window.
pub fn show_main_window(window: &Window, cx: &mut App) {
    #[cfg(windows)]
    win::show(window);
    cx.activate(true);
    window.activate_window();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_the_four_actions() {
        assert_eq!(action_for_menu_id(ID_OPEN), Some(Action::Open));
        assert_eq!(action_for_menu_id(ID_HOST), Some(Action::ToggleHost));
        assert_eq!(action_for_menu_id(ID_ADD), Some(Action::AddDevice));
        assert_eq!(action_for_menu_id(ID_QUIT), Some(Action::Quit));
        assert_eq!(action_for_menu_id("something-else"), None);
        let ids = [ID_OPEN, ID_HOST, ID_ADD, ID_QUIT];
        for (i, a) in ids.iter().enumerate() {
            assert!(!ids[i + 1..].contains(a), "ids are unique");
        }
    }

    #[test]
    fn hosting_item_says_whether_this_machine_can_be_controlled() {
        assert_eq!(host_label(true, &HostStatus::Running), "本机被控：运行中");
        assert_eq!(host_label(true, &HostStatus::Enrolling), "本机被控：运行中");
        assert_eq!(host_label(false, &HostStatus::Off), "本机被控：已暂停");
        assert_eq!(host_label(false, &HostStatus::Running), "本机被控：已暂停");
        assert!(host_label(true, &HostStatus::Missing("x".into())).contains(MISSING_TEXT));
    }

    #[test]
    fn the_embedded_icon_decodes_to_rgba() {
        let (rgba, w, h) = icon_rgba().expect("icon decodes");
        #[cfg(target_os = "macos")]
        assert_eq!((w, h), (36, 36), "macOS embeds the menu bar template");
        #[cfg(not(target_os = "macos"))]
        assert_eq!((w, h), (256, 256), "Windows embeds the colour icon");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(
            rgba.iter().skip(3).step_by(4).any(|&alpha| alpha == 255),
            "has opaque pixels"
        );
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn the_macos_template_is_black_with_alpha_and_the_colour_icon_is_not() {
        let (rgba, w, h) = decode_png(TEMPLATE_PNG).expect("template decodes");
        assert_eq!((w, h), (36, 36));
        assert_eq!(rgba.len(), 36 * 36 * 4);
        let alphas: Vec<u8> = rgba.iter().skip(3).step_by(4).copied().collect();
        assert!(alphas.contains(&0), "transparent background");
        assert!(alphas.contains(&255), "opaque glyph");
        assert!(
            alphas.iter().any(|&a| a > 0 && a < 255),
            "anti-aliased edges keep their alpha"
        );
        // a template image carries its shape in alpha only: every visible pixel is black
        assert!(
            rgba.chunks(4)
                .filter(|p| p[3] > 0)
                .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0),
            "template pixels are pure black"
        );
        // the glyph fills most of the canvas without touching the border
        let at = |x: usize, y: usize| rgba[(y * 36 + x) * 4 + 3];
        assert!(
            (0..36).all(|i| at(i, 0) == 0 && at(0, i) == 0 && at(i, 35) == 0 && at(35, i) == 0)
        );
        assert!(alphas.iter().filter(|&&a| a > 0).count() > 36 * 36 / 8);

        // the Windows icon is the 256 px colour version, cobalt bolt included
        let (colour, cw, ch) = decode_png(COLOUR_PNG).expect("colour icon decodes");
        assert_eq!((cw, ch), (256, 256));
        assert!(
            colour
                .chunks(4)
                .any(|p| p[3] == 255 && p[2] > 200 && p[0] < 90),
            "cobalt bolt"
        );
    }
}
