use std::cell::{Cell, RefCell};
use std::rc::Rc;

use winsafe::{self as w, co, gui, guard::DeleteObjectGuard, guard::DestroyIconGuard, msg, prelude::*, POINT, RECT, SIZE};

use crate::widgets::{self, scale, ButtonState, Fonts, Lamp, MeterArt, Tone};
use crate::{icon, tray, App, DeviceItems, Pick, View};

const TIMER_SAVE: usize = 3;
const SAVE_DELAY_MS: u32 = 1500;
const WM_DEFERRED: u32 = 0x8000 + 4;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_DRAWITEM: u32 = 0x002B;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONUP: u32 = 0x0205;
const CB_SETITEMHEIGHT: u32 = 0x0153;
const COMBO_ITEM_HEIGHT: i32 = 20;
const COMBO_LIST_HEIGHT: i32 = 280;
const SMALL_ICON: u32 = 16;
const BIG_ICON: u32 = 48;

/// 借用中に別のメッセージが割り込んだら、投げ直して後で処理する
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Toggle,
    Open,
    ToggleAutostart,
    Quit,
    Close,
    Resumed,
    TestButton,
    Replay,
    GiveUp,
    PickInput,
    PickOutput,
    PickChannel,
    Volume,
    Buffer,
}

const ACTIONS: [Action; 14] = [
    Action::Toggle,
    Action::Open,
    Action::ToggleAutostart,
    Action::Quit,
    Action::Close,
    Action::Resumed,
    Action::TestButton,
    Action::Replay,
    Action::GiveUp,
    Action::PickInput,
    Action::PickOutput,
    Action::PickChannel,
    Action::Volume,
    Action::Buffer,
];

#[derive(Clone, Copy, Default)]
struct Frames {
    lamp: RECT,
    limit: Option<RECT>,
    message: Option<RECT>,
    separator: RECT,
}

#[derive(Clone, PartialEq)]
struct LayoutKey {
    limit: bool,
    message: Option<String>,
    resume: bool,
    progress: bool,
    take: bool,
    details: bool,
    dpi: u32,
}

struct Brushes {
    body: DeleteObjectGuard<w::HBRUSH>,
    caution: DeleteObjectGuard<w::HBRUSH>,
    danger: DeleteObjectGuard<w::HBRUSH>,
}

struct State {
    dpi: Cell<u32>,
    fonts: RefCell<Option<Fonts>>,
    brushes: Brushes,
    icons: RefCell<Vec<DestroyIconGuard>>,
    art: RefCell<Option<MeterArt>>,
    frames: Cell<Frames>,
    details: Cell<bool>,
    hot: Cell<isize>,
    hidden: Cell<bool>,
    view: RefCell<Option<View>>,
    layout: RefCell<Option<LayoutKey>>,
    inputs: RefCell<Option<DeviceItems>>,
    outputs: RefCell<Option<DeviceItems>>,
    channels: Cell<Option<u32>>,
    saved: RefCell<String>,
}

#[derive(Clone)]
pub struct Ui {
    wnd: gui::WindowMain,
    btn_main: gui::Button,
    lbl_status: gui::Label,
    meter: gui::WindowControl,
    lbl_limit: gui::Label,
    lbl_message: gui::Label,
    btn_give_up: gui::Button,
    lbl_input: gui::Label,
    cmb_input: gui::ComboBox,
    lbl_channel: gui::Label,
    cmb_channel: gui::ComboBox,
    lbl_output: gui::Label,
    cmb_output: gui::ComboBox,
    lbl_volume: gui::Label,
    trk_volume: gui::Trackbar,
    val_volume: gui::Label,
    lbl_test: gui::Label,
    btn_test: gui::Button,
    lbl_progress: gui::Label,
    bar_progress: gui::ProgressBar,
    lbl_take: gui::Label,
    btn_replay: gui::Button,
    btn_details: gui::Button,
    lbl_buffer: gui::Label,
    trk_buffer: gui::Trackbar,
    val_buffer: gui::Label,
    chk_autostart: gui::CheckBox,
    app: Rc<RefCell<App>>,
    st: Rc<State>,
}

fn solid(color: w::COLORREF) -> DeleteObjectGuard<w::HBRUSH> {
    w::HBRUSH::CreateSolidBrush(color).expect("solid brush")
}

fn button(wnd: &gui::WindowMain, text: &str) -> gui::Button {
    gui::Button::new(
        wnd,
        gui::ButtonOpts {
            text,
            control_style: co::BS::OWNERDRAW,
            window_style: co::WS::CHILD | co::WS::TABSTOP | co::WS::VISIBLE,
            ..Default::default()
        },
    )
}

fn label(wnd: &gui::WindowMain, text: &str, align: co::SS) -> gui::Label {
    gui::Label::new(
        wnd,
        gui::LabelOpts { text, size: (1, 1), control_style: align | co::SS::NOPREFIX, ..Default::default() },
    )
}

fn row_label(wnd: &gui::WindowMain, text: &str) -> gui::Label {
    label(wnd, text, co::SS::LEFT | co::SS::CENTERIMAGE)
}

fn combo(wnd: &gui::WindowMain) -> gui::ComboBox {
    gui::ComboBox::new(
        wnd,
        gui::ComboBoxOpts {
            width: 1,
            control_style: co::CBS::DROPDOWNLIST,
            window_style: co::WS::CHILD | co::WS::TABSTOP | co::WS::VISIBLE | co::WS::VSCROLL,
            ..Default::default()
        },
    )
}

fn trackbar(wnd: &gui::WindowMain, range: (u32, u32)) -> gui::Trackbar {
    gui::Trackbar::new(
        wnd,
        gui::TrackbarOpts {
            size: (1, 1),
            control_style: co::TBS::HORZ | co::TBS::NOTICKS | co::TBS::BOTH,
            window_style: co::WS::CHILD | co::WS::TABSTOP | co::WS::VISIBLE,
            range,
            ..Default::default()
        },
    )
}

fn place(hwnd: &w::HWND, x: i32, y: i32, cx: i32, cy: i32) {
    let flags = co::SWP::NOZORDER | co::SWP::NOACTIVATE;
    let _ = hwnd.SetWindowPos(w::HwndPlace::None, POINT::with(x, y), SIZE::with(cx, cy), flags);
}

fn show(hwnd: &w::HWND, on: bool) {
    hwnd.ShowWindow(if on { co::SW::SHOWNA } else { co::SW::HIDE });
}

fn set_text(hwnd: &w::HWND, text: &str) {
    let _ = hwnd.SetWindowText(text);
    let _ = hwnd.InvalidateRect(None, false);
}

fn raw(hwnd: &w::HWND) -> isize {
    hwnd.ptr() as isize
}

fn set_items(cmb: &gui::ComboBox, labels: &[String]) {
    let items = cmb.items();
    items.delete_all();
    let _ = items.add(labels);
}

impl Ui {
    pub fn new(app: Rc<RefCell<App>>) -> Self {
        let wnd = gui::WindowMain::new(gui::WindowMainOpts {
            class_name: tray::CLASS_NAME,
            class_bg_brush: gui::Brush::None,
            title: crate::TITLE,
            size: gui::dpi(widgets::WINDOW_WIDTH, 440),
            style: co::WS::CAPTION | co::WS::SYSMENU | co::WS::MINIMIZEBOX | co::WS::CLIPCHILDREN | co::WS::BORDER,
            ..Default::default()
        });

        // タブの順番は作った順になる
        let btn_main = button(&wnd, "モニター開始");
        let lbl_status = row_label(&wnd, "停止中");
        let meter = gui::WindowControl::new(
            &wnd,
            gui::WindowControlOpts {
                class_bg_brush: gui::Brush::None,
                size: (1, 1),
                style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS,
                ..Default::default()
            },
        );
        let lbl_limit = label(&wnd, "", co::SS::LEFT);
        let lbl_message = label(&wnd, "", co::SS::LEFT);
        let btn_give_up = button(&wnd, "自動再開をやめる");
        let lbl_input = row_label(&wnd, "マイク");
        let cmb_input = combo(&wnd);
        let lbl_channel = row_label(&wnd, "チャンネル");
        let cmb_channel = combo(&wnd);
        let lbl_output = row_label(&wnd, "聞く先");
        let cmb_output = combo(&wnd);
        let lbl_volume = row_label(&wnd, "音量");
        let trk_volume = trackbar(&wnd, (0, 200));
        let val_volume = label(&wnd, "", co::SS::RIGHT | co::SS::CENTERIMAGE);
        let lbl_test = row_label(&wnd, "録音テスト");
        let btn_test = button(&wnd, "録音して再生");
        let lbl_progress = row_label(&wnd, "");
        let bar_progress = gui::ProgressBar::new(
            &wnd,
            gui::ProgressBarOpts { size: (1, 1), range: (0, 1000), ..Default::default() },
        );
        let lbl_take = row_label(&wnd, "");
        let btn_replay = button(&wnd, "もう一度再生");
        let btn_details = button(&wnd, "詳細設定");
        let lbl_buffer = row_label(&wnd, "バッファ");
        let trk_buffer = trackbar(&wnd, (crate::min_target_ms(), 100));
        let val_buffer = label(&wnd, "", co::SS::RIGHT | co::SS::CENTERIMAGE);
        let chk_autostart = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "Windowsと一緒に起動する",
                size: (1, 1),
                window_style: co::WS::CHILD | co::WS::TABSTOP | co::WS::VISIBLE,
                ..Default::default()
            },
        );

        let st = Rc::new(State {
            dpi: Cell::new(96),
            fonts: RefCell::new(None),
            brushes: Brushes {
                body: solid(widgets::BODY),
                caution: solid(Tone::Caution.fill()),
                danger: solid(Tone::Danger.fill()),
            },
            icons: RefCell::new(Vec::new()),
            art: RefCell::new(None),
            frames: Cell::new(Frames::default()),
            details: Cell::new(false),
            hot: Cell::new(0),
            hidden: Cell::new(true),
            view: RefCell::new(None),
            layout: RefCell::new(None),
            inputs: RefCell::new(None),
            outputs: RefCell::new(None),
            channels: Cell::new(None),
            saved: RefCell::new(String::new()),
        });

        let ui = Self {
            wnd,
            btn_main,
            lbl_status,
            meter,
            lbl_limit,
            lbl_message,
            btn_give_up,
            lbl_input,
            cmb_input,
            lbl_channel,
            cmb_channel,
            lbl_output,
            cmb_output,
            lbl_volume,
            trk_volume,
            val_volume,
            lbl_test,
            btn_test,
            lbl_progress,
            bar_progress,
            lbl_take,
            btn_replay,
            btn_details,
            lbl_buffer,
            trk_buffer,
            val_buffer,
            chk_autostart,
            app,
            st,
        };
        ui.events();
        ui
    }

    pub fn run(&self, start_in_tray: bool) -> w::AnyResult<i32> {
        self.wnd.run_main(Some(if start_in_tray { co::SW::HIDE } else { co::SW::SHOW }))
    }

    fn hwnd(&self) -> &w::HWND {
        self.wnd.hwnd()
    }

    fn fonts_controls(&self) -> Vec<&w::HWND> {
        vec![
            self.lbl_status.hwnd(),
            self.lbl_limit.hwnd(),
            self.lbl_message.hwnd(),
            self.lbl_input.hwnd(),
            self.cmb_input.hwnd(),
            self.lbl_channel.hwnd(),
            self.cmb_channel.hwnd(),
            self.lbl_output.hwnd(),
            self.cmb_output.hwnd(),
            self.lbl_volume.hwnd(),
            self.val_volume.hwnd(),
            self.lbl_test.hwnd(),
            self.lbl_progress.hwnd(),
            self.lbl_take.hwnd(),
            self.lbl_buffer.hwnd(),
            self.val_buffer.hwnd(),
            self.chk_autostart.hwnd(),
        ]
    }

    fn buttons(&self) -> [&gui::Button; 5] {
        [&self.btn_main, &self.btn_give_up, &self.btn_test, &self.btn_replay, &self.btn_details]
    }

    fn events(&self) {
        self.window_events();
        self.tray_events();
        self.control_events();
        self.paint_events();
    }

    fn window_events(&self) {
        let me = self.clone();
        self.wnd.on().wm_create(move |_| {
            me.on_create();
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm_close(move || {
            me.act(Action::Close);
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_size(move |_| {
            me.on_visibility_maybe_changed();
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_exit_size_move(move || {
            me.remember_position();
            me.save_soon();
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_end_session(move |p| {
            if p.is_session_being_ended {
                me.save_now();
                tray::remove();
            }
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(WM_DPICHANGED) }, move |p| {
            let suggested = unsafe { *(p.lparam as *const RECT) };
            place(me.hwnd(), suggested.left, suggested.top, suggested.right - suggested.left, suggested.bottom - suggested.top);
            me.set_dpi((p.wparam & 0xFFFF) as u32);
            tray::on_dpi_changed();
            me.sync();
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(WM_DEFERRED) }, move |p| {
            if let Some(action) = ACTIONS.get(p.wparam) {
                me.act(*action);
            }
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm_timer(TIMER_SAVE, move || {
            let _ = me.hwnd().KillTimer(TIMER_SAVE);
            me.save_now();
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_timer(tray::TIMER_TICK, move || {
            me.sync();
            Ok(())
        });
    }

    fn tray_events(&self) {
        let me = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(tray::WM_TRAY) }, move |p| {
            match p.lparam as u32 {
                WM_LBUTTONUP => me.act(Action::Open),
                WM_RBUTTONUP => {
                    me.refresh_autostart();
                    if let Some(event) = tray::show_menu() {
                        me.act(match event {
                            tray::Event::Open => Action::Open,
                            tray::Event::Toggle => Action::Toggle,
                            tray::Event::ToggleAutostart => Action::ToggleAutostart,
                            tray::Event::Quit => Action::Quit,
                        });
                    }
                }
                _ => {}
            }
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(tray::WM_DEVICES) }, move |_| {
            me.sync();
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(tray::WM_OPEN) }, move |_| {
            me.act(Action::Open);
            Ok(0)
        });

        self.wnd.on().wm_timer(tray::TIMER_RETRY, move || {
            tray::add_icon();
            Ok(())
        });

        if let Ok(created) = w::RegisterWindowMessage("TaskbarCreated") {
            self.wnd.on().wm(unsafe { co::WM::from_raw(created) }, move |_| {
                tray::add_icon();
                Ok(0)
            });
        }

        let me = self.clone();
        self.wnd.on().wm_power_broadcast(move |p| {
            if p.event == co::PBT::APMRESUMEAUTOMATIC {
                me.act(Action::Resumed);
            }
            Ok(())
        });
    }

    fn control_events(&self) {
        let clicks = [
            (&self.btn_main, Action::Toggle),
            (&self.btn_give_up, Action::GiveUp),
            (&self.btn_test, Action::TestButton),
            (&self.btn_replay, Action::Replay),
        ];
        for (btn, action) in clicks {
            let me = self.clone();
            btn.on().bn_clicked(move || {
                me.act(action);
                Ok(())
            });
        }

        let me = self.clone();
        self.btn_details.on().bn_clicked(move || {
            me.st.details.set(!me.st.details.get());
            let _ = me.btn_details.hwnd().InvalidateRect(None, false);
            me.sync();
            Ok(())
        });

        let me = self.clone();
        self.chk_autostart.on().bn_clicked(move || {
            me.act(Action::ToggleAutostart);
            Ok(())
        });

        let combos = [
            (&self.cmb_input, Action::PickInput),
            (&self.cmb_output, Action::PickOutput),
            (&self.cmb_channel, Action::PickChannel),
        ];
        for (cmb, action) in combos {
            let me = self.clone();
            cmb.on().cbn_sel_change(move || {
                me.act(action);
                Ok(())
            });
        }

        let me = self.clone();
        self.wnd.on().wm_h_scroll(move |p| {
            if let Some(from) = &p.hcontrol {
                if from == me.trk_volume.hwnd() {
                    me.act(Action::Volume);
                } else if from == me.trk_buffer.hwnd() {
                    me.act(Action::Buffer);
                }
            }
            Ok(())
        });

        for btn in self.buttons() {
            let (me, target) = (self.clone(), btn.clone());
            btn.on_subclass().wm(unsafe { co::WM::from_raw(WM_MOUSEMOVE) }, move |p| {
                let hwnd = target.hwnd();
                if me.st.hot.replace(raw(hwnd)) != raw(hwnd) {
                    let mut track = w::TRACKMOUSEEVENT::default();
                    track.dwFlags = co::TME::LEAVE;
                    track.hwndTrack = unsafe { hwnd.raw_copy() };
                    let _ = w::TrackMouseEvent(&mut track);
                    let _ = hwnd.InvalidateRect(None, false);
                }
                Ok(unsafe { hwnd.DefSubclassProc(p) })
            });
            let (me, target) = (self.clone(), btn.clone());
            btn.on_subclass().wm(unsafe { co::WM::from_raw(WM_MOUSELEAVE) }, move |p| {
                let hwnd = target.hwnd();
                me.st.hot.set(0);
                let _ = hwnd.InvalidateRect(None, false);
                Ok(unsafe { hwnd.DefSubclassProc(p) })
            });
        }
    }

    fn paint_events(&self) {
        self.wnd.on().wm_erase_bkgnd(move |_| Ok(1));

        let me = self.clone();
        self.wnd.on().wm_paint(move || {
            let hdc = me.hwnd().BeginPaint()?;
            let client = me.hwnd().GetClientRect()?;
            let frames = me.st.frames.get();
            let dpi = me.st.dpi.get();
            let lamp = me.st.view.borrow().as_ref().map_or(Lamp::Off, |v| v.lamp);
            let _ = widgets::buffered(&hdc, client, |dc| {
                widgets::fill(dc, client, widgets::BODY);
                widgets::lamp(dc, frames.lamp, lamp, dpi);
                if let Some(rc) = frames.limit {
                    widgets::banner(dc, rc, Tone::Caution, dpi);
                }
                if let Some(rc) = frames.message {
                    widgets::banner(dc, rc, Tone::Danger, dpi);
                }
                widgets::fill(dc, frames.separator, widgets::EDGE);
            });
            Ok(())
        });

        let me = self.clone();
        self.meter.on().wm_paint(move || {
            let hwnd = me.meter.hwnd();
            let hdc = hwnd.BeginPaint()?;
            let client = hwnd.GetClientRect()?;
            let (cx, cy) = (client.right, client.bottom);
            let (lit, peak) = me.st.view.borrow().as_ref().and_then(|v| v.level).unwrap_or((0, 0));
            let mut art = me.st.art.borrow_mut();
            if !art.as_ref().is_some_and(|a| a.fits(cx, cy)) {
                let fonts = me.st.fonts.borrow();
                let Some(fonts) = fonts.as_ref() else { return Ok(()) };
                *art = MeterArt::new(&hdc, cx, cy, me.st.dpi.get(), &fonts.number).ok();
            }
            if let Some(art) = art.as_ref() {
                let _ = art.paint(&hdc, lit, peak);
            }
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_ctl_color_static(move |p| {
            let (ink, bg, brush) = me.static_colors(&p.hwnd);
            let _ = p.hdc.SetTextColor(ink);
            let _ = p.hdc.SetBkColor(bg);
            Ok(unsafe { brush.raw_copy() })
        });

        let me = self.clone();
        self.wnd.on().wm(unsafe { co::WM::from_raw(WM_DRAWITEM) }, move |p| {
            let item = unsafe { &*(p.lparam as *const w::DRAWITEMSTRUCT) };
            me.draw_button(item);
            Ok(1)
        });
    }

    fn static_colors(&self, hwnd: &w::HWND) -> (w::COLORREF, w::COLORREF, &w::HBRUSH) {
        let brushes = &self.st.brushes;
        if hwnd == self.lbl_limit.hwnd() {
            (Tone::Caution.ink(), Tone::Caution.fill(), &brushes.caution)
        } else if hwnd == self.lbl_message.hwnd() {
            (Tone::Danger.ink(), Tone::Danger.fill(), &brushes.danger)
        } else if hwnd == self.lbl_status.hwnd() || hwnd == self.lbl_take.hwnd() || hwnd == self.lbl_test.hwnd() || hwnd == self.chk_autostart.hwnd() {
            (widgets::INK, widgets::BODY, &brushes.body)
        } else {
            (widgets::SUB_INK, widgets::BODY, &brushes.body)
        }
    }

    fn draw_button(&self, item: &w::DRAWITEMSTRUCT) {
        let fonts = self.st.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let dpi = self.st.dpi.get();
        let hwnd = &item.hwndItem;
        let label = hwnd.GetWindowText().unwrap_or_default();
        let state = ButtonState {
            pressed: item.itemState.has(co::ODS::SELECTED),
            hovered: self.st.hot.get() == raw(hwnd),
            enabled: !item.itemState.has(co::ODS::DISABLED),
            focused: item.itemState.has(co::ODS::FOCUS) && !item.itemState.has(co::ODS::NOFOCUSRECT),
        };
        let rc = item.rcItem;
        let _ = widgets::buffered(&item.hDC, rc, |dc| {
            let local = RECT { left: 0, top: 0, right: rc.right - rc.left, bottom: rc.bottom - rc.top };
            if hwnd == self.btn_main.hwnd() {
                let running = self.st.view.borrow().as_ref().is_some_and(|v| v.running);
                widgets::main_button(dc, local, &label, running, state, fonts, dpi);
            } else if hwnd == self.btn_details.hwnd() {
                widgets::disclosure(dc, local, &label, self.st.details.get(), state, fonts, dpi);
            } else if hwnd == self.btn_give_up.hwnd() {
                widgets::push_button(dc, local, &label, Tone::Danger.fill(), state, fonts, dpi);
            } else {
                widgets::push_button(dc, local, &label, widgets::BODY, state, fonts, dpi);
            }
        });
    }

    fn on_create(&self) {
        crate::set_window_handle(self.hwnd());
        tray::start(self.hwnd());
        self.set_window_icons();
        let saved = self.app.borrow().settings_text();
        *self.st.saved.borrow_mut() = saved;
        self.set_dpi(self.hwnd().GetDpiForWindow());
        self.restore_position();
        self.sync();
    }

    fn set_window_icons(&self) {
        let (big, small) = (tray::make_icon(icon::Glyph::App, BIG_ICON), tray::make_icon(icon::Glyph::App, SMALL_ICON));
        unsafe {
            self.hwnd().SendMessage(msg::WmSetIcon { size: co::ICON_SZ::BIG, hicon: big.raw_copy() });
            self.hwnd().SendMessage(msg::WmSetIcon { size: co::ICON_SZ::SMALL, hicon: small.raw_copy() });
        }
        *self.st.icons.borrow_mut() = vec![big, small];
    }

    fn restore_position(&self) {
        let Ok(app) = self.app.try_borrow() else { return };
        let Some((x, y)) = app.window_pos() else { return };
        let on_screen = w::HMONITOR::MonitorFromPoint(POINT::with(x, y), co::MONITOR::DEFAULTTONULL);
        if on_screen != w::HMONITOR::NULL {
            let flags = co::SWP::NOSIZE | co::SWP::NOZORDER | co::SWP::NOACTIVATE;
            let _ = self.hwnd().SetWindowPos(w::HwndPlace::None, POINT::with(x, y), SIZE::default(), flags);
        }
    }

    fn remember_position(&self) {
        if self.hwnd().IsIconic() || !self.hwnd().IsWindowVisible() {
            return;
        }
        let Ok(rc) = self.hwnd().GetWindowRect() else { return };
        if let Ok(mut app) = self.app.try_borrow_mut() {
            app.set_window_pos((rc.left, rc.top));
        }
    }

    fn set_dpi(&self, dpi: u32) {
        self.st.dpi.set(dpi);
        *self.st.art.borrow_mut() = None;
        let fonts = Fonts::new(dpi).ok();
        if let Some(fonts) = &fonts {
            for hwnd in self.fonts_controls() {
                unsafe {
                    hwnd.SendMessage(msg::WmSetFont { hfont: fonts.body.raw_copy(), redraw: true });
                }
            }
        }
        // 古いフォントはコントロールが手放してから消す
        *self.st.fonts.borrow_mut() = fonts;
        let item_height = scale(COMBO_ITEM_HEIGHT, dpi) as isize;
        for cmb in [&self.cmb_input, &self.cmb_channel, &self.cmb_output] {
            unsafe {
                cmb.hwnd().SendMessage(msg::Wm { msg_id: co::WM::from_raw(CB_SETITEMHEIGHT), wparam: usize::MAX, lparam: item_height });
            }
        }
        *self.st.layout.borrow_mut() = None;
    }

    fn hidden(&self) -> bool {
        !self.hwnd().IsWindowVisible() || self.hwnd().IsIconic()
    }

    fn on_visibility_maybe_changed(&self) {
        if self.st.hidden.get() != self.hidden() {
            self.sync();
        }
    }

    fn refresh_autostart(&self) {
        if let Ok(mut app) = self.app.try_borrow_mut() {
            app.refresh_autostart();
        }
    }

    fn act(&self, action: Action) {
        let Ok(mut app) = self.app.try_borrow_mut() else {
            if let Some(index) = ACTIONS.iter().position(|a| *a == action) {
                tray::post(raw(self.hwnd()), WM_DEFERRED, index);
            }
            return;
        };
        match action {
            Action::Toggle => app.toggle_monitor(),
            Action::Open => {
                app.refresh_autostart();
                drop(app);
                self.open_window();
                self.sync();
                return;
            }
            Action::ToggleAutostart => app.toggle_autostart(),
            Action::Quit => {
                app.shutdown();
                drop(app);
                self.remember_position();
                self.quit();
                return;
            }
            Action::Close => {
                if !tray::was_shown() {
                    app.shutdown();
                    drop(app);
                    self.remember_position();
                    self.quit();
                    return;
                }
                let first = app.on_window_closed();
                drop(app);
                self.remember_position();
                self.hwnd().ShowWindow(co::SW::HIDE);
                if first {
                    tray::notify("ウィンドウを閉じても、タスクトレイで動作を続けます。\n終了するには、アイコンを右クリックして「終了」を選んでください。");
                }
                self.sync();
                self.save_now();
                return;
            }
            Action::Resumed => app.on_resumed(),
            Action::TestButton => {
                if app.test_busy() {
                    app.abort_test();
                } else {
                    app.start_test();
                }
            }
            Action::Replay => app.replay(),
            Action::GiveUp => app.give_up_resume(),
            Action::PickInput => {
                let pick = self.picked(&self.cmb_input, &self.st.inputs);
                if let Some(pick) = pick {
                    app.pick_input(pick);
                }
            }
            Action::PickOutput => {
                let pick = self.picked(&self.cmb_output, &self.st.outputs);
                if let Some(pick) = pick {
                    app.pick_output(pick);
                }
            }
            Action::PickChannel => {
                if let Some(ch) = self.cmb_channel.items().selected_index() {
                    app.set_channel(ch);
                }
            }
            Action::Volume => app.set_volume(self.trk_volume.pos()),
            Action::Buffer => app.set_target_ms(self.trk_buffer.pos()),
        }
        drop(app);
        self.sync();
        self.save_soon();
    }

    fn picked(&self, cmb: &gui::ComboBox, items: &RefCell<Option<DeviceItems>>) -> Option<Pick> {
        let index = cmb.items().selected_index()? as usize;
        items.borrow().as_ref()?.picks.get(index).copied()
    }

    fn open_window(&self) {
        let hwnd = self.hwnd();
        hwnd.ShowWindow(if hwnd.IsIconic() { co::SW::RESTORE } else { co::SW::SHOW });
        hwnd.SetForegroundWindow();
    }

    fn quit(&self) {
        self.save_now();
        tray::remove();
        let _ = self.hwnd().DestroyWindow();
    }

    fn save_soon(&self) {
        let Ok(app) = self.app.try_borrow() else { return };
        let text = app.settings_text();
        if text != *self.st.saved.borrow() {
            let _ = self.hwnd().SetTimer(TIMER_SAVE, SAVE_DELAY_MS, None);
        }
    }

    fn save_now(&self) {
        let Ok(app) = self.app.try_borrow() else { return };
        let text = app.settings_text();
        let mut saved = self.st.saved.borrow_mut();
        if text != *saved && crate::save_settings(&text) {
            *saved = text;
        }
    }

    /// App の状態を進めて、その結果をコントロールに映す
    fn sync(&self) {
        let hidden = self.hidden();
        self.st.hidden.set(hidden);
        let Ok(mut app) = self.app.try_borrow_mut() else { return };
        app.set_window_hidden(hidden);
        app.tick();
        if !hidden {
            app.update_levels();
        }
        let view = app.view();
        let items = app.take_device_items();
        drop(app);
        if let Some((inputs, outputs)) = items {
            self.apply_devices(&self.cmb_input, &self.st.inputs, inputs);
            self.apply_devices(&self.cmb_output, &self.st.outputs, outputs);
        }
        self.apply(view);
    }

    fn apply_devices(&self, cmb: &gui::ComboBox, slot: &RefCell<Option<DeviceItems>>, items: DeviceItems) {
        let mut slot = slot.borrow_mut();
        if slot.as_ref() == Some(&items) {
            return;
        }
        if slot.as_ref().map(|s| &s.labels) != Some(&items.labels) {
            set_items(cmb, &items.labels);
            self.fit_drop_width(cmb, &items.labels);
        }
        cmb.items().select(Some(items.selected));
        *slot = Some(items);
    }

    fn fit_drop_width(&self, cmb: &gui::ComboBox, labels: &[String]) {
        let fonts = self.st.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let Ok(dc) = cmb.hwnd().GetDC() else { return };
        let Ok(_font) = dc.SelectObject(&*fonts.body) else { return };
        let widest = labels.iter().filter_map(|l| dc.GetTextExtentPoint32(l).ok()).map(|s| s.cx).max().unwrap_or(0);
        let padding = scale(24, self.st.dpi.get());
        unsafe {
            let _ = cmb.hwnd().SendMessage(msg::CbSetDroppedWidth { min_width: (widest + padding) as u32 });
        }
    }

    fn apply(&self, view: View) {
        let prev = self.st.view.borrow().clone();
        if prev.as_ref() == Some(&view) {
            self.apply_layout(&view);
            return;
        }
        *self.st.view.borrow_mut() = Some(view.clone());
        let changed = |pick: &dyn Fn(&View) -> bool| prev.as_ref().is_none_or(pick);

        if changed(&|p| p.status != view.status) {
            set_text(self.lbl_status.hwnd(), view.status);
        }
        if changed(&|p| p.lamp != view.lamp) {
            let frames = self.st.frames.get();
            let _ = self.hwnd().InvalidateRect(Some(&frames.lamp), false);
        }
        if changed(&|p| p.level != view.level) {
            let _ = self.meter.hwnd().InvalidateRect(None, false);
        }

        if changed(&|p| p.running != view.running) {
            set_text(self.btn_main.hwnd(), if view.running { "モニター終了" } else { "モニター開始" });
        }
        if changed(&|p| p.testing != view.testing) {
            self.btn_main.hwnd().EnableWindow(!view.testing);
            set_text(self.btn_test.hwnd(), if view.testing { "中止" } else { "録音して再生" });
        }
        if changed(&|p| (p.running, p.testing) != (view.running, view.testing)) {
            let locked = view.running || view.testing;
            self.cmb_input.hwnd().EnableWindow(!locked);
            self.cmb_output.hwnd().EnableWindow(!locked);
            let can_play = !locked;
            self.btn_test.hwnd().EnableWindow(view.testing || can_play);
            self.btn_replay.hwnd().EnableWindow(can_play);
        }

        if changed(&|p| p.volume != view.volume) {
            if self.trk_volume.pos() != view.volume {
                self.trk_volume.set_pos(view.volume);
            }
            set_text(self.val_volume.hwnd(), &format!("{} %", view.volume));
        }
        if changed(&|p| p.target_ms != view.target_ms) {
            if self.trk_buffer.pos() != view.target_ms {
                self.trk_buffer.set_pos(view.target_ms);
            }
            set_text(self.val_buffer.hwnd(), &format!("{} ms", view.target_ms));
        }
        if changed(&|p| p.autostart != view.autostart) {
            self.chk_autostart.set_check(view.autostart);
        }
        if self.st.channels.get() != Some(view.channels) {
            self.st.channels.set(Some(view.channels));
            let labels: Vec<String> = (0..=view.channels).map(crate::channel_label).collect();
            set_items(&self.cmb_channel, &labels);
            self.cmb_channel.items().select(Some(view.channel));
        } else if changed(&|p| p.channel != view.channel) {
            self.cmb_channel.items().select(Some(view.channel));
        }

        if changed(&|p| p.limit != view.limit) {
            set_text(self.lbl_limit.hwnd(), view.limit.as_deref().unwrap_or(""));
        }
        if changed(&|p| p.message != view.message) {
            set_text(self.lbl_message.hwnd(), view.message.as_deref().unwrap_or(""));
        }
        if changed(&|p| p.progress != view.progress) {
            if let Some((text, position)) = &view.progress {
                set_text(self.lbl_progress.hwnd(), text);
                self.bar_progress.set_position(*position);
            }
        }
        if changed(&|p| p.take != view.take) {
            set_text(self.lbl_take.hwnd(), view.take.as_deref().unwrap_or(""));
        }

        self.apply_layout(&view);
        self.keep_focus_reachable();
    }

    fn keep_focus_reachable(&self) {
        let Some(focus) = w::HWND::GetFocus() else { return };
        let lost = focus == *self.hwnd() || (self.hwnd().IsChild(&focus) && !(focus.IsWindowVisible() && focus.IsWindowEnabled()));
        if lost && !self.hidden() {
            let target = if self.btn_main.hwnd().IsWindowEnabled() { &self.btn_main } else { &self.btn_test };
            let _ = target.hwnd().SetFocus();
        }
    }

    fn apply_layout(&self, view: &View) {
        let key = LayoutKey {
            limit: view.limit.is_some(),
            message: view.message.clone(),
            resume: view.resume,
            progress: view.progress.is_some(),
            take: view.take.is_some(),
            details: self.st.details.get(),
            dpi: self.st.dpi.get(),
        };
        if self.st.layout.borrow().as_ref() == Some(&key) {
            return;
        }
        *self.st.layout.borrow_mut() = Some(key.clone());
        self.layout(&key);
    }

    fn layout(&self, key: &LayoutKey) {
        let dpi = key.dpi;
        let sc = |v: i32| scale(v, dpi);
        let fonts = self.st.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else { return };
        let (x0, full) = (sc(widgets::PANEL_MARGIN), sc(widgets::WINDOW_WIDTH - 2 * widgets::PANEL_MARGIN));
        let gap = sc(widgets::ITEM_SPACING);
        let row_h = sc(widgets::ROW_HEIGHT);
        let label_w = sc(widgets::LABEL_WIDTH);
        let field_x = x0 + label_w + sc(8);
        let field_w = full - label_w - sc(8);
        let value_w = sc(widgets::VALUE_WIDTH);
        let mut frames = Frames::default();
        let mut y = x0;

        // 状態の行
        let lamp = sc(12);
        let status_h = sc(24);
        let lamp_y = y + (status_h - lamp) / 2;
        frames.lamp = RECT { left: x0, top: lamp_y, right: x0 + lamp, bottom: lamp_y + lamp };
        place(self.lbl_status.hwnd(), x0 + sc(20), y, full - sc(20), status_h);
        y += status_h + gap;

        place(self.meter.hwnd(), x0, y, full, sc(widgets::METER_HEIGHT));
        y += sc(widgets::METER_HEIGHT) + gap;

        // リミッターとエラーの帯
        let (pad_x, pad_y) = (sc(10), sc(8));
        let text_w = full - 2 * pad_x;
        show(self.lbl_limit.hwnd(), key.limit);
        if key.limit {
            let text = "リミッター作動中\nハウリング";
            let h = widgets::measure_height(self.hwnd(), &fonts.body, text, text_w);
            place(self.lbl_limit.hwnd(), x0 + pad_x, y + pad_y, text_w, h);
            let height = h + 2 * pad_y;
            frames.limit = Some(RECT { left: x0, top: y, right: x0 + full, bottom: y + height });
            y += height + gap;
        }
        let give_up = key.message.is_some() && key.resume;
        show(self.lbl_message.hwnd(), key.message.is_some());
        show(self.btn_give_up.hwnd(), give_up);
        if let Some(message) = &key.message {
            let h = widgets::measure_height(self.hwnd(), &fonts.body, message, text_w);
            place(self.lbl_message.hwnd(), x0 + pad_x, y + pad_y, text_w, h);
            let mut height = h + 2 * pad_y;
            if give_up {
                place(self.btn_give_up.hwnd(), x0 + pad_x, y + height - pad_y + gap, sc(150), sc(30));
                height += gap + sc(30);
            }
            frames.message = Some(RECT { left: x0, top: y, right: x0 + full, bottom: y + height });
            y += height + gap;
        }

        place(self.btn_main.hwnd(), x0, y, full, sc(widgets::MAIN_BUTTON_HEIGHT));
        y += sc(widgets::MAIN_BUTTON_HEIGHT) + gap;

        // デバイスの行
        for (lbl, cmb) in [(&self.lbl_input, &self.cmb_input), (&self.lbl_channel, &self.cmb_channel), (&self.lbl_output, &self.cmb_output)] {
            place(lbl.hwnd(), x0, y, label_w, row_h);
            place(cmb.hwnd(), field_x, y, field_w, sc(COMBO_LIST_HEIGHT));
            let combo_h = cmb.hwnd().GetWindowRect().map_or(row_h, |rc| rc.bottom - rc.top);
            place(cmb.hwnd(), field_x, y + (row_h - combo_h) / 2, field_w, sc(COMBO_LIST_HEIGHT));
            y += row_h + gap;
        }

        let slider_row = |lbl: &gui::Label, trk: &gui::Trackbar, val: &gui::Label, y: i32| {
            place(lbl.hwnd(), x0, y, label_w, row_h);
            place(trk.hwnd(), field_x - sc(4), y, field_w - value_w, row_h);
            place(val.hwnd(), x0 + full - value_w, y, value_w, row_h);
        };
        slider_row(&self.lbl_volume, &self.trk_volume, &self.val_volume, y);
        y += row_h + gap;

        frames.separator = RECT { left: x0, top: y, right: x0 + full, bottom: y + sc(1).max(1) };
        y += sc(1).max(1) + gap;

        // 録音テストの行
        let button_w = sc(130);
        let button_h = sc(30);
        place(self.lbl_test.hwnd(), x0, y, full - button_w, row_h);
        place(self.btn_test.hwnd(), x0 + full - button_w, y + (row_h - button_h) / 2, button_w, button_h);
        y += row_h + gap;

        show(self.lbl_progress.hwnd(), key.progress);
        show(self.bar_progress.hwnd(), key.progress);
        if key.progress {
            let text_w = sc(160);
            place(self.lbl_progress.hwnd(), x0, y, text_w, sc(22));
            place(self.bar_progress.hwnd(), x0 + text_w + sc(8), y + sc(4), full - text_w - sc(8), sc(14));
            y += sc(22) + gap;
        }

        show(self.lbl_take.hwnd(), key.take);
        show(self.btn_replay.hwnd(), key.take);
        if key.take {
            place(self.lbl_take.hwnd(), x0, y, full - button_w, row_h);
            place(self.btn_replay.hwnd(), x0 + full - button_w, y + (row_h - button_h) / 2, button_w, button_h);
            y += row_h + gap;
        }

        // 詳細設定
        place(self.btn_details.hwnd(), x0, y, full, sc(26));
        y += sc(26) + gap;
        show(self.lbl_buffer.hwnd(), key.details);
        show(self.trk_buffer.hwnd(), key.details);
        show(self.val_buffer.hwnd(), key.details);
        show(self.chk_autostart.hwnd(), key.details);
        if key.details {
            slider_row(&self.lbl_buffer, &self.trk_buffer, &self.val_buffer, y);
            y += row_h + gap;
            place(self.chk_autostart.hwnd(), x0, y, full, sc(26));
            y += sc(26) + gap;
        }

        y += x0 - gap;
        self.st.frames.set(frames);
        self.fit_window(sc(widgets::WINDOW_WIDTH), y);
        let _ = self.hwnd().InvalidateRect(None, false);
    }

    fn fit_window(&self, cx: i32, cy: i32) {
        let hwnd = self.hwnd();
        let (Ok(outer), Ok(inner)) = (hwnd.GetWindowRect(), hwnd.GetClientRect()) else { return };
        let extra_x = (outer.right - outer.left) - inner.right;
        let extra_y = (outer.bottom - outer.top) - inner.bottom;
        let flags = co::SWP::NOMOVE | co::SWP::NOZORDER | co::SWP::NOACTIVATE;
        let _ = hwnd.SetWindowPos(w::HwndPlace::None, POINT::default(), SIZE::with(cx + extra_x, cy + extra_y), flags);
    }
}
