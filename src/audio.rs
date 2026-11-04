use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, Sample, SampleFormat, SizedSample, I24};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use windows::core::PCWSTR;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    DEVICE_STATE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

pub struct Shared {
    volume: AtomicU32,
    target_ms: AtomicU32,
    channel: AtomicU32,
    peak: AtomicU32,
    fill_frames: AtomicU32,
    underruns: AtomicU32,
    in_callbacks: AtomicU32,
    out_callbacks: AtomicU32,
    in_period_frames: AtomicU32,
    out_period_frames: AtomicU32,
    limit_gain: AtomicU32,
    position: AtomicU32,
    error: Mutex<Option<String>>,
    started: Instant,
}

fn load_f32(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store_f32(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

impl Shared {
    fn new(volume: f32, target_ms: u32, channel: u32) -> Self {
        Self {
            volume: AtomicU32::new(volume.to_bits()),
            target_ms: AtomicU32::new(target_ms),
            channel: AtomicU32::new(channel),
            peak: AtomicU32::new(0f32.to_bits()),
            fill_frames: AtomicU32::new(0),
            underruns: AtomicU32::new(0),
            in_callbacks: AtomicU32::new(0),
            out_callbacks: AtomicU32::new(0),
            in_period_frames: AtomicU32::new(u32::MAX),
            out_period_frames: AtomicU32::new(u32::MAX),
            limit_gain: AtomicU32::new(1f32.to_bits()),
            position: AtomicU32::new(0),
            error: Mutex::new(None),
            started: Instant::now(),
        }
    }

    pub fn set_volume(&self, v: f32) {
        store_f32(&self.volume, v);
    }

    pub fn set_target_ms(&self, ms: u32) {
        self.target_ms.store(ms, Ordering::Relaxed);
    }

    pub fn set_channel(&self, ch: u32) {
        self.channel.store(ch, Ordering::Relaxed);
    }

    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0f32.to_bits(), Ordering::Relaxed))
    }

    pub fn take_limit_db(&self) -> f32 {
        -20.0 * f32::from_bits(self.limit_gain.swap(1f32.to_bits(), Ordering::Relaxed)).log10()
    }

    fn period_floor_frames(&self) -> usize {
        let read = |a: &AtomicU32| match a.load(Ordering::Relaxed) {
            u32::MAX => 0,
            n => n as usize,
        };
        read(&self.in_period_frames).max(read(&self.out_period_frames))
    }

    #[cfg(test)]
    pub fn underruns(&self) -> u32 {
        self.underruns.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn set_error(&self, msg: String) {
        let mut slot = self.error.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(msg);
    }

    fn bump_limit(&self, gain: f32) {
        let _ = self
            .limit_gain
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (gain < f32::from_bits(old)).then_some(gain.to_bits())
            });
    }

    fn bump_peak(&self, v: f32) {
        let _ = self
            .peak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (v > f32::from_bits(old)).then_some(v.to_bits())
            });
    }
}

fn endpoint_id(device: &cpal::Device) -> Option<String> {
    #[allow(unreachable_patterns)]
    let device = match device.as_inner() {
        cpal::platform::DeviceInner::Wasapi(d) => d.immdevice()?,
        _ => return None,
    };
    unsafe {
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

#[derive(Clone)]
pub struct DeviceEntry {
    pub device: cpal::Device,
    pub label: String,
    pub id: Option<String>,
}

#[derive(Clone)]
pub struct DeviceList {
    pub inputs: Vec<DeviceEntry>,
    pub outputs: Vec<DeviceEntry>,
    pub default_input: Option<usize>,
    pub default_output: Option<usize>,
}

impl DeviceList {
    pub fn default_input(&self) -> Option<&DeviceEntry> {
        self.default_input.map(|i| &self.inputs[i])
    }

    pub fn default_output(&self) -> Option<&DeviceEntry> {
        self.default_output.map(|i| &self.outputs[i])
    }

    fn same_as(&self, other: &DeviceList) -> bool {
        fn same(a: &[DeviceEntry], b: &[DeviceEntry]) -> bool {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.label == y.label && x.device == y.device)
        }
        same(&self.inputs, &other.inputs)
            && same(&self.outputs, &other.outputs)
            && self.default_input == other.default_input
            && self.default_output == other.default_output
    }
}

pub fn find_entry<'a>(list: &'a [DeviceEntry], device: &cpal::Device) -> Option<&'a DeviceEntry> {
    list.iter().find(|e| e.device == *device)
}

fn entries(devs: Option<impl Iterator<Item = cpal::Device>>) -> Vec<DeviceEntry> {
    let devs: Vec<(cpal::Device, String)> = devs
        .into_iter()
        .flatten()
        .filter_map(|d| d.description().ok().map(|desc| (d, desc.name().to_string())))
        .collect();
    devs.iter()
        .enumerate()
        .map(|(i, (device, name))| {
            let total = devs.iter().filter(|(_, n)| n == name).count();
            let nth = devs[..i].iter().filter(|(_, n)| n == name).count() + 1;
            let label = if total > 1 { format!("{name} ({nth})") } else { name.clone() };
            DeviceEntry { device: device.clone(), label, id: endpoint_id(device) }
        })
        .collect()
}

pub fn list_devices() -> DeviceList {
    let host = cpal::default_host();
    let inputs = entries(host.input_devices().ok());
    let outputs = entries(host.output_devices().ok());
    let default_input = default_position(&inputs, host.default_input_device());
    let default_output = default_position(&outputs, host.default_output_device());
    DeviceList { inputs, outputs, default_input, default_output }
}

fn default_position(list: &[DeviceEntry], default: Option<cpal::Device>) -> Option<usize> {
    let id = endpoint_id(&default?)?;
    list.iter().position(|e| e.id.as_ref() == Some(&id))
}

struct DeviceEvents {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl DeviceEvents {
    fn register(wake: mpsc::Sender<()>) -> windows::core::Result<Self> {
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let client: IMMNotificationClient = DeviceNotifier { wake }.into();
            enumerator.RegisterEndpointNotificationCallback(&client)?;
            Ok(Self { enumerator, client })
        }
    }
}

impl Drop for DeviceEvents {
    fn drop(&mut self) {
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}

#[windows::core::implement(IMMNotificationClient)]
struct DeviceNotifier {
    wake: mpsc::Sender<()>,
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for DeviceNotifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.wake.send(());
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

const DEVICE_RECHECK: Duration = Duration::from_secs(60);
const DEVICE_POLL: Duration = Duration::from_secs(2);
const DEVICE_SETTLE: Duration = Duration::from_millis(150);

pub fn watch_devices(on_change: impl Fn() + Send + 'static) -> mpsc::Receiver<DeviceList> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut last = list_devices();
        let (wake_tx, wake) = mpsc::channel();
        let events = DeviceEvents::register(wake_tx.clone()).ok();
        let wait = if events.is_some() { DEVICE_RECHECK } else { DEVICE_POLL };
        loop {
            if wake.recv_timeout(wait).is_ok() {
                while wake.recv_timeout(DEVICE_SETTLE).is_ok() {}
            }
            let list = list_devices();
            if !list.same_as(&last) {
                if tx.send(list.clone()).is_err() {
                    break;
                }
                on_change();
                last = list;
            }
        }
    });
    rx
}

pub fn input_channels(device: &cpal::Device) -> Option<u32> {
    device.default_input_config().ok().map(|c| c.channels() as u32)
}

pub const MIN_TARGET_MS: u32 = 10;

pub struct Monitor {
    _input: cpal::Stream,
    _output: cpal::Stream,
    pub input_device: cpal::Device,
    pub output_device: cpal::Device,
    pub shared: Arc<Shared>,
    #[cfg(test)]
    pub in_rate: u32,
    pub in_channels: u16,
    watch: StallWatch,
}

const STALL_LIMIT: Duration = Duration::from_secs(3);
const STALL_START_LIMIT: Duration = Duration::from_secs(10);

struct StallWatch {
    counts: [u32; 2],
    moved_at: [Instant; 2],
    checked_at: Instant,
}

impl StallWatch {
    fn new(now: Instant) -> Self {
        Self { counts: [0; 2], moved_at: [now; 2], checked_at: now }
    }

    fn check(&mut self, counts: [u32; 2], now: Instant) -> bool {
        if now.duration_since(self.checked_at) >= STALL_LIMIT {
            self.moved_at = [now; 2];
        }
        self.checked_at = now;
        let mut stalled = false;
        for (i, count) in counts.into_iter().enumerate() {
            if count != self.counts[i] {
                self.counts[i] = count;
                self.moved_at[i] = now;
            }
            let limit = if self.counts[i] == 0 { STALL_START_LIMIT } else { STALL_LIMIT };
            stalled |= now.duration_since(self.moved_at[i]) >= limit;
        }
        stalled
    }
}

impl Monitor {
    pub fn is_stalled(&mut self) -> bool {
        let counts = [self.shared.in_callbacks.load(Ordering::Relaxed), self.shared.out_callbacks.load(Ordering::Relaxed)];
        self.watch.check(counts, Instant::now())
    }

    pub fn uptime(&self) -> Duration {
        self.shared.started.elapsed()
    }

    pub fn abandon(self) {
        std::thread::spawn(move || drop(self));
    }

    #[cfg(test)]
    pub fn buffer_ms(&self) -> f32 {
        self.shared.fill_frames.load(Ordering::Relaxed) as f32 * 1000.0 / self.in_rate as f32
    }

    #[cfg(test)]
    pub fn floor_ms(&self) -> f32 {
        self.shared.period_floor_frames() as f32 * 1000.0 / self.in_rate as f32
    }
}

pub fn start(
    input: &cpal::Device,
    output: &cpal::Device,
    volume: f32,
    target_ms: u32,
    channel: u32,
) -> Result<Monitor, String> {
    let in_cfg = input.default_input_config().map_err(|e| explain(StreamKind::Input, &e))?;
    let out_cfg = output.default_output_config().map_err(|e| explain(StreamKind::Output, &e))?;
    let in_rate = in_cfg.sample_rate();

    let rb = HeapRb::<f32>::new(in_rate as usize / 2);
    let (prod, cons) = rb.split();
    let shared = Arc::new(Shared::new(volume, target_ms, channel));

    let in_stream = build_input(input, &in_cfg, prod, shared.clone())?;
    let out_stream = build_output(output, &out_cfg, cons, in_rate, shared.clone())?;

    in_stream.play().map_err(|e| explain(StreamKind::Input, &e))?;
    out_stream.play().map_err(|e| explain(StreamKind::Output, &e))?;

    Ok(Monitor {
        _input: in_stream,
        _output: out_stream,
        input_device: input.clone(),
        output_device: output.clone(),
        shared,
        #[cfg(test)]
        in_rate,
        in_channels: in_cfg.channels(),
        watch: StallWatch::new(Instant::now()),
    })
}

#[derive(Clone, Copy)]
pub enum TestPhase {
    Recording(f32),
    Playing(f32),
    Done,
}

pub struct LoopTest {
    input: Option<cpal::Stream>,
    output: Option<cpal::Stream>,
    cons: HeapCons<f32>,
    capacity: usize,
    in_rate: u32,
    out_device: cpal::Device,
    raw: Vec<f32>,
    take: Arc<Vec<f32>>,
    pub shared: Arc<Shared>,
    pub peak: f32,
}

impl LoopTest {
    pub fn start(
        input: &cpal::Device,
        output: &cpal::Device,
        volume: f32,
        channel: u32,
        secs: f32,
    ) -> Result<Self, String> {
        let in_cfg = input.default_input_config().map_err(|e| explain(StreamKind::Input, &e))?;
        output.default_output_config().map_err(|e| explain(StreamKind::Output, &e))?;
        let in_rate = in_cfg.sample_rate();

        let capacity = (in_rate as f32 * secs) as usize;
        let (prod, cons) = HeapRb::<f32>::new(capacity).split();
        let shared = Arc::new(Shared::new(volume, 0, channel));

        let stream = build_input(input, &in_cfg, prod, shared.clone())?;
        stream.play().map_err(|e| explain(StreamKind::Input, &e))?;

        Ok(Self {
            input: Some(stream),
            output: None,
            cons,
            capacity,
            in_rate,
            out_device: output.clone(),
            raw: Vec::new(),
            take: Arc::new(Vec::new()),
            shared,
            peak: 0.0,
        })
    }

    pub fn poll(&mut self) -> Result<TestPhase, String> {
        if let Some(e) = self.shared.error() {
            self.abort();
            return Err(e);
        }
        if self.input.is_some() {
            let fill = self.cons.occupied_len();
            if fill < self.capacity {
                return Ok(TestPhase::Recording(fill as f32 / self.capacity as f32));
            }
            self.input = None;
            self.raw = self.cons.pop_iter().collect();
            self.peak = self.raw.iter().fold(0f32, |m, s| m.max(s.abs()));
            self.play()?;
            return Ok(TestPhase::Playing(0.0));
        }
        if self.output.is_some() {
            let pos = self.shared.position.load(Ordering::Relaxed) as usize;
            if pos < self.take.len() {
                return Ok(TestPhase::Playing(pos as f32 / self.take.len() as f32));
            }
            self.stop_playing();
        }
        Ok(TestPhase::Done)
    }

    pub fn replay(&mut self, output: &cpal::Device) -> Result<(), String> {
        self.out_device = output.clone();
        self.play()
    }

    fn play(&mut self) -> Result<(), String> {
        self.stop_playing();
        let cfg = self.out_device.default_output_config().map_err(|e| explain(StreamKind::Output, &e))?;
        self.take = Arc::new(playback_take(&self.raw, self.in_rate, cfg.sample_rate()));
        self.shared.position.store(0, Ordering::Relaxed);
        let stream = build_playback(&self.out_device, &cfg, self.take.clone(), self.shared.clone())?;
        stream.play().map_err(|e| explain(StreamKind::Output, &e))?;
        self.output = Some(stream);
        Ok(())
    }

    fn stop_playing(&mut self) {
        self.output = None;
        self.take = Arc::new(Vec::new());
    }

    pub fn abort(&mut self) {
        self.input = None;
        self.stop_playing();
    }

    pub fn is_busy(&self) -> bool {
        self.input.is_some() || self.output.is_some()
    }

    pub fn has_take(&self) -> bool {
        !self.raw.is_empty()
    }
}

fn playback_take(raw: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    let mut take = render_take(raw, in_rate, out_rate);
    take.resize(take.len() + (out_rate as f32 * PLAYBACK_TAIL_MS / 1000.0) as usize, 0.0);
    take
}

fn render_take(raw: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    let table = SincTable::new(in_rate, out_rate);
    let step = in_rate as f64 / out_rate as f64;
    let mut hist = [0f32; TAPS];
    let mut t = 0f64;
    let mut src = raw.iter().copied().chain(std::iter::repeat_n(0.0, TAPS));
    let mut out = Vec::with_capacity((raw.len() as f64 / step) as usize + TAPS);
    'render: loop {
        out.push(table.interp(&hist, t));
        t += step;
        while t >= 1.0 {
            t -= 1.0;
            match src.next() {
                Some(s) => {
                    hist.copy_within(1.., 0);
                    hist[TAPS - 1] = s;
                }
                None => break 'render,
            }
        }
    }
    let fade = (out_rate as f32 * FADE_MS / 1000.0) as usize;
    let n = out.len();
    for i in 0..fade.min(n / 2) {
        let g = i as f32 / fade as f32;
        out[i] *= g;
        out[n - 1 - i] *= g;
    }
    out
}

const ACCESS_DENIED: &str = "os error -2147024891";

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Input,
    Output,
}

impl std::fmt::Display for StreamKind {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(if *self == StreamKind::Input { "録音" } else { "再生" })
    }
}

fn explain(kind: StreamKind, e: &cpal::Error) -> String {
    let raw = e.to_string();
    if e.kind() == ErrorKind::PermissionDenied || raw.contains(ACCESS_DENIED) {
        if kind == StreamKind::Input {
            format!(
                "マイクへのアクセスが拒否されました\n「設定 → プライバシーとセキュリティ → マイク」で\n「デスクトップ アプリがマイクにアクセスできるようにする」がオンか確認してください\n（{raw}）"
            )
        } else {
            format!("{kind}デバイスへのアクセスが拒否されました（{raw}）")
        }
    } else if e.kind() == ErrorKind::DeviceBusy {
        format!("{kind}デバイスは他のアプリが排他モードで使用しています\n（{raw}）")
    } else if e.kind() == ErrorKind::DeviceNotAvailable {
        format!("{kind}デバイスが見つかりません")
    } else {
        format!("{kind}エラー: {raw}")
    }
}

const XRUN_SETTLE: Duration = Duration::from_secs(1);

fn on_stream_error(shared: &Shared, kind: StreamKind, e: cpal::Error) {
    if e.kind() == ErrorKind::Xrun {
        if shared.started.elapsed() >= XRUN_SETTLE {
            shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        shared.set_error(explain(kind, &e));
    }
}

struct MmcssTask {
    boosted: std::cell::Cell<bool>,
    handle: std::cell::Cell<windows_sys::Win32::Foundation::HANDLE>,
}

impl Drop for MmcssTask {
    fn drop(&mut self) {
        let handle = self.handle.get();
        if !handle.is_null() {
            unsafe {
                windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle);
            }
        }
    }
}

fn boost_thread_once() {
    thread_local!(static TASK: MmcssTask = const { MmcssTask { boosted: std::cell::Cell::new(false), handle: std::cell::Cell::new(std::ptr::null_mut()) } });
    TASK.with(|task| {
        if task.boosted.replace(true) {
            return;
        }
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        task.handle.set(unsafe { windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) });
    });
}

macro_rules! by_format {
    ($kind:expr, $cfg:expr, $f:ident, $($arg:expr),*) => {
        match $cfg.sample_format() {
            SampleFormat::I16 => $f::<i16>($($arg),*),
            SampleFormat::I24 => $f::<I24>($($arg),*),
            SampleFormat::I32 => $f::<i32>($($arg),*),
            SampleFormat::I64 => $f::<i64>($($arg),*),
            SampleFormat::U8 => $f::<u8>($($arg),*),
            SampleFormat::F32 => $f::<f32>($($arg),*),
            SampleFormat::F64 => $f::<f64>($($arg),*),
            other => return Err(format!("{}デバイスのサンプル形式 {other:?} には対応していません", $kind)),
        }
        .map_err(|e| explain($kind, &e))
    };
}

fn build_input(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    prod: HeapProd<f32>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    by_format!(StreamKind::Input, cfg, input_stream, device, cfg, prod, shared)
}

fn input_stream<T>(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    mut prod: HeapProd<f32>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = cfg.channels() as usize;
    let mut period = PeriodEstimate::new();
    let err_shared = shared.clone();
    device.build_input_stream(
        cfg.config(),
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            boost_thread_once();
            shared.in_callbacks.fetch_add(1, Ordering::Relaxed);
            shared.in_period_frames.store(period.update((data.len() / channels) as u32), Ordering::Relaxed);
            let ch = shared.channel.load(Ordering::Relaxed) as usize;
            let mut peak = 0f32;
            for frame in data.chunks(channels) {
                let s = clean_sample(if (1..=frame.len()).contains(&ch) {
                    f32::from_sample(frame[ch - 1])
                } else {
                    frame.iter().map(|x| f32::from_sample(*x)).sum::<f32>() / frame.len() as f32
                });
                peak = peak.max(s.abs());
                let _ = prod.try_push(s);
            }
            shared.bump_peak(peak);
        },
        move |e| on_stream_error(&err_shared, StreamKind::Input, e),
        None,
    )
}

fn clean_sample(s: f32) -> f32 {
    if s.is_finite() {
        s
    } else {
        0.0
    }
}

fn build_output(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    cons: HeapCons<f32>,
    in_rate: u32,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    by_format!(StreamKind::Output, cfg, output_stream, device, cfg, cons, in_rate, shared)
}

fn build_playback(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    take: Arc<Vec<f32>>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    by_format!(StreamKind::Output, cfg, playback_stream, device, cfg, take, shared)
}

fn playback_stream<T>(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    take: Arc<Vec<f32>>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = cfg.channels() as usize;
    let coef = volume_coef(cfg.sample_rate());
    let mut volume = load_f32(&shared.volume);
    let mut limiter = Limiter::new(cfg.sample_rate());
    let mut pos = 0usize;
    let err_shared = shared.clone();
    device.build_output_stream(
        cfg.config(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            boost_thread_once();
            let target = load_f32(&shared.volume);
            for frame in data.chunks_mut(channels) {
                volume = smooth_volume(volume, target, coef);
                let v = take.get(pos).map_or(0.0, |s| limiter.process(s * volume).clamp(-1.0, 1.0));
                pos += 1;
                let out = T::from_sample(v);
                for s in frame.iter_mut() {
                    *s = out;
                }
            }
            shared.position.store(pos.min(take.len()) as u32, Ordering::Relaxed);
        },
        move |e| on_stream_error(&err_shared, StreamKind::Output, e),
        None,
    )
}

fn volume_coef(rate: u32) -> f32 {
    1.0 - (-1.0 / (rate as f32 * VOLUME_SMOOTH_MS / 1000.0)).exp()
}

fn smooth_volume(volume: f32, goal: f32, coef: f32) -> f32 {
    let next = volume + (goal - volume) * coef;
    if (goal - next).abs() < VOLUME_SNAP {
        goal
    } else {
        next
    }
}

const TAPS: usize = 32;
const PHASES: usize = 256;
const FADE_MS: f32 = 4.0;
const VOLUME_SMOOTH_MS: f32 = 15.0;
const VOLUME_SNAP: f32 = 1e-4;
const LIMITER_CEILING: f32 = 0.708;
const LIMITER_RELEASE_MS: f32 = 100.0;
const PLAYBACK_TAIL_MS: f32 = 150.0;
const RETARGET_BAND_MS: f32 = 10.0;
const RETARGET_SETTLE_MS: f32 = 200.0;
const PERIOD_WINDOW: usize = 32;

struct SincTable {
    rows: Vec<[f32; TAPS]>,
}

impl SincTable {
    fn new(in_rate: u32, out_rate: u32) -> Self {
        use std::f64::consts::PI;
        let fc = 0.5 * (out_rate as f64 / in_rate as f64).min(1.0) * 0.92;
        let half = (TAPS / 2) as f64;
        let rows = (0..=PHASES)
            .map(|p| {
                let t = p as f64 / PHASES as f64;
                let mut row = [0f64; TAPS];
                for (k, h) in row.iter_mut().enumerate() {
                    let x = k as f64 - (half - 1.0) - t;
                    let sinc = if x == 0.0 { 1.0 } else { (2.0 * PI * fc * x).sin() / (2.0 * PI * fc * x) };
                    let w = 0.42 + 0.5 * (PI * x / half).cos() + 0.08 * (2.0 * PI * x / half).cos();
                    *h = sinc * w;
                }
                let sum: f64 = row.iter().sum();
                row.map(|h| (h / sum) as f32)
            })
            .collect();
        Self { rows }
    }

    fn interp(&self, hist: &[f32; TAPS], t: f64) -> f32 {
        let pos = t * PHASES as f64;
        let i = (pos as usize).min(PHASES - 1);
        let f = (pos - i as f64) as f32;
        let (a, b) = (&self.rows[i], &self.rows[i + 1]);
        let mut acc = 0f32;
        for k in 0..TAPS {
            acc += hist[k] * (a[k] + (b[k] - a[k]) * f);
        }
        acc
    }
}

struct Limiter {
    gain: f32,
    release: f32,
}

impl Limiter {
    fn new(rate: u32) -> Self {
        Self { gain: 1.0, release: 1.0 - (-1.0 / (rate as f32 * LIMITER_RELEASE_MS / 1000.0)).exp() }
    }

    fn process(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let needed = if a > LIMITER_CEILING { LIMITER_CEILING / a } else { 1.0 };
        if needed < self.gain {
            self.gain = needed;
        } else {
            self.gain += (needed - self.gain) * self.release;
        }
        x * self.gain
    }
}

struct PeriodEstimate {
    recent: [u32; PERIOD_WINDOW],
    count: usize,
}

impl PeriodEstimate {
    fn new() -> Self {
        Self { recent: [0; PERIOD_WINDOW], count: 0 }
    }

    fn update(&mut self, frames: u32) -> u32 {
        self.recent[self.count % PERIOD_WINDOW] = frames;
        self.count += 1;
        let mut sorted = self.recent;
        let filled = &mut sorted[..self.count.min(PERIOD_WINDOW)];
        filled.sort_unstable();
        filled[filled.len() * 3 / 4]
    }
}

#[derive(Clone, Copy)]
enum Pending {
    Skip,
    Reprime,
}

struct Player {
    cons: HeapCons<f32>,
    shared: Arc<Shared>,
    in_rate: u32,
    out_rate: u32,
    table: SincTable,
    hist: [f32; TAPS],
    t: f64,
    base_step: f64,
    step: f64,
    primed: bool,
    pending: Option<Pending>,
    target: usize,
    aligned_target: usize,
    settle_left: usize,
    settle_frames: usize,
    retarget_band: usize,
    period: PeriodEstimate,
    gain: f32,
    fade_step: f32,
    volume: f32,
    volume_goal: f32,
    volume_coef: f32,
    last: f32,
    limiter: Limiter,
    min_limit_gain: f32,
}

impl Player {
    fn new(cons: HeapCons<f32>, in_rate: u32, out_rate: u32, shared: Arc<Shared>) -> Self {
        Self {
            cons,
            in_rate,
            out_rate,
            table: SincTable::new(in_rate, out_rate),
            hist: [0.0; TAPS],
            t: 0.0,
            base_step: in_rate as f64 / out_rate as f64,
            step: 1.0,
            primed: false,
            pending: None,
            target: 0,
            aligned_target: 0,
            settle_left: 0,
            settle_frames: (out_rate as f32 * RETARGET_SETTLE_MS / 1000.0) as usize,
            retarget_band: (in_rate as f32 * RETARGET_BAND_MS / 1000.0) as usize,
            period: PeriodEstimate::new(),
            gain: 0.0,
            fade_step: 1.0 / (out_rate as f32 * FADE_MS / 1000.0),
            volume: load_f32(&shared.volume),
            volume_goal: 0.0,
            volume_coef: volume_coef(out_rate),
            last: 0.0,
            limiter: Limiter::new(out_rate),
            min_limit_gain: 1.0,
            shared,
        }
    }

    fn reset_history(&mut self) {
        self.hist = [0.0; TAPS];
        self.t = 0.0;
    }

    fn push(&mut self, s: f32) {
        self.hist.copy_within(1.., 0);
        self.hist[TAPS - 1] = s;
    }

    fn begin_block(&mut self, out_frames: usize) {
        let period = (out_frames as u64 * self.in_rate as u64 / self.out_rate as u64) as u32;
        let period = self.period.update(period);
        self.shared.out_period_frames.store(period, Ordering::Relaxed);

        let wanted = self.shared.target_ms.load(Ordering::Relaxed) as usize * self.in_rate as usize / 1000;
        let target = wanted.max(self.shared.period_floor_frames()).max(1);
        let fill = self.cons.occupied_len();
        self.shared.fill_frames.store(fill as u32, Ordering::Relaxed);

        if target != self.target {
            self.target = target;
            self.settle_left = self.settle_frames;
        } else {
            self.settle_left = self.settle_left.saturating_sub(out_frames);
        }
        if !self.primed {
            self.aligned_target = target;
        } else if self.pending.is_none() && self.settle_left == 0 {
            if target.abs_diff(self.aligned_target) > self.retarget_band {
                self.aligned_target = target;
                self.pending = Some(if fill > target { Pending::Skip } else { Pending::Reprime });
            } else if fill > target * 3 {
                self.pending = Some(Pending::Skip);
            }
        }
        if !self.primed && self.pending.is_none() && fill >= target {
            self.primed = true;
            self.reset_history();
        }

        self.volume_goal = load_f32(&self.shared.volume);
        let err = (fill as f64 - target as f64) / target as f64;
        self.step = self.base_step * (1.0 + (err * 0.005).clamp(-0.01, 0.01));
        self.min_limit_gain = 1.0;
    }

    fn next_sample(&mut self) -> f32 {
        let goal = if self.primed && self.pending.is_none() { 1.0 } else { 0.0 };
        self.gain = if self.gain < goal { (self.gain + self.fade_step).min(goal) } else { (self.gain - self.fade_step).max(goal) };
        self.volume = smooth_volume(self.volume, self.volume_goal, self.volume_coef);

        let v = if self.primed {
            let v = self.table.interp(&self.hist, self.t);
            self.t += self.step;
            while self.t >= 1.0 {
                self.t -= 1.0;
                match self.cons.try_pop() {
                    Some(s) => self.push(s),
                    None => {
                        self.primed = false;
                        self.pending = None;
                        self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
            self.last = v;
            v
        } else {
            self.last
        };

        if self.gain == 0.0 {
            match self.pending.take() {
                Some(Pending::Skip) => {
                    let excess = self.cons.occupied_len().saturating_sub(self.target);
                    self.cons.skip(excess);
                    self.reset_history();
                }
                Some(Pending::Reprime) => self.primed = false,
                None => {}
            }
        }

        let limited = self.limiter.process(v * self.gain * self.volume);
        self.min_limit_gain = self.min_limit_gain.min(self.limiter.gain);
        limited.clamp(-1.0, 1.0)
    }

    fn end_block(&mut self) {
        self.shared.bump_limit(self.min_limit_gain);
    }
}

fn output_stream<T>(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    cons: HeapCons<f32>,
    in_rate: u32,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = cfg.channels() as usize;
    let mut p = Player::new(cons, in_rate, cfg.sample_rate(), shared.clone());
    let err_shared = shared.clone();
    device.build_output_stream(
        cfg.config(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            boost_thread_once();
            shared.out_callbacks.fetch_add(1, Ordering::Relaxed);

            p.begin_block(data.len() / channels);
            for frame in data.chunks_mut(channels) {
                let out = T::from_sample(p.next_sample());
                for s in frame.iter_mut() {
                    *s = out;
                }
            }
            p.end_block();
        },
        move |e| on_stream_error(&err_shared, StreamKind::Output, e),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_sine(in_rate: u32, out_rate: u32) {
        let raw: Vec<f32> = (0..in_rate)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / in_rate as f32).sin() * 0.5)
            .collect();
        let out = render_take(&raw, in_rate, out_rate);
        assert!((out.len() as i64 - out_rate as i64).abs() < 64, "len = {}", out.len());
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak = {peak}");
    }

    #[test]
    fn render_keeps_length_and_level() {
        check_sine(48000, 48000);
        check_sine(44100, 48000);
        check_sine(48000, 44100);
    }

    #[test]
    fn render_rejects_aliasing() {
        let (in_rate, out_rate) = (96000u32, 44100u32);
        let raw: Vec<f32> = (0..in_rate)
            .map(|i| (2.0 * std::f32::consts::PI * 30000.0 * i as f32 / in_rate as f32).sin() * 0.5)
            .collect();
        let out = render_take(&raw, in_rate, out_rate);
        let peak = out[1000..out.len() - 1000].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.01, "peak = {peak}");
    }

    #[test]
    fn playback_take_has_a_silent_tail() {
        let raw = vec![0.5; 48000];
        for out_rate in [44100u32, 48000] {
            let take = playback_take(&raw, 48000, out_rate);
            let tail = (out_rate as f32 * PLAYBACK_TAIL_MS / 1000.0) as usize;
            assert_eq!(take.len(), render_take(&raw, 48000, out_rate).len() + tail);
            assert!(take[take.len() - tail..].iter().all(|s| *s == 0.0));
        }
    }

    #[test]
    fn limiter_caps_loud_and_keeps_quiet() {
        let sine = |amp: f32, i: usize| (i as f32 * 0.13).sin() * amp;
        let mut l = Limiter::new(48000);
        let peak = (0..48000).map(|i| l.process(sine(2.0, i)).abs()).fold(0f32, f32::max);
        assert!(peak <= LIMITER_CEILING + 1e-4, "peak = {peak}");

        let mut l = Limiter::new(48000);
        for i in 0..48000 {
            assert_eq!(l.process(sine(0.5, i)), sine(0.5, i));
        }
    }

    struct Rig {
        p: Player,
        prod: HeapProd<f32>,
        shared: Arc<Shared>,
        cuts: u32,
        was_full: bool,
    }

    const BLOCK: usize = 480;

    impl Rig {
        fn new(target_ms: u32) -> Self {
            let (prod, cons) = HeapRb::<f32>::new(48000).split();
            let shared = Arc::new(Shared::new(1.0, target_ms, 0));
            let p = Player::new(cons, 48000, 48000, shared.clone());
            Self { p, prod, shared, cuts: 0, was_full: false }
        }

        fn input(&mut self, frames: usize, s: f32) {
            for _ in 0..frames {
                self.prod.try_push(s).unwrap();
            }
        }

        fn output(&mut self, frames: usize) -> Vec<f32> {
            self.p.begin_block(frames);
            let out: Vec<f32> = (0..frames)
                .map(|_| {
                    let v = self.p.next_sample();
                    let full = self.p.gain == 1.0;
                    if self.was_full && !full {
                        self.cuts += 1;
                    }
                    self.was_full = full;
                    v
                })
                .collect();
            self.p.end_block();
            out
        }

        fn run(&mut self, blocks: usize) {
            for _ in 0..blocks {
                self.input(BLOCK, 0.1);
                self.output(BLOCK);
            }
        }

        fn primed(target_ms: u32) -> Self {
            let mut r = Self::new(target_ms);
            r.input(target_ms as usize * 48, 0.1);
            r.run(40);
            assert!(r.p.primed && r.p.gain == 1.0);
            r
        }

        fn fill_ms(&self) -> f32 {
            self.p.cons.occupied_len() as f32 / 48.0
        }
    }

    #[test]
    fn player_waits_for_target_then_plays() {
        let mut r = Rig::new(30);
        r.input(29 * 48, 0.1);
        assert!(r.output(BLOCK).iter().all(|v| *v == 0.0));
        assert!(!r.p.primed);
        r.input(48, 0.1);
        r.run(20);
        assert!(r.p.primed);
        let last = *r.output(BLOCK).last().unwrap();
        assert!((last - 0.1).abs() < 1e-3, "last = {last}");
        assert_eq!(r.cuts, 0);
    }

    #[test]
    fn player_underrun_fades_and_counts_once() {
        let mut r = Rig::primed(20);
        let out = r.output(48000 / 10);
        assert_eq!(r.shared.underruns(), 1);
        assert!(!r.p.primed);
        let max_jump = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(max_jump <= 0.1 * r.p.fade_step + 1e-3, "jump = {max_jump}");
        assert_eq!(*out.last().unwrap(), 0.0);
        r.input(20 * 48, 0.1);
        r.run(10);
        assert!(r.p.primed && r.p.gain == 1.0);
    }

    #[test]
    fn dragging_target_up_cuts_once_after_release() {
        let mut r = Rig::primed(20);
        for ms in 21..=100 {
            r.shared.set_target_ms(ms);
            r.run(1);
        }
        assert_eq!(r.cuts, 0, "must not cut while dragging");
        r.run(100);
        assert_eq!(r.cuts, 1, "must realign exactly once after release");
        assert!(r.p.primed);
        assert!((r.fill_ms() - 100.0).abs() < 15.0, "fill = {}", r.fill_ms());
    }

    #[test]
    fn dragging_target_down_cuts_once_after_release() {
        let mut r = Rig::primed(100);
        for ms in (10..100).rev() {
            r.shared.set_target_ms(ms);
            r.run(1);
        }
        assert_eq!(r.cuts, 0, "must not cut while dragging");
        r.run(100);
        assert_eq!(r.cuts, 1, "must realign exactly once after release");
        assert!(r.fill_ms() < 15.0, "fill = {}", r.fill_ms());
    }

    #[test]
    fn small_target_change_is_chased_without_cut() {
        let mut r = Rig::primed(30);
        r.shared.set_target_ms(25);
        r.run(2000);
        assert_eq!(r.cuts, 0);
        assert!(r.fill_ms() < 26.0, "fill = {}", r.fill_ms());
    }

    #[test]
    fn floor_wobble_does_not_cut() {
        let mut r = Rig::primed(10);
        for frames in [960u32, 480, 960, 720, 480, 600, 480] {
            r.shared.in_period_frames.store(frames, Ordering::Relaxed);
            r.run(5);
        }
        r.run(100);
        assert_eq!(r.cuts, 0);
    }

    #[test]
    fn overfull_buffer_is_skipped_back_to_target() {
        let mut r = Rig::primed(20);
        r.input(200 * 48, 0.1);
        r.run(5);
        assert_eq!(r.cuts, 1);
        assert!(r.fill_ms() < 25.0, "fill = {}", r.fill_ms());
        r.run(5);
        assert!(r.p.gain == 1.0);
    }

    #[test]
    fn silent_output_settles_to_exact_zero() {
        let mut r = Rig::primed(20);
        r.shared.set_volume(0.0);
        r.run(100);
        assert_eq!(r.p.volume, 0.0);
        assert!(r.output(BLOCK).iter().all(|v| !v.is_subnormal()));

        r.shared.set_volume(1.0);
        r.run(100);
        assert_eq!(r.p.volume, 1.0);
    }

    #[test]
    fn broken_samples_become_silence() {
        assert_eq!(clean_sample(0.5), 0.5);
        assert_eq!(clean_sample(f32::NAN), 0.0);
        assert_eq!(clean_sample(f32::INFINITY), 0.0);
        assert_eq!(clean_sample(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn stall_watch_sees_a_stream_that_stops() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = StallWatch::new(t0);
        for i in 1..=20u32 {
            assert!(!w.check([i, i], at(i as u64 * 500)));
        }
        assert!(!w.check([20, 21], at(10_500)));
        assert!(!w.check([20, 22], at(12_500)));
        assert!(w.check([20, 23], at(13_000)));
    }

    #[test]
    fn stall_watch_waits_longer_for_the_first_callback() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = StallWatch::new(t0);
        for ms in (500..10_000).step_by(500) {
            assert!(!w.check([0, 0], at(ms)));
        }
        assert!(w.check([0, 0], at(10_000)));
    }

    #[test]
    fn stall_watch_ignores_time_when_checks_were_paused() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut w = StallWatch::new(t0);
        assert!(!w.check([1, 1], at(1)));
        assert!(!w.check([1, 1], at(3600)));
        assert!(!w.check([1, 1], at(3602)));
        assert!(w.check([1, 1], at(3604)));
    }

    #[test]
    fn period_estimate_ignores_outliers() {
        let mut e = PeriodEstimate::new();
        assert_eq!(e.update(480), 480);
        assert_eq!(e.update(13), 480);
        for _ in 0..40 {
            assert_eq!(e.update(480), 480);
        }
        assert_eq!(e.update(4800), 480);
        for _ in 0..4 {
            assert_eq!(e.update(480), 480);
        }
        for _ in 0..16 {
            e.update(1440);
        }
        assert_eq!(e.update(1440), 1440);
    }

    #[test]
    #[ignore]
    fn device_events_register() {
        let _ = list_devices();
        let (tx, _rx) = mpsc::channel();
        assert!(DeviceEvents::register(tx).is_ok());
    }

    #[test]
    fn explains_errors_from_cpal() {
        use windows::Win32::Foundation::E_ACCESSDENIED;
        use windows::Win32::Media::Audio::{AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_DEVICE_IN_USE};
        let explained = |code: windows::core::HRESULT| explain(StreamKind::Input, &cpal::Error::from(windows::core::Error::from(code)));
        assert!(explained(E_ACCESSDENIED).contains("プライバシー"));
        assert!(explained(AUDCLNT_E_DEVICE_IN_USE).contains("排他モード"));
        assert!(explained(AUDCLNT_E_DEVICE_INVALIDATED).contains("見つかりません"));
    }

    #[test]
    fn xrun_counts_as_dropout_and_others_stop_the_stream() {
        let mut shared = Shared::new(1.0, 20, 0);
        on_stream_error(&shared, StreamKind::Input, ErrorKind::Xrun.into());
        assert_eq!(shared.underruns(), 0);
        shared.started -= XRUN_SETTLE;
        on_stream_error(&shared, StreamKind::Input, ErrorKind::Xrun.into());
        assert_eq!(shared.underruns(), 1);
        assert!(shared.error().is_none());
        on_stream_error(&shared, StreamKind::Input, ErrorKind::StreamInvalidated.into());
        assert!(shared.error().is_some());
    }

    #[test]
    #[ignore]
    fn starting_and_stopping_keeps_handles_flat() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
        let handles = || {
            let mut n = 0u32;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) };
            n
        };
        let host = cpal::default_host();
        let (i, o) = (host.default_input_device().unwrap(), host.default_output_device().unwrap());
        let cycle = || {
            let mut m = start(&i, &o, 0.0, 20, 0).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            assert!(!m.is_stalled());
        };
        for _ in 0..5 {
            cycle();
        }
        let before = handles();
        for _ in 0..30 {
            cycle();
        }
        let after = handles();
        assert!(after < before + 10, "{before} -> {after}");
    }

    #[test]
    #[ignore]
    fn measure_on_real_devices() {
        let host = cpal::default_host();
        let (i, o) = (host.default_input_device().unwrap(), host.default_output_device().unwrap());
        for ms in [5, 10, 15, 20, 30] {
            let m = start(&i, &o, 0.0, ms, 0).unwrap();
            std::thread::sleep(Duration::from_secs(1));
            let before = m.shared.underruns();
            let mut sum = 0.0;
            for _ in 0..200 {
                std::thread::sleep(Duration::from_millis(20));
                sum += m.buffer_ms();
            }
            println!("target {ms} ms: floor {:.1} ms, underruns {} / 4 s, avg buffer {:.1} ms, error {:?}", m.floor_ms(), m.shared.underruns() - before, sum / 200.0, m.shared.error());
        }

        let mut t = LoopTest::start(&i, &o, 0.0, 0, 1.0).unwrap();
        let mut phases = String::new();
        loop {
            let c = match t.poll().unwrap() {
                TestPhase::Recording(_) => 'R',
                TestPhase::Playing(_) => 'P',
                TestPhase::Done => break,
            };
            if !phases.ends_with(c) {
                phases.push(c);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("loop test: phases {phases}, peak {:.4}, has_take {}", t.peak, t.has_take());
        assert_eq!(phases, "RP");

        t.replay(&o).unwrap();
        assert!(matches!(t.poll().unwrap(), TestPhase::Playing(_)));
        t.abort();
        assert!(!t.is_busy() && t.has_take());
        t.replay(&o).unwrap();
        assert!(t.is_busy());
    }
}
