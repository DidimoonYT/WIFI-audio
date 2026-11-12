//! Main UDP control server, discovery announcements and session management.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::applog::log;
use crate::audio::AudioSession;
use crate::config::Config;
use crate::i18n::t;
use crate::input::{KeyboardController, MouseController};
use crate::mute::MuteController;
use crate::protocol::{
    control_reply, ServerInfo, CHANNELS, CONTROL_PORT, DISCOVERY_PORT, FRAME_MS, MAGIC_CTRL,
    MAGIC_DISCOVER, RATE,
};
use crate::screen::ScreenSession;

pub struct ServerHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct AudioSlot {
    session: AudioSession,
    client_ip: IpAddr,
    media_port: u16,
}

struct ScreenSlot {
    session: ScreenSession,
}

struct Shared {
    config: Config,
    id: String,
    mute: Arc<MuteController>,
    audio: Mutex<Option<AudioSlot>>,
    screen: Mutex<Option<ScreenSlot>>,
    mouse: Mutex<MouseController>,
    keyboard: Mutex<KeyboardController>,
    client_addr: Mutex<Option<SocketAddr>>,
    last_control: Mutex<Instant>,
}

pub fn start(config: Config) -> Result<ServerHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let join = thread::Builder::new()
        .name("server".into())
        .spawn(move || {
            if let Err(err) = run(config, thread_stop, ready_tx.clone()) {
                let text = err.to_string();
                log(format!("{}: {text}", t("Ошибка сервера", "Server error")));
                let _ = ready_tx.send(Err(text));
            }
        })
        .map_err(|e| anyhow!("{}: {e}", t("не удалось запустить поток сервера", "failed to start the server thread")))?;

    match ready_rx.recv_timeout(Duration::from_secs(4)) {
        Ok(Ok(())) => Ok(ServerHandle {
            stop,
            join: Some(join),
        }),
        Ok(Err(err)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            Err(anyhow!(err))
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            Err(anyhow!(
                "{}",
                t(
                    "сервер не запустился за 4 секунды",
                    "the server did not start within 4 seconds"
                )
            ))
        }
    }
}

fn broadcast_addresses() -> Vec<Ipv4Addr> {
    let mut result = vec![Ipv4Addr::BROADCAST];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                if let Some(broadcast) = v4.broadcast {
                    if !result.contains(&broadcast) {
                        result.push(broadcast);
                    }
                }
            }
        }
    }
    result
}

fn load_server_id() -> String {
    let dir = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .or_else(dirs::config_dir)
        .map(|path| path.join("PcSoundLink"));
    if let Some(dir) = dir {
        let path = dir.join("server_id");
        if let Ok(value) = std::fs::read_to_string(&path) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
        let _ = std::fs::create_dir_all(&dir);
        let value = uuid::Uuid::new_v4().simple().to_string();
        let value = value[..12].to_string();
        let _ = std::fs::write(path, &value);
        return value;
    }
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

fn run(
    config: Config,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<()> {
    let socket = match UdpSocket::bind(("0.0.0.0", CONTROL_PORT)) {
        Ok(socket) => socket,
        Err(err) => {
            let text = format!(
                "{} {CONTROL_PORT}: {err}",
                t("не удалось занять UDP-порт", "failed to bind UDP port")
            );
            let _ = ready.send(Err(text.clone()));
            return Err(anyhow!(text));
        }
    };
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .context(t("таймаут UDP-сокета", "UDP socket timeout"))?;

    let announce_socket = UdpSocket::bind(("0.0.0.0", 0))?;
    announce_socket.set_broadcast(true)?;
    let id = load_server_id();
    let mute = MuteController::new();

    let state = Arc::new(Shared {
        config,
        id,
        mute: mute.clone(),
        audio: Mutex::new(None),
        screen: Mutex::new(None),
        mouse: Mutex::new(MouseController::new()),
        keyboard: Mutex::new(KeyboardController::new()),
        client_addr: Mutex::new(None),
        last_control: Mutex::new(Instant::now()),
    });

    log("=".repeat(58));
    log(t(
        "  WIFI audio — сервер",
        "  WIFI audio — server",
    ));
    log("=".repeat(58));
    log(format!(
        "{}: {}",
        t("Имя компьютера", "Computer name"),
        state.config.name
    ));
    let report = crate::netinfo::analyze();
    if let Some(ip) = report.primary {
        log(format!(
            "{}: {ip} ({})",
            t("IP для телефона", "IP for the phone"),
            report
                .primary_name
                .as_deref()
                .unwrap_or(t("сетевой адаптер", "network adapter"))
        ));
    }
    let other_ips: Vec<String> = report
        .adapters
        .iter()
        .filter(|adapter| adapter.up && adapter.ip != report.primary)
        .filter_map(|adapter| {
            adapter
                .ip
                .map(|ip| format!("{} ({})", ip, adapter.name))
        })
        .collect();
    if !other_ips.is_empty() {
        log(format!(
            "{}: {}",
            t("Другие адреса", "Other addresses"),
            other_ips.join(", ")
        ));
    }
    for warning in &report.warnings {
        log(format!("{}: {warning}", t("Сеть", "Network")));
    }
    log(format!(
        "{}: {CONTROL_PORT} ({}), {DISCOVERY_PORT} ({})",
        t("Порты UDP", "UDP ports"),
        t("управление", "control"),
        t("поиск", "discovery")
    ));
    if state.mute.supported() {
        log(t("Управление mute: доступно", "Mute control: available"));
        log(t(
            "Автоусиление: включено (громкость ПК компенсируется)",
            "Auto gain: enabled (PC volume is compensated)",
        ));
    } else {
        log(t(
            "Управление mute: НЕДОСТУПНО",
            "Mute control: UNAVAILABLE",
        ));
    }
    log(t(
        "Готово. Жду телефон в локальной сети…",
        "Ready. Waiting for the phone on the local network…",
    ));
    let _ = ready.send(Ok(()));

    let announce_stop = stop.clone();
    let announce_state = state.clone();
    let announce = thread::Builder::new()
        .name("announce".into())
        .spawn(move || announce_loop(announce_socket, announce_state, announce_stop));

    let mut buffer = [0u8; 8192];
    while !stop.load(Ordering::SeqCst) {
        match socket.recv_from(&mut buffer) {
            Ok((len, address)) => {
                *state.last_control.lock().unwrap() = Instant::now();
                *state.client_addr.lock().unwrap() = Some(address);
                if let Err(err) = handle_packet(&state, &socket, &buffer[..len], address) {
                    log(format!(
                        "{}: {err}",
                        t("Ошибка обработки команды", "Command handling error")
                    ));
                }
            }
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => {
                log(format!("{}: {err}", t("Ошибка UDP", "UDP error")));
                break;
            }
        }
        housekeeping(&state, &socket);
    }

    if let Ok(announce) = announce {
        let _ = announce.join();
    }
    stop_screen(&state);
    stop_audio(&state, true);
    state.mute.restore_startup();
    Ok(())
}

fn announce_loop(socket: UdpSocket, state: Arc<Shared>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        let info = ServerInfo::new(
            &state.id,
            &state.config.name,
            state.audio.lock().unwrap().is_some(),
            state.mute.supported(),
            state.mute.muted(),
        );
        if let Ok(data) = serde_json::to_vec(&info) {
            for address in broadcast_addresses() {
                let _ = socket.send_to(&data, (address, DISCOVERY_PORT));
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn send_json(socket: &UdpSocket, address: SocketAddr, value: &Value) {
    if let Ok(data) = serde_json::to_vec(value) {
        let _ = socket.send_to(&data, address);
    }
}

fn handle_packet(
    state: &Arc<Shared>,
    socket: &UdpSocket,
    data: &[u8],
    address: SocketAddr,
) -> Result<()> {
    let payload: Value = serde_json::from_slice(data)?;
    let magic = payload.get("magic").and_then(Value::as_str).unwrap_or("");
    if magic == MAGIC_DISCOVER {
        let info = ServerInfo::new(
            &state.id,
            &state.config.name,
            state.audio.lock().unwrap().is_some(),
            state.mute.supported(),
            state.mute.muted(),
        );
        send_json(socket, address, &serde_json::to_value(info)?);
        return Ok(());
    }
    if magic != MAGIC_CTRL {
        return Ok(());
    }

    let cmd = payload.get("cmd").and_then(Value::as_str).unwrap_or("");
    match cmd {
        "start" => start_audio(state, socket, address, &payload),
        "stop" => {
            stop_audio(state, true);
            send_json(socket, address, &control_reply("stopped", json!({})));
        }
        "ping" => {
            let t = payload.get("t").cloned().unwrap_or(json!(0));
            send_json(socket, address, &control_reply("pong", json!({ "t": t })));
        }
        "mute" => {
            let supported = state.mute.supported();
            let value = payload.get("value").and_then(Value::as_bool).unwrap_or(false);
            let muted = if supported {
                state.mute.set_muted(value)
            } else {
                false
            };
            send_json(
                socket,
                address,
                &control_reply(
                    "mute_state",
                    json!({ "value": muted, "supported": supported }),
                ),
            );
        }
        "status" => {
            send_json(
                socket,
                address,
                &control_reply(
                    "status",
                    json!({
                        "name": state.config.name,
                        "busy": state.audio.lock().unwrap().is_some(),
                        "muteSupported": state.mute.supported(),
                        "muted": state.mute.muted(),
                    }),
                ),
            );
        }
        "screen_start" => start_screen(state, socket, address, &payload),
        "screen_stop" => {
            stop_screen(state);
            send_json(socket, address, &control_reply("screen_stopped", json!({})));
        }
        "mouse" => {
            let action = payload.get("action").and_then(Value::as_str).unwrap_or("");
            let button = payload.get("button").and_then(Value::as_str).unwrap_or("left");
            let mouse = state.mouse.lock().unwrap();
            match action {
                "move" => mouse.move_by(
                    payload.get("dx").and_then(Value::as_i64).unwrap_or(0) as i32,
                    payload.get("dy").and_then(Value::as_i64).unwrap_or(0) as i32,
                ),
                "click" => {
                    mouse.click(button);
                    log(format!("{}: {button}", t("Мышь: клик", "Mouse: click")));
                }
                "button" => {
                    let down = payload.get("down").and_then(Value::as_bool).unwrap_or(false);
                    mouse.button(button, down);
                    log(format!(
                        "{}: {button} {}",
                        t("Мышь", "Mouse"),
                        if down {
                            t("вниз", "down")
                        } else {
                            t("вверх", "up")
                        }
                    ));
                }
                "wheel" => mouse.wheel(
                    payload.get("by").and_then(Value::as_i64).unwrap_or(0) as i32,
                ),
                _ => {}
            }
        }
        "text" => {
            let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
            state.keyboard.lock().unwrap().type_text(text);
            log(format!(
                "{} {} {}",
                t("Клавиатура: введено", "Keyboard: typed"),
                text.chars().count(),
                t("симв.", "chars")
            ));
        }
        other => {
            send_json(
                socket,
                address,
                &control_reply(
                    "error",
                    json!({ "message": format!("{}: {other}", t("Неизвестная команда", "Unknown command")) }),
                ),
            );
        }
    }
    Ok(())
}

fn start_audio(
    state: &Arc<Shared>,
    socket: &UdpSocket,
    address: SocketAddr,
    payload: &Value,
) {
    let media_port = payload.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
    if media_port == 0 {
        send_json(
            socket,
            address,
            &control_reply(
                "error",
                json!({ "message": t("Некорректный порт телефона", "Invalid phone port") }),
            ),
        );
        return;
    }
    let rate = payload
        .get("rate")
        .and_then(Value::as_u64)
        .unwrap_or(RATE as u64);
    let channels = payload
        .get("channels")
        .and_then(Value::as_u64)
        .unwrap_or(CHANNELS as u64);
    if rate != RATE as u64 || channels != CHANNELS as u64 {
        send_json(
            socket,
            address,
            &control_reply(
                "error",
                json!({ "message": t(
                    "Поддерживается только PCM 48000 Гц, стерео",
                    "Only PCM 48000 Hz stereo is supported"
                ) }),
            ),
        );
        return;
    }

    let client_ip = address.ip();
    let already_running = {
        let slot = state.audio.lock().unwrap();
        slot.as_ref()
            .map(|existing| existing.client_ip == client_ip && existing.media_port == media_port)
            .unwrap_or(false)
    };
    if already_running {
        reply_started(state, socket, address);
        return;
    }
    stop_audio(state, true);

    let device = state.config.device_name.clone();
    let (session, ready) = AudioSession::start(client_ip, media_port, device, state.mute.clone());
    {
        let mut slot = state.audio.lock().unwrap();
        *slot = Some(AudioSlot {
            session,
            client_ip,
            media_port,
        });
    }

    match ready.recv_timeout(Duration::from_secs(3)) {
        Ok(Err(err)) => {
            stop_audio(state, true);
            send_json(
                socket,
                address,
                &control_reply(
                    "error",
                    json!({ "message": format!("{}: {err}", t("Не удалось захватить звук", "Failed to capture audio")) }),
                ),
            );
        }
        _ => {
            log(format!(
                "{} {client_ip}:{media_port}, {} {FRAME_MS} {}",
                t("Трансляция на", "Streaming to"),
                t("пакет", "frame"),
                t("мс", "ms")
            ));
            reply_started(state, socket, address);
        }
    }
}

fn reply_started(state: &Shared, socket: &UdpSocket, address: SocketAddr) {
    send_json(
        socket,
        address,
        &control_reply(
            "started",
            json!({
                "frameMs": FRAME_MS,
                "name": state.config.name,
                "muteSupported": state.mute.supported(),
                "muted": state.mute.muted(),
            }),
        ),
    );
}

fn stop_audio(state: &Shared, restore_mute: bool) {
    let taken = state.audio.lock().unwrap().take();
    if let Some(mut slot) = taken {
        slot.session.stop();
        if restore_mute {
            state.mute.set_muted(slot.session.original_mute);
        }
        log(t("Трансляция остановлена", "Streaming stopped"));
    }
}

fn start_screen(
    state: &Arc<Shared>,
    socket: &UdpSocket,
    address: SocketAddr,
    payload: &Value,
) {
    let port = payload.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
    if port == 0 {
        send_json(
            socket,
            address,
            &control_reply(
                "error",
                json!({ "message": t("Некорректный порт экрана", "Invalid screen port") }),
            ),
        );
        return;
    }
    stop_screen(state);
    let (session, ready) = ScreenSession::start(address.ip(), port);
    {
        let mut slot = state.screen.lock().unwrap();
        *slot = Some(ScreenSlot { session });
    }

    match ready.recv_timeout(Duration::from_secs(6)) {
        Ok(Ok((w, h))) => {
            log(format!(
                "{} {}:{port}",
                t("Демонстрация экрана для", "Screen streaming to"),
                address.ip()
            ));
            send_json(
                socket,
                address,
                &control_reply("screen", json!({ "screen": true, "w": w, "h": h })),
            );
        }
        Ok(Err(err)) => {
            stop_screen(state);
            send_json(
                socket,
                address,
                &control_reply(
                    "error",
                    json!({ "message": format!("{}: {err}", t("Демонстрация экрана недоступна", "Screen streaming unavailable")) }),
                ),
            );
        }
        Err(_) => {
            send_json(
                socket,
                address,
                &control_reply("screen", json!({ "screen": true, "w": 0, "h": 0 })),
            );
        }
    }
}

fn stop_screen(state: &Shared) {
    let taken = state.screen.lock().unwrap().take();
    if let Some(mut slot) = taken {
        slot.session.stop();
        log(t(
            "Демонстрация экрана остановлена",
            "Screen streaming stopped",
        ));
    }
}

fn housekeeping(state: &Arc<Shared>, socket: &UdpSocket) {
    let mut stop = false;
    let mut error = None;
    {
        let slot = state.audio.lock().unwrap();
        if let Some(session) = slot.as_ref() {
            if session.session.is_done() {
                error = session.session.take_error();
                stop = true;
            }
        }
    }
    if !stop {
        let idle = state.last_control.lock().unwrap().elapsed();
        if idle > Duration::from_secs(8) && state.audio.lock().unwrap().is_some() {
            log(t(
                "Телефон не отвечает, останавливаю трансляцию",
                "The phone is not responding, stopping the stream",
            ));
            stop = true;
        }
    }
    if stop {
        stop_audio(state, true);
        if let Some(error) = error {
            if let Some(address) = *state.client_addr.lock().unwrap() {
                send_json(
                    socket,
                    address,
                    &control_reply(
                        "error",
                        json!({ "message": format!("{}: {error}", t("Захват звука прерван", "Audio capture interrupted")) }),
                    ),
                );
            }
        }
    }
}
