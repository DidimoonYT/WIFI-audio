//! Системный трей Windows.
//!
//! Иконка создаётся на главном потоке (там, где работает цикл сообщений winit).
//! Фоновый поток слушает пункты меню и сам показывает/скрывает окно через
//! Win32: скрытое окно winit не перерисовывается, поэтому одних
//! `ViewportCommand` недостаточно.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindowVisible, SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOWNORMAL,
};

static SHOULD_HIDE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ToggleServer,
    Quit,
}

/// Программа сейчас должна быть скрыта в трее.
pub fn should_hide() -> bool {
    SHOULD_HIDE.load(Ordering::SeqCst)
}

pub fn hide_window(hwnd: isize) {
    SHOULD_HIDE.store(true, Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = ShowWindow(HWND(hwnd as *mut _), SW_HIDE);
        }
    }
}

pub fn show_window(hwnd: isize) {
    SHOULD_HIDE.store(false, Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn window_visible(hwnd: isize) -> bool {
    hwnd != 0 && unsafe { IsWindowVisible(HWND(hwnd as *mut _)).as_bool() }
}

pub struct Tray {
    icon: TrayIcon,
    show_item: MenuItem,
    hide_item: MenuItem,
    toggle_item: MenuItem,
    quit_item: MenuItem,
    tx: mpsc::Sender<TrayAction>,
    rx: mpsc::Receiver<TrayAction>,
    running: bool,
}

impl Tray {
    pub fn new(
        egui_ctx: eframe::egui::Context,
        hwnd: isize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel();

        let menu = Menu::new();
        let show_item = MenuItem::new(crate::i18n::t("Показать окно", "Show window"), true, None);
        let hide_item = MenuItem::new(crate::i18n::t("Скрыть в трей", "Hide to tray"), true, None);
        let toggle_item = MenuItem::new(
            crate::i18n::t("Запустить сервер", "Start server"),
            true,
            None,
        );
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::new(crate::i18n::t("Выход", "Quit"), true, None);

        menu.append(&show_item)?;
        menu.append(&hide_item)?;
        menu.append(&toggle_item)?;
        menu.append(&separator)?;
        menu.append(&quit_item)?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip(false))
            .with_icon(crate::icons::tray_icon()?)
            .build()?;

        // Единственный обработчик меню: фоновый поток.
        let show_id = show_item.id().clone();
        let hide_id = hide_item.id().clone();
        let toggle_id = toggle_item.id().clone();
        let quit_id = quit_item.id().clone();
        let tx_events = tx.clone();
        std::thread::Builder::new()
            .name("tray-events".to_string())
            .spawn(move || loop {
                let Ok(event) = MenuEvent::receiver().recv() else {
                    break;
                };
                if event.id == show_id {
                    show_window(hwnd);
                } else if event.id == hide_id {
                    hide_window(hwnd);
                } else if event.id == toggle_id {
                    // Если окно скрыто, покажем его, чтобы GUI точно проснулся.
                    if !window_visible(hwnd) {
                        show_window(hwnd);
                    }
                    if tx_events.send(TrayAction::ToggleServer).is_err() {
                        break;
                    }
                    egui_ctx.request_repaint();
                } else if event.id == quit_id {
                    // Показываем окно, иначе eframe не получит кадр и не завершится.
                    if !window_visible(hwnd) {
                        show_window(hwnd);
                    }
                    if tx_events.send(TrayAction::Quit).is_err() {
                        break;
                    }
                    egui_ctx.request_repaint();
                }
            })
            .map_err(|err| -> Box<dyn std::error::Error> { Box::new(err) })?;

        Ok(Self {
            icon,
            show_item,
            hide_item,
            toggle_item,
            quit_item,
            tx,
            rx,
            running: false,
        })
    }

    pub fn try_recv(&self) -> Option<TrayAction> {
        self.rx.try_recv().ok()
    }

    pub fn set_server_running(&mut self, running: bool) {
        if self.running == running {
            return;
        }
        self.running = running;
        self.apply_language();
    }

    /// Обновляет подписи меню после смены языка интерфейса.
    pub fn apply_language(&self) {
        use crate::i18n::t;
        let _ = self.show_item.set_text(t("Показать окно", "Show window"));
        let _ = self.hide_item.set_text(t("Скрыть в трей", "Hide to tray"));
        let _ = self.toggle_item.set_text(if self.running {
            t("Остановить сервер", "Stop server")
        } else {
            t("Запустить сервер", "Start server")
        });
        let _ = self.quit_item.set_text(t("Выход", "Quit"));
        let _ = self.icon.set_tooltip(Some(tooltip(self.running)));
    }

    /// Канал используется только для отладки/тестов, но держим его живым.
    #[allow(dead_code)]
    pub fn sender(&self) -> mpsc::Sender<TrayAction> {
        self.tx.clone()
    }
}

fn tooltip(running: bool) -> &'static str {
    use crate::i18n::t;
    if running {
        t(
            "WIFI audio — сервер запущен",
            "WIFI audio — server running",
        )
    } else {
        t(
            "WIFI audio — сервер остановлен",
            "WIFI audio — server stopped",
        )
    }
}
