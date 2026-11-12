//! Control of the default Windows playback endpoint (mute + volume).
//! COM objects are created and used on one dedicated worker thread.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};

enum Command {
    GetMuted(Sender<bool>),
    SetMuted(bool, Sender<bool>),
    Restore(Sender<bool>),
}

struct Shared {
    supported: AtomicBool,
    muted: AtomicBool,
    volume_bits: AtomicU32,
}

impl Shared {
    fn set_volume(&self, value: f32) {
        self.volume_bits.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }
}

pub struct MuteController {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    startup_mute: bool,
}

fn open_endpoint() -> Option<IAudioEndpointVolume> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
    }
}

fn read_state(endpoint: &IAudioEndpointVolume) -> (bool, f32) {
    unsafe {
        let muted = endpoint.GetMute().map(|v| v.as_bool()).unwrap_or(false);
        let volume = endpoint.GetMasterVolumeLevelScalar().unwrap_or(1.0);
        (muted, volume)
    }
}

impl MuteController {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            supported: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
        });
        let worker_shared = shared.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        thread::Builder::new()
            .name("mute".into())
            .spawn(move || worker(rx, worker_shared, ready_tx))
            .ok();

        let supported = ready_rx.recv_timeout(Duration::from_secs(3)).unwrap_or(false);
        shared.supported.store(supported, Ordering::Relaxed);
        let startup_mute = shared.muted.load(Ordering::Relaxed);
        Arc::new(Self {
            tx,
            shared,
            startup_mute,
        })
    }

    pub fn supported(&self) -> bool {
        self.shared.supported.load(Ordering::Relaxed)
    }

    pub fn volume(&self) -> f32 {
        self.shared.volume()
    }

    pub fn muted(&self) -> bool {
        self.query(Command::GetMuted)
    }

    pub fn set_muted(&self, value: bool) -> bool {
        if !self.supported() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        if self.tx.send(Command::SetMuted(value, tx)).is_err() {
            return false;
        }
        rx.recv_timeout(Duration::from_secs(1))
            .unwrap_or_else(|_| self.shared.muted.load(Ordering::Relaxed))
    }

    pub fn restore_startup(&self) {
        if !self.supported() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Command::Restore(tx));
        let _ = rx.recv_timeout(Duration::from_secs(1));
        self.shared
            .muted
            .store(self.startup_mute, Ordering::Relaxed);
    }

    fn query(&self, make: impl FnOnce(Sender<bool>) -> Command) -> bool {
        if !self.supported() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        if self.tx.send(make(tx)).is_err() {
            return self.shared.muted.load(Ordering::Relaxed);
        }
        rx.recv_timeout(Duration::from_secs(1))
            .unwrap_or_else(|_| self.shared.muted.load(Ordering::Relaxed))
    }
}

fn worker(rx: Receiver<Command>, shared: Arc<Shared>, ready: Sender<bool>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Some(endpoint) = open_endpoint() else {
        let _ = ready.send(false);
        return;
    };
    let (startup_mute, startup_volume) = read_state(&endpoint);
    shared.muted.store(startup_mute, Ordering::Relaxed);
    shared.set_volume(startup_volume);
    shared.supported.store(true, Ordering::Relaxed);
    let _ = ready.send(true);

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Command::GetMuted(reply)) => {
                let (muted, volume) = read_state(&endpoint);
                shared.muted.store(muted, Ordering::Relaxed);
                shared.set_volume(volume);
                let _ = reply.send(muted);
            }
            Ok(Command::SetMuted(value, reply)) => {
                unsafe {
                    let _ = endpoint.SetMute(value, std::ptr::null());
                }
                let (muted, volume) = read_state(&endpoint);
                shared.muted.store(muted, Ordering::Relaxed);
                shared.set_volume(volume);
                let _ = reply.send(muted);
            }
            Ok(Command::Restore(reply)) => {
                unsafe {
                    let _ = endpoint.SetMute(startup_mute, std::ptr::null());
                }
                shared.muted.store(startup_mute, Ordering::Relaxed);
                let _ = reply.send(startup_mute);
            }
            Err(RecvTimeoutError::Timeout) => {
                let (muted, volume) = read_state(&endpoint);
                shared.muted.store(muted, Ordering::Relaxed);
                shared.set_volume(volume);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    unsafe {
        windows::Win32::System::Com::CoUninitialize();
    }
}
