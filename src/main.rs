#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod icon;
mod tray;
mod ui;
mod widgets;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use widgets::Lamp;
use winsafe::{self as w, co, prelude::*};

const APP_ID: &str = "just-mic-check";
pub const TITLE: &str = "マイクの音声確認";

fn main() {
    install_panic_hook();
    // 文字入力欄がないので、日本語入力を読み込まない
    unsafe {
        windows_sys::Win32::UI::Input::Ime::ImmDisableIME(u32::MAX);
    }
    if !tray::acquire_single_instance() {
        tray::open_running_instance();
        return;
    }
    let start_in_tray = std::env::args().any(|a| a == tray::START_IN_TRAY_ARG);
    let app = Rc::new(RefCell::new(App::new()));
    if let Err(e) = ui::Ui::new(app).run(start_in_tray) {
        show_error_box(&format!("ウィンドウを開けませんでした\n\n{e}"));
    }
}

pub const TEST_SECS: f32 = 5.0;
const METER_FRAME: Duration = Duration::from_millis(50);
const METER_DECAY: f32 = 0.78;
const LIMIT_DECAY: f32 = 0.9;
const PEAK_HOLD_FRAMES: f32 = 20.0;
const TICK_MONITOR_MS: u32 = 500;
const TICK_TEST_MS: u32 = 100;
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
const DEFAULT_VOLUME_PCT: f32 = 80.0;
const MAX_VOLUME_PCT: f32 = 200.0;
const DEFAULT_TARGET_MS: u32 = 20;

struct Settings {
    input: Option<String>,
    output: Option<String>,
    input_id: Option<String>,
    output_id: Option<String>,
    channel: u32,
    volume_pct: f32,
    target_ms: u32,
    tray_notice_seen: bool,
    window_pos: Option<(i32, i32)>,
}

impl Settings {
    fn to_text(&self) -> String {
        let mut text = String::new();
        for (key, value) in [("input", &self.input), ("output", &self.output), ("input_id", &self.input_id), ("output_id", &self.output_id)] {
            if let Some(value) = value {
                text += &format!("{key}={}\n", escape(value));
            }
        }
        text += &format!("channel={}\nvolume_pct={}\ntarget_ms={}\ntray_notice_seen={}\n", self.channel, self.volume_pct.round() as u32, self.target_ms, self.tray_notice_seen);
        if let Some((x, y)) = self.window_pos {
            text += &format!("window_pos={x},{y}\n");
        }
        text
    }

    /// 壊れた項目は既定値のままにする。知っている項目が1つもなければ None
    fn parse(text: &str) -> Option<Self> {
        let mut s = Self {
            input: None,
            output: None,
            input_id: None,
            output_id: None,
            channel: 0,
            volume_pct: DEFAULT_VOLUME_PCT,
            target_ms: DEFAULT_TARGET_MS,
            tray_notice_seen: false,
            window_pos: None,
        };
        let mut found = false;
        for line in text.trim_start_matches('\u{feff}').lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "input" => s.input = Some(unescape(value)),
                "output" => s.output = Some(unescape(value)),
                "input_id" => s.input_id = Some(unescape(value)),
                "output_id" => s.output_id = Some(unescape(value)),
                "channel" => s.channel = value.trim().parse().unwrap_or(s.channel),
                "volume_pct" => {
                    // 手で書かれた小数は切り捨てて読む
                    let whole = value.trim().split('.').next().unwrap_or_default();
                    s.volume_pct = whole.parse::<u32>().map_or(s.volume_pct, |v| (v as f32).min(MAX_VOLUME_PCT));
                }
                "target_ms" => s.target_ms = value.trim().parse().unwrap_or(s.target_ms),
                "tray_notice_seen" => s.tray_notice_seen = value.trim() == "true",
                "window_pos" => s.window_pos = parse_pos(value),
                _ => continue,
            }
            found = true;
        }
        found.then_some(s)
    }
}

fn parse_pos(value: &str) -> Option<(i32, i32)> {
    let (x, y) = value.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') | None => out.push('\\'),
            Some(other) => out.extend(['\\', other]),
        }
    }
    out
}

const SETTINGS_FILE: &str = "settings.txt";

fn appdata() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

fn settings_dir() -> Option<PathBuf> {
    Some(appdata()?.join(APP_ID))
}

fn load_settings() -> Option<Settings> {
    read_settings(&settings_dir()?)
}

fn read_settings(dir: &Path) -> Option<Settings> {
    Settings::parse(&std::fs::read_to_string(dir.join(SETTINGS_FILE)).ok()?)
}

fn save_settings_to(dir: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!("{SETTINGS_FILE}.tmp"));
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, dir.join(SETTINGS_FILE))
}

pub enum Choice {
    Default,
    Device(audio::DeviceEntry),
    Missing { label: String, id: Option<String> },
}

impl Choice {
    fn resolve<'a>(&'a self, list: &'a [audio::DeviceEntry], default: Option<&'a audio::DeviceEntry>) -> Option<&'a audio::DeviceEntry> {
        match self {
            Choice::Default => default,
            Choice::Device(e) => audio::find_entry(list, &e.device),
            Choice::Missing { .. } => None,
        }
    }

    fn is_default(&self) -> bool {
        matches!(self, Choice::Default)
    }

    fn saved_label(&self) -> Option<String> {
        match self {
            Choice::Default => None,
            Choice::Device(e) => Some(e.label.clone()),
            Choice::Missing { label, .. } => Some(label.clone()),
        }
    }

    fn saved_id(&self) -> Option<String> {
        match self {
            Choice::Default => None,
            Choice::Device(e) => e.id.clone(),
            Choice::Missing { id, .. } => id.clone(),
        }
    }

    fn refresh(&mut self, list: &[audio::DeviceEntry]) {
        match self {
            Choice::Default => {}
            Choice::Device(e) => {
                if let Some(now) = audio::find_entry(list, &e.device) {
                    e.label.clone_from(&now.label);
                } else if let Some(found) = Self::find_saved(&e.label, &e.id, list) {
                    *e = found.clone();
                }
            }
            Choice::Missing { label, id } => {
                if let Some(found) = Self::find_saved(label, id, list) {
                    *self = Choice::Device(found.clone());
                }
            }
        }
    }

    fn find_saved<'a>(label: &str, id: &Option<String>, list: &'a [audio::DeviceEntry]) -> Option<&'a audio::DeviceEntry> {
        list.iter().find(|e| e.id.is_some() && e.id == *id).or_else(|| list.iter().find(|e| e.label == label))
    }

    fn from_saved(saved: &Option<String>, saved_id: &Option<String>, list: &[audio::DeviceEntry]) -> Choice {
        let Some(label) = saved else { return Choice::Default };
        Self::find_saved(label, saved_id, list)
            .map_or_else(|| Choice::Missing { label: label.clone(), id: saved_id.clone() }, |e| Choice::Device(e.clone()))
    }
}

/// デバイスを選ぶコンボボックスの 1 行が指すもの
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Default,
    Entry(usize),
    Gone,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DeviceItems {
    pub labels: Vec<String>,
    pub picks: Vec<Pick>,
    pub selected: u32,
}

fn device_items(choice: &Choice, list: &[audio::DeviceEntry], default: Option<&audio::DeviceEntry>) -> DeviceItems {
    let mut labels = vec![format!("既定に追従（今は {}）", default.map_or("なし", |d| d.label.as_str()))];
    let mut picks = vec![Pick::Default];
    let mut selected = 0;
    for (i, entry) in list.iter().enumerate() {
        let is_default = default.is_some_and(|d| d.device == entry.device);
        labels.push(if is_default { format!("{}（既定）", entry.label) } else { entry.label.clone() });
        picks.push(Pick::Entry(i));
        if matches!(choice, Choice::Device(e) if e.device == entry.device) {
            selected = picks.len() as u32 - 1;
        }
    }
    let gone = match choice {
        Choice::Device(e) if audio::find_entry(list, &e.device).is_none() => Some(&e.label),
        Choice::Missing { label, .. } => Some(label),
        _ => None,
    };
    if let Some(label) = gone {
        labels.push(format!("{label}（見つかりません）"));
        picks.push(Pick::Gone);
        selected = picks.len() as u32 - 1;
    }
    DeviceItems { labels, picks, selected }
}

#[derive(Clone, PartialEq)]
pub struct View {
    pub running: bool,
    pub testing: bool,
    pub lamp: Lamp,
    pub status: &'static str,
    pub limit: Option<String>,
    pub message: Option<String>,
    pub resume: bool,
    pub volume: u32,
    pub target_ms: u32,
    pub channel: u32,
    pub channels: u32,
    pub autostart: bool,
    pub progress: Option<(String, u32)>,
    pub take: Option<String>,
    pub level: Option<(usize, usize)>,
}

/// 音声スレッドやデバイス監視スレッドからウィンドウを起こすための窓口
static WINDOW: AtomicIsize = AtomicIsize::new(0);

fn watch_devices() -> mpsc::Receiver<audio::DeviceList> {
    audio::watch_devices(|| match WINDOW.load(Ordering::Relaxed) {
        0 => {}
        hwnd => tray::post(hwnd, tray::WM_DEVICES, 0),
    })
}

pub struct App {
    devices: audio::DeviceList,
    device_updates: mpsc::Receiver<audio::DeviceList>,
    devices_dirty: bool,
    input: Choice,
    output: Choice,
    channel: u32,
    in_channels: u32,
    volume_pct: f32,
    target_ms: u32,
    monitor: Option<audio::Monitor>,
    test: Option<audio::LoopTest>,
    test_phase: audio::TestPhase,
    resume: bool,
    retry_at: Option<Instant>,
    retry_delay: Duration,
    meter: f32,
    peak_hold: f32,
    peak_hold_left: f32,
    levels_at: Instant,
    limit_db: f32,
    message: Option<String>,
    window_hidden: bool,
    autostart: bool,
    tray_notice_seen: bool,
    last_message: Option<String>,
    window_pos: Option<(i32, i32)>,
}

impl App {
    fn new() -> Self {
        let device_updates = watch_devices();
        let devices = audio::list_devices();
        let fixed_output = || {
            devices
                .default_output()
                .or_else(|| devices.outputs.first())
                .map_or(Choice::Default, |e| Choice::Device(e.clone()))
        };

        let saved = load_settings();
        let (input, output) = match &saved {
            Some(s) => (Choice::from_saved(&s.input, &s.input_id, &devices.inputs), Choice::from_saved(&s.output, &s.output_id, &devices.outputs)),
            None => (Choice::Default, fixed_output()),
        };
        let message = missing_notice(&input, &output);

        let mut app = Self {
            device_updates,
            devices_dirty: true,
            input,
            output,
            channel: saved.as_ref().map_or(0, |s| s.channel),
            in_channels: 2,
            volume_pct: saved.as_ref().map_or(DEFAULT_VOLUME_PCT, |s| s.volume_pct),
            target_ms: saved.as_ref().map_or(DEFAULT_TARGET_MS, |s| s.target_ms.max(audio::MIN_TARGET_MS)),
            tray_notice_seen: saved.as_ref().is_some_and(|s| s.tray_notice_seen),
            window_pos: saved.as_ref().and_then(|s| s.window_pos),
            devices,
            monitor: None,
            test: None,
            test_phase: audio::TestPhase::Done,
            resume: false,
            retry_at: None,
            retry_delay: RETRY_FIRST,
            meter: 0.0,
            peak_hold: 0.0,
            peak_hold_left: 0.0,
            levels_at: Instant::now(),
            limit_db: 0.0,
            last_message: message.clone(),
            message,
            window_hidden: false,
            autostart: tray::autostart_enabled(),
        };
        app.refresh_channels();
        app
    }

    fn settings(&self) -> Settings {
        Settings {
            input: self.input.saved_label(),
            output: self.output.saved_label(),
            input_id: self.input.saved_id(),
            output_id: self.output.saved_id(),
            channel: self.channel,
            volume_pct: self.volume_pct,
            target_ms: self.target_ms,
            tray_notice_seen: self.tray_notice_seen,
            window_pos: self.window_pos,
        }
    }

    pub fn settings_text(&self) -> String {
        self.settings().to_text()
    }

    pub fn window_pos(&self) -> Option<(i32, i32)> {
        self.window_pos
    }

    pub fn set_window_pos(&mut self, pos: (i32, i32)) {
        self.window_pos = Some(pos);
    }

    fn refresh_channels(&mut self) {
        let input = self.input.resolve(&self.devices.inputs, self.devices.default_input());
        let Some(count) = input.and_then(|e| audio::input_channels(&e.device)) else { return };
        self.in_channels = count;
        if self.channel > count {
            self.channel = 0;
        }
    }

    fn reload_devices(&mut self) {
        self.devices = audio::list_devices();
        self.input.refresh(&self.devices.inputs);
        self.output.refresh(&self.devices.outputs);
        self.devices_dirty = true;
    }

    fn resolve_devices(&mut self) -> Result<(cpal::Device, cpal::Device), String> {
        self.reload_devices();
        let input = self
            .input
            .resolve(&self.devices.inputs, self.devices.default_input())
            .ok_or("録音デバイスが見つかりません")?;
        Ok((input.device.clone(), self.resolve_output()?))
    }

    fn resolve_output(&self) -> Result<cpal::Device, String> {
        let output = self
            .output
            .resolve(&self.devices.outputs, self.devices.default_output())
            .ok_or("再生デバイスが見つかりません")?;
        Ok(output.device.clone())
    }

    pub fn start_test(&mut self) {
        let result = self.resolve_devices().and_then(|(i, o)| {
            audio::LoopTest::start(&i, &o, self.volume_pct / 100.0, self.channel, TEST_SECS)
        });
        match result {
            Ok(t) => {
                self.test = Some(t);
                self.test_phase = audio::TestPhase::Recording(0.0);
                self.message = None;
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn poll_test(&mut self) {
        let Some(t) = &mut self.test else { return };
        match t.poll() {
            Ok(phase) => self.test_phase = phase,
            Err(e) => {
                self.test = None;
                self.test_phase = audio::TestPhase::Done;
                self.message = Some(e);
            }
        }
    }

    pub fn test_busy(&self) -> bool {
        self.test.as_ref().is_some_and(|t| t.is_busy())
    }

    pub fn abort_test(&mut self) {
        if let Some(t) = &mut self.test {
            t.abort();
        }
        if !self.test.as_ref().is_some_and(|t| t.has_take()) {
            self.test = None;
        }
        self.test_phase = audio::TestPhase::Done;
    }

    pub fn replay(&mut self) {
        self.reload_devices();
        let result = self.resolve_output().and_then(|o| self.test.as_mut().map_or(Ok(()), |t| t.replay(&o)));
        match result {
            Ok(()) => self.test_phase = audio::TestPhase::Playing(0.0),
            Err(e) => self.message = Some(e),
        }
    }

    fn start(&mut self) {
        let result = self.resolve_devices().and_then(|(i, o)| {
            audio::start(&i, &o, self.volume_pct / 100.0, self.target_ms, self.channel)
        });
        match result {
            Ok(m) => {
                self.monitor = Some(m);
                self.message = None;
                self.cancel_resume();
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn stop(&mut self) {
        self.monitor = None;
        self.meter = 0.0;
        self.limit_db = 0.0;
    }

    pub fn shutdown(&mut self) {
        self.stop();
        self.test = None;
    }

    pub fn toggle_monitor(&mut self) {
        if self.monitor.is_none() && self.test_busy() {
            return;
        }
        self.cancel_resume();
        self.retry_delay = RETRY_FIRST;
        if self.monitor.is_some() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn on_devices_changed(&mut self) {
        let notice = missing_notice(&self.input, &self.output);
        self.input.refresh(&self.devices.inputs);
        self.output.refresh(&self.devices.outputs);
        self.devices_dirty = true;
        if notice.is_some() && self.message == notice {
            self.message = missing_notice(&self.input, &self.output);
        }
        self.refresh_channels();
        if let Some(m) = &self.monitor {
            let moved = |choice: &Choice, current: &cpal::Device, default: Option<&audio::DeviceEntry>| {
                choice.is_default() && default.is_some_and(|d| d.device != *current)
            };
            if moved(&self.input, &m.input_device, self.devices.default_input())
                || moved(&self.output, &m.output_device, self.devices.default_output())
            {
                self.stop();
                self.resume = true;
                self.retry_delay = RETRY_FIRST;
                self.try_resume();
            }
        } else if self.resume {
            self.retry_delay = RETRY_FIRST;
            self.retry_at = Some(Instant::now());
        }
    }

    pub fn on_resumed(&mut self) {
        if self.resume {
            self.retry_delay = RETRY_FIRST;
            self.schedule_retry();
        }
    }

    fn cancel_resume(&mut self) {
        self.resume = false;
        self.retry_at = None;
    }

    pub fn give_up_resume(&mut self) {
        self.cancel_resume();
        self.retry_delay = RETRY_FIRST;
        self.message = None;
    }

    fn try_resume(&mut self) {
        self.start();
        if self.resume {
            self.schedule_retry();
        }
    }

    fn schedule_retry(&mut self) {
        self.retry_at = Some(Instant::now() + self.retry_delay);
        self.retry_delay = (self.retry_delay * 2).min(RETRY_MAX);
    }

    fn poll_resume(&mut self) {
        if self.retry_at.is_some_and(|at| Instant::now() >= at) && !self.test_busy() {
            self.try_resume();
        }
        if self.monitor.as_ref().is_some_and(|m| m.uptime() >= RETRY_MAX) {
            self.retry_delay = RETRY_FIRST;
        }
    }

    fn check_error(&mut self) {
        let Some(m) = &mut self.monitor else { return };
        let (e, stalled) = match m.shared.error() {
            Some(e) => (e, false),
            None if m.is_stalled() => ("音声が停止しました".to_string(), true),
            None => return,
        };
        if stalled {
            if let Some(m) = self.monitor.take() {
                m.abandon();
            }
        }
        self.stop();
        self.resume = true;
        self.message = Some(e);
        self.schedule_retry();
    }

    pub fn tick(&mut self) {
        let mut changed = false;
        loop {
            match self.device_updates.try_recv() {
                Ok(list) => {
                    self.devices = list;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.device_updates = watch_devices();
                    break;
                }
            }
        }
        if changed {
            self.on_devices_changed();
        }
        self.check_error();
        self.poll_test();
        self.poll_resume();
        self.notify_when_hidden();
        self.sync_tray();
    }

    fn notify_when_hidden(&mut self) {
        if self.window_hidden && self.message != self.last_message && self.retry_delay <= RETRY_FIRST * 2 {
            if let Some(message) = &self.message {
                tray::notify(message);
            }
        }
        self.last_message.clone_from(&self.message);
    }

    fn sync_tray(&self) {
        tray::set_status(if self.monitor.is_some() { tray::Status::Live } else { tray::Status::Stopped });
        let active = self.monitor.is_some() || self.test_busy();
        tray::set_tick(if active && !self.window_hidden {
            Some(METER_FRAME.as_millis() as u32)
        } else if self.test_busy() {
            Some(TICK_TEST_MS)
        } else if self.monitor.is_some() || self.retry_at.is_some() {
            Some(TICK_MONITOR_MS)
        } else {
            None
        });
    }

    pub fn set_window_hidden(&mut self, hidden: bool) {
        self.window_hidden = hidden;
    }

    /// ウィンドウを閉じる。初めて閉じたときは true を返す（トレイの案内を出すため）
    pub fn on_window_closed(&mut self) -> bool {
        if self.test_busy() {
            self.abort_test();
        }
        !std::mem::replace(&mut self.tray_notice_seen, true)
    }

    pub fn toggle_autostart(&mut self) {
        let on = !tray::autostart_enabled();
        if tray::set_autostart(on) {
            self.autostart = on;
        } else {
            self.message = Some("Windowsと一緒に起動する設定を変更できませんでした".to_string());
        }
    }

    pub fn refresh_autostart(&mut self) {
        self.autostart = tray::autostart_enabled();
    }

    pub fn update_levels(&mut self) {
        let now = Instant::now();
        let frames = now.duration_since(self.levels_at).as_secs_f32() / METER_FRAME.as_secs_f32();
        self.levels_at = now;
        let meter_decay = METER_DECAY.powf(frames);

        let peak = match (&self.monitor, &self.test, self.test_phase) {
            (Some(m), _, _) => Some(m.shared.take_peak()),
            (None, Some(t), audio::TestPhase::Recording(_)) => Some(t.shared.take_peak()),
            _ => None,
        };
        match peak {
            Some(peak) => {
                self.meter = peak.max(self.meter * meter_decay);
                if self.meter >= self.peak_hold {
                    self.peak_hold = self.meter;
                    self.peak_hold_left = PEAK_HOLD_FRAMES;
                } else if self.peak_hold_left > 0.0 {
                    self.peak_hold_left -= frames;
                } else {
                    self.peak_hold *= meter_decay;
                }
            }
            None => {
                self.meter = 0.0;
                self.peak_hold = 0.0;
            }
        }
        if let Some(m) = &self.monitor {
            self.limit_db = m.shared.take_limit_db().max(self.limit_db * LIMIT_DECAY.powf(frames));
        }
    }

    pub fn view(&self) -> View {
        let running = self.monitor.is_some();
        let testing = self.test_busy();
        let recording = matches!(self.test_phase, audio::TestPhase::Recording(_));
        let (lamp, status) = match self.test_phase {
            _ if running => (Lamp::Live, "モニター中"),
            audio::TestPhase::Recording(_) => (Lamp::Test, "録音中…"),
            audio::TestPhase::Playing(_) => (Lamp::Test, "再生中…"),
            audio::TestPhase::Done => (Lamp::Off, "停止中"),
        };
        let progress = match self.test_phase {
            audio::TestPhase::Recording(p) => Some((format!("録音中… {:.1} / {TEST_SECS:.0} 秒", p * TEST_SECS), (p * 1000.0) as u32)),
            audio::TestPhase::Playing(p) => Some(("再生中…".to_string(), (p * 1000.0) as u32)),
            audio::TestPhase::Done => None,
        };
        let level = (running || recording).then(|| {
            (widgets::lit_segments(amp_to_db(self.meter)), widgets::lit_segments(amp_to_db(self.peak_hold)))
        });
        View {
            running,
            testing,
            lamp,
            status,
            limit: (self.limit_db > 0.5).then(|| format!("リミッター作動中 -{:.0} dB\nハウリングしている場合は音量を下げてください", self.limit_db)),
            message: self.message.clone(),
            resume: self.resume,
            volume: self.volume_pct.round() as u32,
            target_ms: self.target_ms,
            channel: self.channel,
            channels: self.monitor.as_ref().map_or(self.in_channels, |m| m.in_channels as u32),
            autostart: self.autostart,
            progress,
            take: self.test.as_ref().filter(|t| t.has_take()).map(|t| format!("録音の最大レベル {:.0} dB", amp_to_db(t.peak))),
            level,
        }
    }

    pub fn take_device_items(&mut self) -> Option<(DeviceItems, DeviceItems)> {
        std::mem::take(&mut self.devices_dirty).then(|| {
            (
                device_items(&self.input, &self.devices.inputs, self.devices.default_input()),
                device_items(&self.output, &self.devices.outputs, self.devices.default_output()),
            )
        })
    }

    fn picked(pick: Pick, list: &[audio::DeviceEntry]) -> Option<Choice> {
        match pick {
            Pick::Default => Some(Choice::Default),
            Pick::Entry(i) => list.get(i).map(|e| Choice::Device(e.clone())),
            Pick::Gone => None,
        }
    }

    pub fn pick_input(&mut self, pick: Pick) {
        if let Some(choice) = Self::picked(pick, &self.devices.inputs) {
            self.input = choice;
            self.devices_dirty = true;
            self.refresh_channels();
        }
    }

    pub fn pick_output(&mut self, pick: Pick) {
        if let Some(choice) = Self::picked(pick, &self.devices.outputs) {
            self.output = choice;
            self.devices_dirty = true;
        }
    }

    pub fn set_channel(&mut self, ch: u32) {
        self.channel = ch;
        if let Some(m) = &self.monitor {
            m.shared.set_channel(ch);
        }
        if let Some(t) = &self.test {
            t.shared.set_channel(ch);
        }
    }

    pub fn set_volume(&mut self, pct: u32) {
        self.volume_pct = pct as f32;
        if let Some(m) = &self.monitor {
            m.shared.set_volume(self.volume_pct / 100.0);
        }
        if let Some(t) = &self.test {
            t.shared.set_volume(self.volume_pct / 100.0);
        }
    }

    pub fn set_target_ms(&mut self, ms: u32) {
        self.target_ms = ms;
        if let Some(m) = &self.monitor {
            m.shared.set_target_ms(ms);
        }
    }
}

pub fn min_target_ms() -> u32 {
    audio::MIN_TARGET_MS
}

pub fn set_window_handle(hwnd: &w::HWND) {
    WINDOW.store(hwnd.ptr() as isize, Ordering::Relaxed);
}

fn missing_notice(input: &Choice, output: &Choice) -> Option<String> {
    let names: Vec<&str> = [input, output]
        .into_iter()
        .filter_map(|c| match c {
            Choice::Missing { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    (!names.is_empty()).then(|| format!("デバイスが見つかりません: {}", names.join(" / ")))
}

fn amp_to_db(amp: f32) -> f32 {
    20.0 * amp.max(1e-5).log10()
}

pub fn channel_label(ch: u32) -> String {
    match ch {
        0 => "ミックス（全チャンネルの平均）".to_string(),
        1 => "1ch（L）".to_string(),
        2 => "2ch（R）".to_string(),
        n => format!("{n}ch"),
    }
}

pub fn save_settings(text: &str) -> bool {
    settings_dir().is_some_and(|dir| save_settings_to(&dir, text).is_ok())
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let text = format!("{info}\n\n{}", std::backtrace::Backtrace::force_capture());
        let path = write_panic_log(&text);
        let where_ = path.map_or("（記録ファイルを書き込めませんでした）".to_string(), |p| format!("詳細な記録: {}", p.display()));
        show_error_box(&format!("エラーが発生しました\n\n{info}\n\n{where_}"));
    }));
}

fn write_panic_log(text: &str) -> Option<PathBuf> {
    use std::io::Write;
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for dir in settings_dir().into_iter().chain([std::env::temp_dir()]) {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{APP_ID}-panic.log"));
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        if let Ok(mut f) = file {
            if writeln!(f, "==== unix time {unix} ====\n{text}\n").is_ok() {
                return Some(path);
            }
        }
    }
    None
}

fn show_error_box(msg: &str) {
    let _ = w::HWND::NULL.MessageBox(msg, TITLE, co::MB::OK | co::MB::ICONERROR);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            input: Some("USB = Mic \\ A\nB\r".to_string()),
            output: Some(String::new()),
            input_id: None,
            output_id: Some("{0.0.0}.{id}".to_string()),
            channel: 3,
            volume_pct: 37.0,
            target_ms: 30,
            tray_notice_seen: true,
            window_pos: Some((-10, 20)),
        }
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings::parse(&sample().to_text()).unwrap();
        let want = sample();
        assert_eq!((s.input, s.output, s.input_id, s.output_id), (want.input, want.output, want.input_id, want.output_id));
        assert_eq!((s.channel, s.target_ms, s.tray_notice_seen, s.window_pos), (3, 30, true, Some((-10, 20))));
        assert_eq!(s.volume_pct, 37.0);
    }

    #[test]
    fn missing_settings_use_the_defaults() {
        let s = Settings::parse("output=x\n").unwrap();
        assert_eq!(s.output.as_deref(), Some("x"));
        assert_eq!((s.input, s.input_id, s.output_id, s.window_pos), (None, None, None, None));
        assert_eq!((s.channel, s.volume_pct, s.target_ms, s.tray_notice_seen), (0, DEFAULT_VOLUME_PCT, DEFAULT_TARGET_MS, false));
    }

    #[test]
    fn broken_values_keep_the_defaults() {
        let s = Settings::parse("channel=x\nvolume_pct=NaN\ntarget_ms=-1\nwindow_pos=1\nunknown=1\nnot a setting\n").unwrap();
        assert_eq!((s.channel, s.volume_pct, s.target_ms, s.window_pos), (0, DEFAULT_VOLUME_PCT, DEFAULT_TARGET_MS, None));
    }

    #[test]
    fn hand_edited_files_are_read() {
        let s = Settings::parse("\u{feff}output=a=b \r\nvolume_pct = 50 \r\nwindow_pos= 1, 2\r\n").unwrap();
        assert_eq!(s.output.as_deref(), Some("a=b "));
        assert_eq!((s.volume_pct, s.window_pos), (50.0, Some((1, 2))));
        assert_eq!(Settings::parse(r"input=C:\mic\").unwrap().input.as_deref(), Some(r"C:\mic\"));
    }

    #[test]
    fn volume_stays_in_range() {
        assert_eq!(Settings::parse("volume_pct=99999").unwrap().volume_pct, MAX_VOLUME_PCT);
        assert_eq!(Settings::parse("volume_pct=37.9").unwrap().volume_pct, 37.0);
        assert_eq!(Settings::parse("volume_pct=-5").unwrap().volume_pct, DEFAULT_VOLUME_PCT);
        assert_eq!(Settings::parse("volume_pct=inf").unwrap().volume_pct, DEFAULT_VOLUME_PCT);
    }

    #[test]
    fn missing_device_stays_in_settings() {
        let saved = Some("USB Audio".to_string());
        let id = Some("id".to_string());
        let mut choice = Choice::from_saved(&saved, &id, &[]);
        choice.refresh(&[]);
        assert!(!choice.is_default() && choice.resolve(&[], None).is_none());
        assert_eq!((choice.saved_label(), choice.saved_id()), (saved, id));

        let notice = missing_notice(&Choice::Default, &choice).unwrap();
        assert!(notice.contains("USB Audio"));
        assert!(missing_notice(&Choice::Default, &Choice::Default).is_none());
    }

    #[test]
    fn device_items_show_a_missing_choice_last() {
        let choice = Choice::Missing { label: "USB Audio".to_string(), id: None };
        let items = device_items(&choice, &[], None);
        assert_eq!(items.labels, ["既定に追従（今は なし）", "USB Audio（見つかりません）"]);
        assert!(items.picks == [Pick::Default, Pick::Gone]);
        assert_eq!(items.selected, 1);
        assert_eq!(device_items(&Choice::Default, &[], None).selected, 0);
    }

    #[test]
    fn channel_labels_name_the_first_two_channels() {
        assert!(channel_label(0).starts_with("ミックス"));
        assert_eq!(channel_label(1), "1ch（L）");
        assert_eq!(channel_label(3), "3ch");
    }

    #[test]
    fn settings_file_is_saved_and_replaced() {
        let dir = std::env::temp_dir().join(format!("{APP_ID}-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let volume = || read_settings(&dir).map(|s| s.volume_pct);
        assert_eq!(volume(), None);

        save_settings_to(&dir, "volume_pct=20\n").unwrap();
        assert_eq!(volume(), Some(20.0));
        save_settings_to(&dir, "volume_pct=30\n").unwrap();
        assert_eq!(volume(), Some(30.0));
        assert!(!dir.join(format!("{SETTINGS_FILE}.tmp")).exists());

        std::fs::write(dir.join(SETTINGS_FILE), "").unwrap();
        assert_eq!(volume(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_settings_are_ignored() {
        assert!(Settings::parse("").is_none());
        assert!(Settings::parse("(input:None,channel:0)").is_none());
        assert!(Settings::parse("unknown=1\n").is_none());
    }

    #[test]
    fn icons_have_the_requested_size_and_rounded_corners() {
        for glyph in [icon::Glyph::App, icon::Glyph::Stopped, icon::Glyph::Live] {
            for size in [16u32, 20, 32, 48] {
                let rgba = icon::render(glyph, size);
                assert_eq!(rgba.len(), (size * size * 4) as usize);
                assert!(rgba[3] < 128, "{glyph:?} {size}: corner should be transparent");
                let center = ((size / 2 * size + size / 2) * 4 + 3) as usize;
                assert_eq!(rgba[center], 255, "{glyph:?} {size}: center should be opaque");
            }
        }
    }

    #[test]
    fn tray_states_look_different() {
        assert_ne!(icon::render(icon::Glyph::Stopped, 20), icon::render(icon::Glyph::Live, 20));
    }
}
