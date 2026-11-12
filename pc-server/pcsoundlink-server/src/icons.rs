//! Встроенные иконки приложения.
//!
//! `build.rs` растрирует SVG из `assets/icons` в сырые RGBA-файлы, которые
//! подключаются на этапе компиляции — отдельных файлов рядом с exe не нужно.

pub const SIZE: u32 = 256;
pub const TRAY_SIZE: u32 = 32;

static COMPUTER_256: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/computer-256.rgba"));
static COMPUTER_32: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/computer-32.rgba"));
static PHONE_256: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phone-256.rgba"));
#[allow(dead_code)]
static PHONE_32: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phone-32.rgba"));

/// Иконка окна и панели задач (компьютер с Wi-Fi и звуком).
pub fn app_icon() -> eframe::egui::IconData {
    eframe::egui::IconData {
        rgba: COMPUTER_256.to_vec(),
        width: SIZE,
        height: SIZE,
    }
}

/// Иконка значка в системном трее.
pub fn tray_icon() -> Result<tray_icon::Icon, Box<dyn std::error::Error>> {
    Ok(tray_icon::Icon::from_rgba(
        COMPUTER_32.to_vec(),
        TRAY_SIZE,
        TRAY_SIZE,
    )?)
}

/// Телефон с Wi-Fi и звуком — например, для диалогов о сопряжении.
#[allow(dead_code)]
pub fn phone_icon() -> eframe::egui::IconData {
    eframe::egui::IconData {
        rgba: PHONE_256.to_vec(),
        width: SIZE,
        height: SIZE,
    }
}
