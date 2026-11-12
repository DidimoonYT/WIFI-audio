//! Wire protocol shared with the Android client.

use serde::Serialize;

pub const CONTROL_PORT: u16 = 47832;
pub const DISCOVERY_PORT: u16 = 47833;
pub const PROTO_VERSION: u8 = 1;

pub const MAGIC_DISCOVER: &str = "PCSOUND_DISCOVER";
pub const MAGIC_SERVER: &str = "PCSOUND_SERVER";
pub const MAGIC_CTRL: &str = "PCSOUND_CTRL";

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const FRAME_MS: u16 = 10;
pub const CHUNK_BYTES: usize = 960;
pub const AUDIO_HEADER_BYTES: usize = 16;

pub const SCREEN_MAX_WIDTH: u32 = 1280;
pub const SCREEN_FPS: u64 = 15;
pub const SCREEN_JPEG_QUALITY: u8 = 55;

/// PCM packet header, network byte order:
/// magic(2) version(1) flags(1) seq(u32) timestamp(u32) total(u16) index(u8) chunks(u8)
pub fn audio_header(
    seq: u32,
    timestamp: u32,
    total_len: u16,
    index: u8,
    chunks: u8,
) -> [u8; AUDIO_HEADER_BYTES] {
    let mut header = [0u8; AUDIO_HEADER_BYTES];
    header[0..2].copy_from_slice(b"PS");
    header[2] = PROTO_VERSION;
    header[3] = 0;
    header[4..8].copy_from_slice(&seq.to_be_bytes());
    header[8..12].copy_from_slice(&timestamp.to_be_bytes());
    header[12..14].copy_from_slice(&total_len.to_be_bytes());
    header[14] = index;
    header[15] = chunks;
    header
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub magic: &'static str,
    pub v: u8,
    pub id: String,
    pub name: String,
    pub port: u16,
    pub proto: u8,
    pub frame_ms: u16,
    pub busy: bool,
    pub mute_supported: bool,
    pub muted: bool,
}

impl ServerInfo {
    pub fn new(
        id: &str,
        name: &str,
        busy: bool,
        mute_supported: bool,
        muted: bool,
    ) -> Self {
        Self {
            magic: MAGIC_SERVER,
            v: PROTO_VERSION,
            id: id.to_string(),
            name: name.to_string(),
            port: CONTROL_PORT,
            proto: PROTO_VERSION,
            frame_ms: FRAME_MS,
            busy,
            mute_supported,
            muted,
        }
    }
}

pub fn control_reply(cmd: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut value = serde_json::json!({
        "magic": MAGIC_CTRL,
        "v": PROTO_VERSION,
        "cmd": cmd,
    });
    if let (Some(obj), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        for (k, v) in more {
            obj.insert(k.clone(), v.clone());
        }
    }
    value
}
