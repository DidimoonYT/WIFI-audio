//! PC Sound Link — Windows server (Rust).
//!
//! Streams system audio and a live screen preview to the Android app over the
//! local network. Portable single executable, no installation required.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod applog;
mod audio;
mod autostart;
mod config;
mod firewall;
mod gui;
mod i18n;
mod icons;
mod input;
mod mute;
mod netinfo;
mod protocol;
mod screen;
mod server;
mod tray;

use anyhow::{anyhow, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "PcSoundLinkServer", version, about = "WIFI audio — server")]
struct Args {
    /// Сразу запустить сервер при открытии окна
    #[arg(long)]
    autostart: bool,

    /// Запустить скрыто в системном трее
    #[arg(long)]
    minimized: bool,

    /// Показать список устройств вывода и выйти
    #[arg(long)]
    list_devices: bool,

    /// Служебный запуск с правами администратора: открыть порты и выйти
    #[arg(long, hide = true)]
    install_firewall: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Язык интерфейса: выбор пользователя или системный.
    let config = config::Config::load();
    i18n::init(config.language.resolve());

    if args.list_devices {
        for device in audio::list_output_devices() {
            println!("{device}");
        }
        return Ok(());
    }
    if args.install_firewall {
        return match firewall::install_rules() {
            Ok(()) => Ok(()),
            Err(err) => Err(err),
        };
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([820.0, 600.0])
            .with_min_inner_size([620.0, 460.0])
            .with_title(i18n::window_title())
            .with_icon(icons::app_icon())
            .with_visible(!args.minimized),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        i18n::window_title(),
        options,
        Box::new(move |cc| Ok(Box::new(gui::App::new(cc, args.autostart, args.minimized)))),
    )
    .map_err(|err| anyhow!("{}: {err}", i18n::t("Ошибка окна", "Window error")))
}
