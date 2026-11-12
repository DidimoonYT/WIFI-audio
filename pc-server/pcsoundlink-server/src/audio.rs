//! WASAPI loopback capture and PCM packet sender.

use std::net::{IpAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use wasapi::{
    AudioCaptureClient, Device, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat,
};

use crate::applog::log;
use crate::i18n::t;
use crate::mute::MuteController;
use crate::protocol::{
    audio_header, CHANNELS, CHUNK_BYTES, FRAME_MS, RATE,
};

const SAMPLE_RATE: usize = RATE as usize;
const CHANNEL_COUNT: usize = CHANNELS as usize;
const FRAME_SAMPLES: usize = SAMPLE_RATE * FRAME_MS as usize / 1000;
const FRAME_I16_BYTES: usize = FRAME_SAMPLES * CHANNEL_COUNT * 2;

#[derive(Clone, Copy)]
struct FormatInfo {
    channels: usize,
    bits: usize,
    float: bool,
}

pub fn list_output_devices() -> Vec<String> {
    let _ = wasapi::initialize_mta();
    let Ok(enumerator) = DeviceEnumerator::new() else {
        return Vec::new();
    };
    let Ok(collection) = enumerator.get_device_collection(Direction::Render) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for device in &collection {
        if let Ok(device) = device {
            if let Ok(name) = device.get_friendlyname() {
                names.push(name);
            }
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    names
}

fn find_device(enumerator: &DeviceEnumerator, wanted: Option<&str>) -> Result<Device> {
    if let Some(wanted) = wanted {
        let needle = wanted.to_lowercase();
        if let Ok(collection) = enumerator.get_device_collection(Direction::Render) {
            for device in &collection {
                if let Ok(device) = device {
                    if let Ok(name) = device.get_friendlyname() {
                        if name.to_lowercase().contains(&needle) {
                            return Ok(device);
                        }
                    }
                }
            }
        }
        log(format!(
            "{} «{wanted}» — {}",
            t("Устройство", "Device"),
            t(
                "не найдено, использую системное",
                "not found, using the system default"
            )
        ));
    }
    enumerator
        .get_default_device(Direction::Render)
        .map_err(|e| anyhow!("{}: {e}", t("не найдено устройство вывода", "no output device found")))
}

fn parse_format(format: &WaveFormat) -> Result<FormatInfo> {
    let bits = format.get_bitspersample() as usize;
    let subformat = format
        .get_subformat()
        .map_err(|e| anyhow!("{}: {e}", t("неизвестный формат звука", "unknown audio format")))?;
    Ok(FormatInfo {
        channels: format.get_nchannels() as usize,
        bits,
        float: matches!(subformat, SampleType::Float),
    })
}

fn convert_packet(raw: &[u8], info: FormatInfo, out: &mut Vec<i16>) -> Result<()> {
    let bytes_per_sample = info.bits / 8;
    if bytes_per_sample == 0 || info.channels == 0 {
        return Err(anyhow!(t("некорректный формат звука", "invalid audio format")));
    }
    let frame_bytes = bytes_per_sample * info.channels;
    if frame_bytes == 0 {
        return Err(anyhow!(t("некорректный формат звука", "invalid audio format")));
    }

    for frame in raw.chunks_exact(frame_bytes) {
        let left = sample_at(frame, 0, info)?;
        let right = if info.channels >= 2 {
            sample_at(frame, 1, info)?
        } else {
            left
        };
        out.push(left);
        out.push(right);
    }
    Ok(())
}

fn sample_at(frame: &[u8], channel: usize, info: FormatInfo) -> Result<i16> {
    let offset = channel * (info.bits / 8);
    let value = if info.float && info.bits == 32 {
        let bytes: [u8; 4] = frame[offset..offset + 4]
            .try_into()
            .map_err(|_| anyhow!(t("короткий звуковой пакет", "short audio packet")))?;
        let sample = f32::from_le_bytes(bytes);
        (sample.clamp(-1.0, 1.0) * 32767.0) as i16
    } else if info.bits == 16 {
        let bytes: [u8; 2] = frame[offset..offset + 2]
            .try_into()
            .map_err(|_| anyhow!(t("короткий звуковой пакет", "short audio packet")))?;
        i16::from_le_bytes(bytes)
    } else if info.bits == 32 {
        let bytes: [u8; 4] = frame[offset..offset + 4]
            .try_into()
            .map_err(|_| anyhow!(t("короткий звуковой пакет", "short audio packet")))?;
        (i32::from_le_bytes(bytes) >> 16) as i16
    } else {
        return Err(anyhow!(
            "{}: {}",
            t("неподдерживаемый формат", "unsupported format"),
            info.bits
        ));
    };
    Ok(value)
}

pub struct AudioSession {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
    pub original_mute: bool,
}

impl AudioSession {
    pub fn start(
        client_ip: IpAddr,
        media_port: u16,
        device_name: Option<String>,
        mute: Arc<MuteController>,
    ) -> (Self, std::sync::mpsc::Receiver<Result<String, String>>) {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let thread_stop = stop.clone();
        let thread_done = done.clone();
        let thread_error = error.clone();
        let original_mute = mute.muted();

        let handle = thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let result = capture_loop(
                    client_ip,
                    media_port,
                    device_name,
                    mute,
                    thread_stop,
                    thread_done.clone(),
                    thread_error.clone(),
                    ready_tx,
                );
                if let Err(err) = result {
                    let text = err.to_string();
                    log(format!(
                        "{}: {text}",
                        t("Ошибка захвата звука", "Audio capture error")
                    ));
                    if let Ok(mut slot) = thread_error.lock() {
                        *slot = Some(text);
                    }
                }
                thread_done.store(true, Ordering::SeqCst);
            })
            .ok();

        (
            Self {
                stop,
                done,
                error,
                handle,
                original_mute,
            },
            ready_rx,
        )
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn capture_loop(
    client_ip: IpAddr,
    media_port: u16,
    device_name: Option<String>,
    mute: Arc<MuteController>,
    stop: Arc<AtomicBool>,
    _done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    ready: Sender<Result<String, String>>,
) -> Result<()> {
    let _ = wasapi::initialize_mta();

    let result = (|| -> Result<(wasapi::AudioClient, AudioCaptureClient, String, FormatInfo)> {
        let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("WASAPI: {e}"))?;
        let device = find_device(&enumerator, device_name.as_deref())?;
        let name = device
            .get_friendlyname()
            .unwrap_or_else(|_| t("системное", "system default").to_string());
        let mut client = device
            .get_iaudioclient()
            .map_err(|e| anyhow!("{}: {e}", t("не удалось открыть устройство", "failed to open the device")))?;

        let desired = WaveFormat::new(
            16,
            16,
            SampleType::Int,
            SAMPLE_RATE,
            CHANNEL_COUNT,
            None,
        );
        let info = FormatInfo {
            channels: CHANNEL_COUNT,
            bits: 16,
            float: false,
        };

        let mode = StreamMode::PollingShared {
            autoconvert: true,
            buffer_duration_hns: 10_000_000,
        };
        match client.initialize_client(&desired, Direction::Capture, mode, None) {
            Ok(()) => {}
            Err(first_error) => {
                log(format!(
                    "{} ({first_error}), {}",
                    t(
                        "Автоконвертация 48 кГц недоступна",
                        "48 kHz autoconversion is unavailable"
                    ),
                    t(
                        "пробую формат устройства",
                        "trying the device format"
                    )
                ));
                let mix = client
                    .get_mixformat()
                    .map_err(|e| anyhow!("{}: {e}", t("не удалось получить формат устройства", "failed to get the device format")))?;
                let info = parse_format(&mix)?;
                if mix.get_samplespersec() != SAMPLE_RATE as u32 {
                    return Err(anyhow!(
                        "{} {} {}",
                        t("устройство работает на", "the device runs at"),
                        mix.get_samplespersec(),
                        t("Гц, нужен режим 48 кГц", "Hz, 48 kHz mode is required")
                    ));
                }
                client
                    .initialize_client(
                        &mix,
                        Direction::Capture,
                        StreamMode::PollingShared {
                            autoconvert: false,
                            buffer_duration_hns: 10_000_000,
                        },
                        None,
                    )
                    .map_err(|e| anyhow!("{}: {e}", t("не удалось включить захват", "failed to start capture")))?;
                let capture = client
                    .get_audiocaptureclient()
                    .map_err(|e| anyhow!("{}: {e}", t("клиент захвата WASAPI", "WASAPI capture client")))?;
                client
                    .start_stream()
                    .map_err(|e| anyhow!("{}: {e}", t("запуск захвата", "starting capture")))?;
                return Ok((client, capture, name, info));
            }
        }

        let capture = client
            .get_audiocaptureclient()
            .map_err(|e| anyhow!("{}: {e}", t("клиент захвата WASAPI", "WASAPI capture client")))?;
        client
            .start_stream()
            .map_err(|e| anyhow!("{}: {e}", t("запуск захвата", "starting capture")))?;
        Ok((client, capture, name, info))
    })();

    let (client, capture, name, format) = match result {
        Ok(value) => value,
        Err(err) => {
            let _ = ready.send(Err(err.to_string()));
            return Err(err);
        }
    };

    log(format!("{}: {name}", t("Захват звука", "Audio capture")));
    let _ = ready.send(Ok(name));

    let socket = UdpSocket::bind("0.0.0.0:0")
        .context(t("UDP-сокет для звука", "UDP socket for audio"))?;
    let _ = socket.set_write_timeout(Some(Duration::from_millis(500)));

    let block_align = format.channels * (format.bits / 8);
    let mut samples: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2 * 4);
    let mut seq: u32 = 0;
    let start = Instant::now();
    let mut gain = 1.0f32;
    let mut gain_at = Instant::now();
    let mut logged_error = false;
    let mut last_data_at = Instant::now();

    // Windows по умолчанию округляет короткие sleep до ~15-20 мс, из-за чего
    // вместо 100 кадров в секунду отправлялось ~50. Просим у системы точность
    // 1 мс на время работы потока захвата.
    unsafe {
        let _ = windows::Win32::Media::timeBeginPeriod(1);
    }
    let frame_interval = Duration::from_micros(1_000_000 * FRAME_MS as u64 / 1000);
    let mut next_frame = Instant::now() + frame_interval;

    while !stop.load(Ordering::SeqCst) {
        // Держим стабильный ритм по абсолютному времени, а не по длительности
        // сна: так задержки планировщика не накапливаются.
        let now = Instant::now();
        if next_frame > now {
            thread::sleep(next_frame - now);
        }
        next_frame += frame_interval;
        if next_frame < Instant::now() {
            // Сильно отстали (например, после сна/подвисания) — начинаем заново.
            next_frame = Instant::now() + frame_interval;
        }

        loop {
            match capture.get_next_packet_size() {
                Ok(Some(frames)) if frames > 0 => {
                    let mut raw = vec![0u8; frames as usize * block_align];
                    match capture.read_from_device(&mut raw) {
                        Ok((read, _info)) => {
                            let bytes = read as usize * block_align;
                            if bytes > 0 {
                                last_data_at = Instant::now();
                            }
                            if let Err(err) = convert_packet(&raw[..bytes], format, &mut samples) {
                                if let Ok(mut slot) = error.lock() {
                                    *slot = Some(err.to_string());
                                }
                                let _ = client.stop_stream();
                                return Err(err);
                            }
                        }
                        Err(err) => {
                            if !logged_error {
                                logged_error = true;
                                log(format!(
                                    "{}: {err}",
                                    t("Ошибка чтения звука", "Audio read error")
                                ));
                            }
                            break;
                        }
                    }
                }
                Ok(Some(_)) => break,
                Ok(None) => break,
                Err(err) => {
                    if !logged_error {
                        logged_error = true;
                        log(format!(
                            "{}: {err}",
                            t("Ошибка очереди звука", "Audio queue error")
                        ));
                    }
                    break;
                }
            }
        }

        // Если устройство молчит (нет активного аудиопотока), WASAPI может не
        // отдавать данные вовсе. Шлём тишину, чтобы телефон не решил, что
        // соединение прервано, и буфер оставался стабильным.
        if last_data_at.elapsed() >= Duration::from_millis(25) {
            samples.extend(std::iter::repeat(0i16).take(FRAME_SAMPLES * CHANNEL_COUNT));
        }

        if gain_at.elapsed() >= Duration::from_millis(500) {
            gain_at = Instant::now();
            let volume = mute.volume();
            gain = if volume > 0.001 {
                volume.powf(-1.7).min(150.0)
            } else {
                1.0
            };
        }

        while samples.len() >= FRAME_SAMPLES * CHANNEL_COUNT && !stop.load(Ordering::SeqCst) {
            let frame: Vec<i16> = samples.drain(..FRAME_SAMPLES * CHANNEL_COUNT).collect();
            let mut pcm = Vec::with_capacity(FRAME_I16_BYTES);
            for sample in frame {
                let value = (sample as f32 * gain).clamp(-32768.0, 32767.0) as i16;
                pcm.extend_from_slice(&value.to_le_bytes());
            }

            let timestamp = start.elapsed().as_millis() as u32;
            let total = pcm.len() as u16;
            let chunks = pcm.len().div_ceil(CHUNK_BYTES);
            for (index, chunk) in pcm.chunks(CHUNK_BYTES).enumerate() {
                let header = audio_header(seq, timestamp, total, index as u8, chunks as u8);
                let mut packet = Vec::with_capacity(header.len() + chunk.len());
                packet.extend_from_slice(&header);
                packet.extend_from_slice(chunk);
                if socket.send_to(&packet, (client_ip, media_port)).is_err() {
                    break;
                }
            }
            seq = seq.wrapping_add(1);
        }

        if samples.len() > FRAME_SAMPLES * CHANNEL_COUNT * 50 {
            let keep = FRAME_SAMPLES * CHANNEL_COUNT;
            samples.drain(..samples.len() - keep);
        }
    }

    unsafe {
        let _ = windows::Win32::Media::timeEndPeriod(1);
    }
    let _ = client.stop_stream();
    Ok(())
}
