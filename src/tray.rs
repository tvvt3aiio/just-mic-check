use std::cell::{Cell, RefCell};
use std::ptr::null;
use std::rc::Rc;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIcon, PostMessageW};
use winsafe::{self as w, co, guard::DestroyIconGuard};

use crate::icon::{self, Glyph};

pub const CLASS_NAME: &str = "JustMicCheck";
const INSTANCE_MUTEX: &str = "just-mic-check-single-instance";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "just-mic-check";
pub const START_IN_TRAY_ARG: &str = "--tray";

pub const WM_TRAY: u32 = 0x8000 + 1;
pub const WM_DEVICES: u32 = 0x8000 + 2;
pub const WM_OPEN: u32 = 0x8000 + 3;
const ICON_ID: u32 = 1;
pub const TIMER_TICK: usize = 1;
pub const TIMER_RETRY: usize = 2;
const RETRY_MS: u32 = 3000;

const ID_TOGGLE: u16 = 1;
const ID_OPEN: u16 = 3;
const ID_AUTOSTART: u16 = 4;
const ID_QUIT: u16 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Open,
    Toggle,
    ToggleAutostart,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Stopped,
    Live,
}

impl Status {
    fn index(self) -> usize {
        self as usize
    }
}

struct Tray {
    hwnd: w::HWND,
    icons: RefCell<[DestroyIconGuard; 2]>,
    icon_size: Cell<u32>,
    status: Cell<Status>,
    tick_ms: Cell<Option<u32>>,
    shown: Cell<bool>,
    ever_shown: Cell<bool>,
}

thread_local! {
    static TRAY: RefCell<Option<Rc<Tray>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<Tray>> {
    TRAY.with(|t| t.borrow().clone())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn make_icon(glyph: Glyph, size: u32) -> DestroyIconGuard {
    let bgra: Vec<u8> = icon::render(glyph, size).chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    let mask = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
    unsafe {
        let module = w::HINSTANCE::GetModuleHandle(None).expect("own module handle");
        let raw = CreateIcon(module.ptr() as _, size as i32, size as i32, 1, 32, mask.as_ptr(), bgra.as_ptr());
        DestroyIconGuard::new(w::HICON::from_ptr(raw as _))
    }
}

fn make_icons(size: u32) -> [DestroyIconGuard; 2] {
    [make_icon(Glyph::Stopped, size), make_icon(Glyph::Live, size)]
}

fn icon_size(hwnd: &w::HWND) -> u32 {
    w::GetSystemMetricsForDpi(co::SM::CXSMICON, hwnd.GetDpiForWindow()).unwrap_or(16).max(16) as u32
}

impl Tray {
    fn data(&self, flags: co::NIF) -> w::NOTIFYICONDATA {
        let mut d = w::NOTIFYICONDATA::default();
        d.hWnd = unsafe { self.hwnd.raw_copy() };
        d.uID = ICON_ID;
        d.uFlags = flags;
        d.uCallbackMessage = unsafe { co::WM::from_raw(WM_TRAY) };
        d.hIcon = unsafe { self.icons.borrow()[self.status.get().index()].raw_copy() };
        d
    }

    fn resize_icons(&self) -> bool {
        let size = icon_size(&self.hwnd);
        let changed = self.icon_size.replace(size) != size;
        if changed {
            *self.icons.borrow_mut() = make_icons(size);
        }
        changed
    }

    fn add_icon(&self) {
        self.resize_icons();
        let _ = w::Shell_NotifyIcon(co::NIM::DELETE, &self.data(co::NIF::default()));
        let ok = w::Shell_NotifyIcon(co::NIM::ADD, &self.data(co::NIF::MESSAGE | co::NIF::ICON)).is_ok();
        self.shown.set(ok);
        if ok {
            self.ever_shown.set(true);
            let _ = self.hwnd.KillTimer(TIMER_RETRY);
        } else {
            let _ = self.hwnd.SetTimer(TIMER_RETRY, RETRY_MS, None);
        }
    }
}

pub fn acquire_single_instance() -> bool {
    unsafe {
        CreateMutexW(null(), 0, wide(INSTANCE_MUTEX).as_ptr());
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

pub fn open_running_instance() {
    let found = w::HWND::FindWindow(Some(w::AtomStr::from_str(CLASS_NAME)), None);
    if let Ok(Some(hwnd)) = found {
        let (_, process_id) = hwnd.GetWindowThreadProcessId();
        let _ = w::AllowSetForegroundWindow(Some(process_id));
        post(hwnd.ptr() as isize, WM_OPEN, 0);
    }
}

pub fn post(hwnd: isize, msg: u32, wparam: usize) {
    unsafe {
        PostMessageW(hwnd as _, msg, wparam, 0);
    }
}

pub fn start(hwnd: &w::HWND) {
    let size = icon_size(hwnd);
    let tray = Rc::new(Tray {
        hwnd: unsafe { hwnd.raw_copy() },
        icons: RefCell::new(make_icons(size)),
        icon_size: Cell::new(size),
        status: Cell::new(Status::Stopped),
        tick_ms: Cell::new(None),
        shown: Cell::new(false),
        ever_shown: Cell::new(false),
    });
    tray.add_icon();
    TRAY.with(|t| *t.borrow_mut() = Some(tray));
}

pub fn add_icon() {
    if let Some(tray) = current() {
        tray.add_icon();
    }
}

pub fn on_dpi_changed() {
    let Some(tray) = current() else { return };
    if tray.resize_icons() && tray.shown.get() {
        let _ = w::Shell_NotifyIcon(co::NIM::MODIFY, &tray.data(co::NIF::ICON));
    }
}

pub fn show_menu() -> Option<Event> {
    let tray = current()?;
    let live = tray.status.get() == Status::Live;
    let mut menu = w::HMENU::CreatePopupMenu().ok()?;
    menu.append_item(&[
        w::MenuItem::Entry { cmd_id: ID_TOGGLE, text: if live { "モニター終了" } else { "モニター開始" } },
        w::MenuItem::Entry { cmd_id: ID_OPEN, text: "ウィンドウを開く" },
        w::MenuItem::Separator,
        w::MenuItem::Entry { cmd_id: ID_AUTOSTART, text: "Windowsと一緒に起動" },
        w::MenuItem::Separator,
        w::MenuItem::Entry { cmd_id: ID_QUIT, text: "終了" },
    ])
    .ok()?;
    let _ = menu.CheckMenuItem(w::IdPos::Id(ID_AUTOSTART), autostart_enabled());
    let _ = menu.SetMenuDefaultItem(w::IdPos::Id(ID_OPEN));

    let at = w::GetCursorPos().unwrap_or_default();
    tray.hwnd.SetForegroundWindow();
    let picked = menu.TrackPopupMenu(co::TPM::RETURNCMD | co::TPM::RIGHTBUTTON | co::TPM::NONOTIFY, at, &tray.hwnd);
    post(tray.hwnd.ptr() as isize, co::WM::NULL.raw(), 0);
    let _ = menu.DestroyMenu();
    match picked.ok()?? as u16 {
        ID_TOGGLE => Some(Event::Toggle),
        ID_OPEN => Some(Event::Open),
        ID_AUTOSTART => Some(Event::ToggleAutostart),
        ID_QUIT => Some(Event::Quit),
        _ => None,
    }
}

pub fn was_shown() -> bool {
    current().is_some_and(|t| t.ever_shown.get())
}

pub fn set_status(status: Status) {
    let Some(tray) = current() else { return };
    if tray.status.replace(status) != status && tray.shown.get() {
        let _ = w::Shell_NotifyIcon(co::NIM::MODIFY, &tray.data(co::NIF::ICON));
    }
}

pub fn set_tick(ms: Option<u32>) {
    let Some(tray) = current() else { return };
    if tray.tick_ms.replace(ms) == ms {
        return;
    }
    match ms {
        Some(ms) => {
            let _ = tray.hwnd.SetTimer(TIMER_TICK, ms, None);
        }
        None => {
            let _ = tray.hwnd.KillTimer(TIMER_TICK);
        }
    }
}

pub fn notify(text: &str) {
    let Some(tray) = current() else { return };
    if !tray.shown.get() {
        return;
    }
    let mut d = tray.data(co::NIF::INFO);
    d.set_szInfoTitle(crate::TITLE);
    d.set_szInfo(text);
    d.dwInfoFlags = co::NIIF::INFO | co::NIIF::NOSOUND;
    let _ = w::Shell_NotifyIcon(co::NIM::MODIFY, &d);
}

pub fn remove() {
    if let Some(tray) = current() {
        let _ = w::Shell_NotifyIcon(co::NIM::DELETE, &tray.data(co::NIF::default()));
    }
}

fn autostart_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {START_IN_TRAY_ARG}", exe.display()))
}

pub fn autostart_enabled() -> bool {
    let Some(command) = autostart_command() else { return false };
    matches!(
        w::HKEY::CURRENT_USER.RegGetValue(Some(RUN_KEY), Some(RUN_VALUE), co::RRF::RT_REG_SZ),
        Ok(w::RegistryValue::Sz(value)) if value == command
    )
}

fn delete_run_value() -> w::SysResult<()> {
    let opened = w::HKEY::CURRENT_USER.RegOpenKeyEx(Some(RUN_KEY), co::REG_OPTION::default(), co::KEY::SET_VALUE)?;
    opened.RegDeleteValue(Some(RUN_VALUE))
}

pub fn set_autostart(on: bool) -> bool {
    if on {
        let Some(command) = autostart_command() else { return false };
        w::HKEY::CURRENT_USER.RegSetKeyValue(Some(RUN_KEY), Some(RUN_VALUE), w::RegistryValue::Sz(command)).is_ok()
    } else {
        delete_run_value().is_ok()
    }
}
