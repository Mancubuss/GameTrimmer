//! Win32 System Tray Icon & Hidden Message Window.
//!
//! Creates a background message window via `CreateWindowExW`, manages the tray icon
//! with `Shell_NotifyIconW`, and provides the context menu with "Open GameTrimmer",
//! "Check now", "Pause/Resume", and "Exit".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, GetWindowLongPtrW, LoadIconW, PostMessageW, RegisterClassExW,
    SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenuEx, GWLP_USERDATA, HMENU,
    IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WM_USER, WNDCLASSEXW,
    WS_OVERLAPPED,
};

use crate::i18n::WatchStrings;
use crate::toast::show_tray_balloon;

pub const WM_TRAYICON: u32 = WM_USER + 1;
pub const TRAY_ICON_ID: u32 = 1;

pub const IDM_OPEN: usize = 1001;
pub const IDM_CHECK: usize = 1002;
pub const IDM_PAUSE: usize = 1003;
pub const IDM_EXIT: usize = 1004;

const WINDOW_CLASS_NAME: &str = "GameTrimmerWatchHiddenWindowClass";

/// Recovers from a poisoned `RwLock` instead of panicking.
///
/// This daemon's entire job is to keep running unattended; a panic on one
/// thread while it held `strings` (formatting a toast, say) must not also
/// take down the tray icon's next redraw. The strings themselves cannot be
/// left half-written by a panic - `WatchStrings` is replaced wholesale by
/// `update_i18n`, never mutated field-by-field - so the recovered guard's
/// data is exactly as usable as an unpoisoned one's.
trait PoisonTolerant<T> {
    fn read_poison_tolerant(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn write_poison_tolerant(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}

impl<T> PoisonTolerant<T> for RwLock<T> {
    fn read_poison_tolerant(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_poison_tolerant(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Tray menu actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    OpenUi,
    CheckNow,
    TogglePause,
    Exit,
}

struct WindowUserData {
    command_tx: Sender<TrayCommand>,
    is_paused: Arc<AtomicBool>,
    strings: Arc<RwLock<WatchStrings>>,
}

/// Win32 Tray Icon controller.
pub struct TrayIcon {
    hwnd: HWND,
    is_paused: Arc<AtomicBool>,
    strings: Arc<RwLock<WatchStrings>>,
    command_rx: Receiver<TrayCommand>,
    _user_data: Box<WindowUserData>,
}

impl TrayIcon {
    /// Creates and registers the system tray icon with a hidden message window.
    pub fn create(strings: WatchStrings) -> std::io::Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let is_paused = Arc::new(AtomicBool::new(false));
        let strings_arc = Arc::new(RwLock::new(strings));

        let user_data = Box::new(WindowUserData {
            command_tx,
            is_paused: Arc::clone(&is_paused),
            strings: Arc::clone(&strings_arc),
        });

        let hwnd = create_hidden_window(WINDOW_CLASS_NAME, &user_data)?;

        let mut tray = Self {
            hwnd,
            is_paused,
            strings: strings_arc,
            command_rx,
            _user_data: user_data,
        };

        let initial_tip = tray
            .strings
            .read_poison_tolerant()
            .tray_tooltip_active
            .clone();
        tray.add_icon(&initial_tip)?;
        Ok(tray)
    }

    /// Try to receive a pending user action from tray menu.
    pub fn try_recv_command(&self) -> Option<TrayCommand> {
        self.command_rx.try_recv().ok()
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.is_paused.store(paused, Ordering::SeqCst);
        let strings = self.strings.read_poison_tolerant();
        let tip = if paused {
            &strings.tray_tooltip_paused
        } else {
            &strings.tray_tooltip_active
        };
        let _ = self.update_tooltip(tip);
    }

    pub fn update_i18n(&mut self, new_strings: WatchStrings) {
        {
            let mut s = self.strings.write_poison_tolerant();
            *s = new_strings;
        }
        self.set_paused(self.is_paused());
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused.load(Ordering::SeqCst)
    }

    pub fn show_notification(&self, title: &str, body: &str) -> bool {
        show_tray_balloon(self.hwnd, TRAY_ICON_ID, title, body)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn add_icon(&mut self, tooltip: &str) -> std::io::Result<()> {
        let icon = unsafe { LoadIconW(None, IDI_APPLICATION) }
            .map_err(|e| std::io::Error::other(format!("LoadIconW: {e}")))?;

        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };

        let tip_wide = to_wide_capped(tooltip, 128);
        nid.szTip[..tip_wide.len()].copy_from_slice(&tip_wide);

        let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
        Ok(())
    }

    pub fn update_tooltip(&self, tooltip: &str) -> std::io::Result<()> {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_TIP,
            ..Default::default()
        };

        let tip_wide = to_wide_capped(tooltip, 128);
        nid.szTip[..tip_wide.len()].copy_from_slice(&tip_wide);

        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
        Ok(())
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ICON_ID,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn create_hidden_window(class_name: &str, user_data: &WindowUserData) -> std::io::Result<HWND> {
    let wide_class = to_wide(class_name);
    let hinstance = unsafe { GetModuleHandleW(None) }
        .map_err(|e| std::io::Error::other(format!("GetModuleHandleW: {e}")))?;

    let wnd_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: PCWSTR::from_raw(wide_class.as_ptr()),
        ..Default::default()
    };

    unsafe {
        let _ = RegisterClassExW(&wnd_class);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR::from_raw(wide_class.as_ptr()),
            PCWSTR::from_raw(wide_class.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
    }
    .map_err(|e| std::io::Error::other(format!("CreateWindowExW: {e}")))?;

    if hwnd.is_invalid() || hwnd.0.is_null() {
        return Err(std::io::Error::last_os_error());
    }

    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, user_data as *const _ as isize);
    }

    Ok(hwnd)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let user_data_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowUserData;
            if !user_data_ptr.is_null() {
                let user_data = &*user_data_ptr;
                let event_type = lparam.0 as u32;

                if event_type == WM_RBUTTONUP {
                    show_popup_menu(hwnd, user_data);
                } else if event_type == WM_LBUTTONDBLCLK {
                    let _ = user_data.command_tx.send(TrayCommand::OpenUi);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let user_data_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowUserData;
            if !user_data_ptr.is_null() {
                let user_data = &*user_data_ptr;
                let cmd_id = wparam.0 & 0xffff;

                match cmd_id {
                    IDM_OPEN => {
                        let _ = user_data.command_tx.send(TrayCommand::OpenUi);
                    }
                    IDM_CHECK => {
                        let _ = user_data.command_tx.send(TrayCommand::CheckNow);
                    }
                    IDM_PAUSE => {
                        let _ = user_data.command_tx.send(TrayCommand::TogglePause);
                    }
                    IDM_EXIT => {
                        let _ = user_data.command_tx.send(TrayCommand::Exit);
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn show_popup_menu(hwnd: HWND, user_data: &WindowUserData) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };

    let is_paused = user_data.is_paused.load(Ordering::SeqCst);
    let strings = user_data.strings.read_poison_tolerant();
    let pause_label = if is_paused {
        &strings.tray_menu_resume
    } else {
        &strings.tray_menu_pause
    };

    append_menu_item(menu, IDM_OPEN, &strings.tray_menu_open);
    append_menu_item(menu, IDM_CHECK, &strings.tray_menu_check_now);
    append_menu_item(menu, IDM_PAUSE, pause_label);
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
    append_menu_item(menu, IDM_EXIT, &strings.tray_menu_exit);

    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenuEx(
            menu,
            TPM_RIGHTBUTTON.0 | TPM_BOTTOMALIGN.0,
            pt.x,
            pt.y,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

fn append_menu_item(menu: HMENU, id: usize, label: &str) {
    let wide = to_wide(label);
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, id, PCWSTR::from_raw(wide.as_ptr()));
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn to_wide_capped(s: &str, max_len: usize) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_utf16().take(max_len - 1).collect();
    wide.push(0);
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_creation_and_command_channel() {
        let tray = TrayIcon::create(WatchStrings::english()).expect("create tray icon");
        assert!(!tray.is_paused());

        // Test sending command
        tray._user_data
            .command_tx
            .send(TrayCommand::CheckNow)
            .unwrap();
        let cmd = tray.try_recv_command();
        assert_eq!(cmd, Some(TrayCommand::CheckNow));
    }
}
