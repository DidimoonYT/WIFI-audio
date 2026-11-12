//! Russian-language GUI for the server.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, ViewportCommand};
use crate::applog;
use crate::audio::list_output_devices;
use crate::autostart;
use crate::config::Config;
use crate::firewall;
use crate::i18n::{self, LangChoice};
use crate::netinfo::{self, NetReport};
use crate::server::{start, ServerHandle};
use crate::tray::{Tray, TrayAction};

// Палитра Material 3 (тёмная тема), как в Android-приложении.
const BG: Color32 = Color32::from_rgb(20, 18, 24); // surface
const CARD: Color32 = Color32::from_rgb(25, 25, 35); // surface container
const CARD_ALT: Color32 = Color32::from_rgb(33, 31, 38); // поля ввода / вложенные блоки
const TEXT: Color32 = Color32::from_rgb(230, 224, 233); // onSurface
const MUTED: Color32 = Color32::from_rgb(202, 196, 208); // onSurfaceVariant
const DIM: Color32 = Color32::from_rgb(147, 143, 153); // outline
const OK: Color32 = Color32::from_rgb(27, 170, 96); // status_green
const BUSY: Color32 = Color32::from_rgb(125, 162, 255); // brand в работе
const ERR: Color32 = Color32::from_rgb(229, 72, 77); // status_red
const BORDER: Color32 = Color32::from_rgb(58, 56, 66);
const BRAND: Color32 = Color32::from_rgb(125, 162, 255); // brand (night)
const BRAND_TEXT: Color32 = Color32::from_rgb(20, 18, 24); // текст на brand
const WARN_BG: Color32 = Color32::from_rgb(255, 247, 237);
const WARN_BORDER: Color32 = Color32::from_rgb(253, 186, 116);
const WARN_TEXT: Color32 = Color32::from_rgb(140, 62, 9);
const ANDROID_APP_URL: &str =
    "https://github.com/DidimoonYT/WIFI-audio/releases/latest/download/WIFI-audio-android.apk";

pub struct App {
    config: Config,
    name: String,
    devices: Vec<String>,
    device_index: usize,
    handle: Option<ServerHandle>,
    status: String,
    detail: String,
    starting: bool,
    log_pos: usize,
    logs: Vec<String>,
    start_now: bool,
    start_now_done: bool,
    autostart_enabled: bool,
    language: LangChoice,
    quitting: bool,
    ip: String,
    net: NetReport,
    last_net_check: Instant,
    tray: Option<Tray>,
    hwnd: isize,
    link_copied_until: Option<Instant>,
}

fn window_hwnd(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start_now: bool, start_hidden: bool) -> Self {
        install_fonts(&cc.egui_ctx);
        style(&cc.egui_ctx);
        fit_window(&cc.egui_ctx);

        let config = Config::load();
        let name = config.name.clone();
        let language = config.language;
        let devices = list_output_devices();
        let device_index = config
            .device_name
            .as_ref()
            .and_then(|wanted| {
                devices
                    .iter()
                    .position(|item| item.eq_ignore_ascii_case(wanted))
            })
            .map(|index| index + 1)
            .unwrap_or(0);

        let net = netinfo::analyze();
        let ip = net
            .primary
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "—".to_string());
        for warning in &net.warnings {
            applog::log(format!("{}: {warning}", i18n::t("Сеть", "Network")));
        }
        firewall::ensure_async();

        // Initialize tray
        let hwnd = window_hwnd(cc);
        if start_hidden {
            // eframe/winit может показать окно уже после создания App, поэтому
            // несколько секунд удерживаем его скрытым.
            crate::tray::hide_window(hwnd);
            std::thread::spawn(move || {
                for _ in 0..60 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    if !crate::tray::should_hide() {
                        break;
                    }
                    crate::tray::hide_window(hwnd);
                }
            });
        }
        let tray = Tray::new(cc.egui_ctx.clone(), hwnd).ok();
        if tray.is_none() {
            applog::log(
                i18n::t(
                    "Не удалось создать значок в системном трее",
                    "Failed to create the system tray icon",
                )
                .to_string(),
            );
        }

        // Load autostart setting
        let autostart_enabled = autostart::is_autostart_enabled();

        let mut app = Self {
            config,
            name,
            devices,
            device_index,
            handle: None,
            status: i18n::t("Сервер остановлен", "Server stopped").to_string(),
            detail: String::new(),
            starting: false,
            log_pos: 0,
            logs: Vec::new(),
            start_now,
            start_now_done: false,
            autostart_enabled,
            language,
            quitting: false,
            ip,
            net,
            last_net_check: Instant::now(),
            tray,
            hwnd,
            link_copied_until: None,
        };
        app.refresh_logs();
        app
    }

    fn refresh_network(&mut self) {
        self.net = netinfo::analyze();
        self.ip = self
            .net
            .primary
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "—".to_string());
    }

    fn refresh_logs(&mut self) {
        let (lines, next) = applog::lines_from(self.log_pos);
        if !lines.is_empty() {
            self.logs.extend(lines);
            self.log_pos = next;
            if self.logs.len() > 3000 {
                let drop = self.logs.len() - 2000;
                self.logs.drain(0..drop);
            }
        }
    }

    fn start_server(&mut self) {
        if self.starting || self.handle.is_some() {
            return;
        }
        self.starting = true;
        self.status = i18n::t("Запуск сервера…", "Starting server…").to_string();
        self.detail.clear();

        let name = self.name.trim();
        let name = if name.is_empty() {
            hostname::get()
                .ok()
                .and_then(|s| s.to_str().map(str::to_string))
                .unwrap_or_else(|| "PC".to_string())
        } else {
            name.to_string()
        };
        let device = if self.device_index > 0 {
            self.devices.get(self.device_index - 1).cloned()
        } else {
            None
        };
        self.config = Config {
            name: name.clone(),
            device_name: device,
            language: self.language,
        };
        let _ = self.config.save();

        match start(self.config.clone()) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.status = i18n::t("Сервер запущен", "Server running").to_string();
                self.detail = i18n::t(
                    "Телефон найдёт компьютер автоматически в приложении.",
                    "The phone will find this computer automatically in the app.",
                )
                .to_string();
                if let Some(t) = self.tray.as_mut() {
                    t.set_server_running(true);
                }
            }
            Err(err) => {
                self.status = format!("{}: {err}", i18n::t("Ошибка", "Error"));
                self.detail.clear();
                applog::log(format!(
                    "{}: {err}",
                    i18n::t("Не удалось запустить сервер", "Failed to start the server")
                ));
            }
        }
        self.starting = false;
        self.refresh_logs();
    }

    fn stop_server(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            self.status = i18n::t("Остановка…", "Stopping…").to_string();
            handle.stop();
            self.status = i18n::t("Сервер остановлен", "Server stopped").to_string();
            self.detail.clear();
            if let Some(t) = self.tray.as_mut() {
                t.set_server_running(false);
            }
        }
    }

    /// Применяет выбранный язык ко всем подписям и сохраняет настройку.
    fn apply_language(&mut self, ctx: &egui::Context) {
        self.config.language = self.language;
        let _ = self.config.save();
        i18n::init(self.language.resolve());
        if let Some(tray) = &self.tray {
            tray.apply_language();
        }
        self.status = if self.handle.is_some() {
            i18n::t("Сервер запущен", "Server running").to_string()
        } else if self.starting {
            i18n::t("Запуск сервера…", "Starting server…").to_string()
        } else {
            i18n::t("Сервер остановлен", "Server stopped").to_string()
        };
        if self.handle.is_some() {
            self.detail = i18n::t(
                "Телефон найдёт компьютер автоматически в приложении.",
                "The phone will find this computer automatically in the app.",
            )
            .to_string();
        }
        ctx.send_viewport_cmd(ViewportCommand::Title(i18n::window_title().to_string()));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            handle.stop();
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Закрытие окна: по умолчанию сворачиваем в трей, но выход из меню
        // трея должен закрывать программу по-настоящему.
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.quitting {
                // Разрешаем закрытие — сервер остановится в Drop.
            } else {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                crate::tray::hide_window(self.hwnd);
                applog::log(
                    i18n::t(
                        "Окно свёрнуто в трей, сервер продолжает работу",
                        "Window minimized to tray, the server keeps running",
                    )
                    .to_string(),
                );
            }
        }

        // Handle tray events
        let mut actions = Vec::new();
        if let Some(tray) = &self.tray {
            while let Some(action) = tray.try_recv() {
                actions.push(action);
            }
        }
        for action in actions {
            match action {
                TrayAction::ToggleServer => {
                    if self.handle.is_some() {
                        self.stop_server();
                    } else {
                        self.start_server();
                    }
                }
                TrayAction::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        if self.start_now && !self.start_now_done {
            self.start_now_done = true;
            self.start_server();
        }
        self.refresh_logs();
        if self.last_net_check.elapsed() >= Duration::from_secs(4) {
            self.last_net_check = Instant::now();
            self.refresh_network();
        }
        // Пока окно скрыто, продолжаем polling логов и событий.
        ctx.request_repaint_after(Duration::from_millis(500));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(18.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                ui.style_mut().visuals.override_text_color = Some(TEXT);
                ui.style_mut().spacing.item_spacing = egui::vec2(8.0, 7.0);

                ui.label(
                    RichText::new("WIFI audio")
                        .size(26.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    RichText::new(i18n::t(
                        "Звук с компьютера на телефон по Wi-Fi",
                        "PC audio to your phone over Wi-Fi",
                    ))
                    .size(14.0)
                    .color(MUTED),
                );
                ui.add_space(12.0);

                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, color) = if self.handle.is_some() {
                            ("●", OK)
                        } else if self.starting {
                            ("●", BUSY)
                        } else if self.status.starts_with(i18n::t("Ошибка", "Error")) {
                            ("●", ERR)
                        } else {
                            ("●", DIM)
                        };
                        ui.label(RichText::new(dot).size(22.0).color(color));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&self.status).size(17.0).strong().color(TEXT));
                            if !self.detail.is_empty() {
                                ui.label(RichText::new(&self.detail).size(12.0).color(MUTED));
                            }
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let text = if self.handle.is_some() {
                                    i18n::t("Остановить сервер", "Stop server")
                                } else {
                                    i18n::t("Запустить сервер", "Start server")
                                };
                                let enabled = !self.starting;
                                let button =
                                    egui::Button::new(
                                        RichText::new(text)
                                            .size(14.0)
                                            .strong()
                                            .color(BRAND_TEXT),
                                    )
                                    .min_size(egui::vec2(180.0, 40.0))
                                    .fill(BRAND)
                                    .stroke(egui::Stroke::NONE)
                                    .rounding(egui::Rounding::same(20.0));
                                if ui.add_enabled(enabled, button).clicked() {
                                    if self.handle.is_some() {
                                        self.stop_server();
                                    } else {
                                        self.start_server();
                                    }
                                }
                            },
                        );
                    });
                });

                if !self.net.warnings.is_empty() {
                    ui.add_space(2.0);
                    egui::Frame::none()
                        .fill(WARN_BG)
                        .stroke(egui::Stroke::new(1.0_f32, WARN_BORDER))
                        .rounding(egui::Rounding::same(20.0))
                        .inner_margin(egui::Margin::same(16.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                RichText::new(i18n::t(
                                    "⚠  Проверьте сеть",
                                    "⚠  Check your network",
                                ))
                                .size(15.0)
                                .strong()
                                .color(WARN_TEXT),
                            );
                            for warning in &self.net.warnings {
                                ui.add_space(2.0);
                                ui.label(RichText::new(warning).size(12.0).color(WARN_TEXT));
                            }
                        });
                }

                ui.add_space(2.0);
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&self.ip).size(18.0).strong().color(TEXT));
                        let adapter = self
                            .net
                            .primary_name
                            .as_deref()
                            .unwrap_or(i18n::t(
                                "IP этого компьютера",
                                "This computer's IP",
                            ));
                        ui.label(RichText::new(adapter).size(12.0).color(MUTED));
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if outlined_button(ui, i18n::t("Скопировать IP", "Copy IP"))
                                    .clicked()
                                {
                                    ctx.copy_text(self.ip.clone());
                                }
                            },
                        );
                    });

                    ui.add_space(3.0);
                    ui.horizontal(|ui| {
                        let (text, color) = match firewall::status() {
                            firewall::Status::Ok => (
                                i18n::t(
                                    "Брандмауэр: порты открыты",
                                    "Firewall: ports are open",
                                ),
                                OK,
                            ),
                            firewall::Status::Checking => (
                                i18n::t(
                                    "Брандмауэр: проверяю доступ…",
                                    "Firewall: checking access…",
                                ),
                                MUTED,
                            ),
                            firewall::Status::Declined => (
                                i18n::t(
                                    "Брандмауэр: доступ не разрешён — телефон может не подключиться.",
                                    "Firewall: access denied — the phone may not connect.",
                                ),
                                ERR,
                            ),
                            firewall::Status::Failed => (
                                i18n::t(
                                    "Брандмауэр: не удалось открыть порты автоматически.",
                                    "Firewall: could not open the ports automatically.",
                                ),
                                ERR,
                            ),
                            firewall::Status::Unknown => (
                                i18n::t(
                                    "Брандмауэр: порты 47832 / 47833",
                                    "Firewall: ports 47832 / 47833",
                                ),
                                MUTED,
                            ),
                        };
                        ui.label(RichText::new(text).size(11.0).color(color));
                        if matches!(
                            firewall::status(),
                            firewall::Status::Declined | firewall::Status::Failed
                        ) {
                            if outlined_button(ui, i18n::t("Открыть порты", "Open ports")).clicked()
                            {
                                firewall::ensure_async();
                            }
                        }
                    });
                });

                ui.add_space(4.0);
                section_card(ui, i18n::t("Настройки", "Settings"), |ui| {
                    let enabled = self.handle.is_none() && !self.starting;
                    ui.add_enabled_ui(enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [130.0, 22.0],
                                egui::Label::new(i18n::t("Имя компьютера", "Computer name")),
                            );
                            ui.add_sized(
                                [290.0, 26.0],
                                egui::TextEdit::singleline(&mut self.name),
                            );
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [130.0, 22.0],
                                egui::Label::new(i18n::t("Устройство вывода", "Output device")),
                            );
                            egui::ComboBox::from_id_salt("device")
                                .selected_text(if self.device_index == 0 {
                                    i18n::t("Системное (по умолчанию)", "System default")
                                } else {
                                    self.devices
                                        .get(self.device_index - 1)
                                        .map(String::as_str)
                                        .unwrap_or(i18n::t(
                                            "Системное (по умолчанию)",
                                            "System default",
                                        ))
                                })
                                .width(300.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.device_index,
                                        0,
                                        i18n::t("Системное (по умолчанию)", "System default"),
                                    );
                                    for (index, device) in self.devices.iter().enumerate() {
                                        ui.selectable_value(
                                            &mut self.device_index,
                                            index + 1,
                                            device,
                                        );
                                    }
                                });
                            if outlined_button(ui, i18n::t("Обновить", "Refresh"))
                                .on_hover_text(i18n::t(
                                    "Обновить список устройств",
                                    "Refresh the device list",
                                ))
                                .clicked()
                            {
                                self.devices = list_output_devices();
                                if self.device_index > self.devices.len() {
                                    self.device_index = 0;
                                }
                            }
                        });
                    });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [130.0, 22.0],
                            egui::Label::new(i18n::t("Язык", "Language")),
                        );
                        let previous = self.language;
                        egui::ComboBox::from_id_salt("language")
                            .selected_text(self.language.label())
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                for choice in
                                    [LangChoice::System, LangChoice::Ru, LangChoice::En]
                                {
                                    ui.selectable_value(
                                        &mut self.language,
                                        choice,
                                        choice.label(),
                                    );
                                }
                            });
                        if self.language != previous {
                            self.apply_language(ui.ctx());
                        }
                    });
                });

                ui.add_space(4.0);
                section_card(ui, i18n::t("Трей и автозапуск", "Tray and autostart"), |ui| {
                    ui.label(
                        RichText::new(i18n::t(
                            "Закрытие окна сворачивает программу в трей — сервер продолжает работать. Выход — через меню значка в трее.",
                            "Closing the window hides the app to the tray — the server keeps running. To quit, use the tray icon menu.",
                        ))
                        .size(12.0)
                        .color(MUTED),
                    );
                    ui.add_space(8.0);
                    if switch_row(
                        ui,
                        &mut self.autostart_enabled,
                        i18n::t(
                            "Запускать программу при входе в Windows",
                            "Start the app when Windows starts",
                        ),
                    )
                    .changed()
                    {
                        match autostart::set_autostart(self.autostart_enabled) {
                            Ok(()) => {}
                            Err(err) => {
                                self.autostart_enabled = autostart::is_autostart_enabled();
                                applog::log(format!(
                                    "{}: {err}",
                                    i18n::t(
                                        "Не удалось изменить автозапуск",
                                        "Failed to change autostart"
                                    )
                                ));
                            }
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(i18n::t(
                            "При автозапуске программа стартует в трее и сразу запускает сервер.",
                            "On autostart the app launches in the tray and starts the server immediately.",
                        ))
                        .size(11.0)
                        .color(MUTED),
                    );
                });

                ui.add_space(4.0);
                section_card(
                    ui,
                    i18n::t("Приложение для телефона", "Phone app"),
                    |ui| {
                        ui.label(
                            RichText::new(i18n::t(
                                "Нужен приёмник звука на Android? Скачайте приложение — это тот же APK, что на сайте.",
                                "Need the Android receiver? Download the app — the same APK as on the site.",
                            ))
                            .size(12.0)
                            .color(MUTED),
                        );
                        ui.add_space(8.0);
                        if outlined_button(
                            ui,
                            i18n::t("Скачать для Android", "Download for Android"),
                        )
                        .clicked()
                        {
                            open_url(ANDROID_APP_URL);
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(ANDROID_APP_URL).size(11.0).color(DIM),
                                )
                                .wrap()
                                .selectable(true),
                            );
                            if copy_icon_button(ui)
                                .on_hover_text(i18n::t("Копировать ссылку", "Copy link"))
                                .clicked()
                            {
                                ui.ctx().copy_text(ANDROID_APP_URL.to_string());
                                self.link_copied_until =
                                    Some(Instant::now() + Duration::from_secs(2));
                                ui.ctx().request_repaint_after(Duration::from_secs(2));
                            }
                        });
                        match self.link_copied_until {
                            Some(until) if Instant::now() < until => {
                                ui.label(
                                    RichText::new(i18n::t("Ссылка скопирована", "Link copied"))
                                        .size(11.0)
                                        .color(BRAND),
                                );
                                ui.ctx().request_repaint_after(Duration::from_millis(200));
                            }
                            Some(_) => self.link_copied_until = None,
                            None => {}
                        }
                    },
                );

                ui.add_space(4.0);
                section_card(ui, i18n::t("Журнал", "Log"), |ui| {
                    egui::Frame::none()
                        .fill(BG)
                        .rounding(egui::Rounding::same(12.0))
                        .inner_margin(egui::Margin::same(12.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .stick_to_bottom(true)
                                .auto_shrink([false, false])
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    if self.logs.is_empty() {
                                        ui.label(
                                            RichText::new(i18n::t(
                                                "Журнал пуст",
                                                "The log is empty",
                                            ))
                                            .monospace()
                                            .size(12.0)
                                            .color(DIM),
                                        );
                                    }
                                    for line in &self.logs {
                                        ui.label(
                                            RichText::new(line)
                                                .monospace()
                                                .size(12.0)
                                                .color(MUTED),
                                        );
                                    }
                                });
                        });
                });
            });
            });

                ctx.request_repaint_after(Duration::from_millis(400));
    }
}

/// Открывает ссылку в браузере по умолчанию.
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .fill(CARD)
        .rounding(egui::Rounding::same(20.0))
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Карточка с заголовком, как разделы в Android-приложении.
fn section_card<R>(
    ui: &mut egui::Ui,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card(ui, |ui| {
        ui.label(RichText::new(title).size(16.0).strong().color(TEXT));
        ui.add_space(6.0);
        add(ui)
    })
}

/// Небольшая квадратная кнопка с иконкой копирования (два листа).
fn copy_icon_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), CARD_ALT);
    }
    let color = if response.hovered() { BRAND } else { DIM };
    let stroke = egui::Stroke::new(1.3_f32, color);
    let rounding = egui::Rounding::same(2.0);
    let c = rect.center();
    // Задний лист смещён вправо-вверх, передний — влево-вниз.
    let back = egui::Rect::from_min_size(
        egui::pos2(c.x - 1.5, c.y - 5.5),
        egui::vec2(8.0, 10.0),
    );
    let front = egui::Rect::from_min_size(
        egui::pos2(c.x - 5.5, c.y - 3.5),
        egui::vec2(8.0, 10.0),
    );
    let painter = ui.painter();
    painter.rect_stroke(back, rounding, stroke);
    painter.rect_filled(front, rounding, CARD);
    painter.rect_stroke(front, rounding, stroke);
    response
}

/// Второстепенная кнопка Material 3: прозрачная с контуром и синим текстом.
fn outlined_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let button = egui::Button::new(RichText::new(text).size(13.0).color(BRAND))
        .min_size(egui::vec2(0.0, 34.0))
        .fill(Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0_f32, DIM))
        .rounding(egui::Rounding::same(17.0));
    ui.add(button)
}

/// Переключатель в стиле Material 3 (как MaterialSwitch на телефоне).
fn switch_row(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, mut response) =
            ui.allocate_exact_size(egui::vec2(46.0, 26.0), egui::Sense::click());
        let t = ui.ctx().animate_bool(response.id, *on);
        let track = if *on {
            BRAND
        } else {
            Color32::from_rgb(79, 76, 83)
        };
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(13.0), track);
        let cx = egui::lerp((rect.left() + 13.0)..=(rect.right() - 13.0), t);
        ui.painter()
            .circle_filled(egui::pos2(cx, rect.center().y), 10.0, Color32::WHITE);

        let label = ui.add(
            egui::Label::new(RichText::new(text).size(14.0).color(TEXT))
                .sense(egui::Sense::click()),
        );
        if response.clicked() || label.clicked() {
            *on = !*on;
            response.mark_changed();
            ui.ctx().request_repaint();
        }
        response
    })
    .inner
}

fn fit_window(_ctx: &egui::Context) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetSystemMetrics, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, SM_CXSCREEN,
        SM_CYSCREEN,
    };

    let title = HSTRING::from(i18n::window_title());

    unsafe {
        let Ok(hwnd) = FindWindowW(None, &title) else {
            return;
        };

        // Тёмный заголовок окна, чтобы он сочетался с интерфейсом.
        {
            use windows::Win32::Graphics::Dwm::{
                DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
            };
            let dark: i32 = 1;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }

        let scale = (windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) as f32 / 96.0).max(1.0);
        let width = (820.0 * scale).round() as i32;
        let height = (700.0 * scale).round() as i32;
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let x = ((screen_w - width) / 2).max(0);
        let y = ((screen_h - height) / 2).max(0);
        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(data) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".to_string(), egui::FontData::from_owned(data).into());
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "segoe".to_string());
        }
    }
    if let Ok(data) = std::fs::read(r"C:\Windows\Fonts\consola.ttf") {
        fonts
            .font_data
            .insert("consolas".to_string(), egui::FontData::from_owned(data).into());
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            family.insert(0, "consolas".to_string());
        }
    }
    ctx.set_fonts(fonts);
}

fn style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.extreme_bg_color = CARD_ALT;
    visuals.faint_bg_color = CARD_ALT;
    visuals.window_stroke = egui::Stroke::NONE;
    visuals.hyperlink_color = BRAND;

    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = CARD_ALT;
    w.noninteractive.weak_bg_fill = CARD_ALT;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    w.noninteractive.rounding = egui::Rounding::same(12.0);

    w.inactive.bg_fill = CARD_ALT;
    w.inactive.weak_bg_fill = CARD_ALT;
    w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    w.inactive.rounding = egui::Rounding::same(12.0);

    w.hovered.bg_fill = Color32::from_rgb(43, 41, 51);
    w.hovered.weak_bg_fill = Color32::from_rgb(43, 41, 51);
    w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, DIM);
    w.hovered.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    w.hovered.rounding = egui::Rounding::same(12.0);

    w.active.bg_fill = Color32::from_rgb(52, 50, 61);
    w.active.weak_bg_fill = Color32::from_rgb(52, 50, 61);
    w.active.bg_stroke = egui::Stroke::new(1.0_f32, BRAND);
    w.active.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    w.active.rounding = egui::Rounding::same(12.0);

    w.open.bg_fill = CARD_ALT;
    w.open.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    w.open.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    w.open.rounding = egui::Rounding::same(12.0);

    visuals.selection.bg_fill = BRAND;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, BRAND_TEXT);
    visuals.widgets.hovered.expansion = 0.0;
    visuals.widgets.active.expansion = 0.0;

    style.visuals = visuals;
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.interact_size = egui::vec2(40.0, 28.0);
    ctx.set_style(style);
}

