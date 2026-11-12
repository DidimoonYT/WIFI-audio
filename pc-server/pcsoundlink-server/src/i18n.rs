//! Локализация интерфейса и сообщений сервера.
//!
//! Поддерживаются русский и английский. По умолчанию берётся язык Windows;
//! пользователь может выбрать язык вручную — выбор сохраняется в конфиге.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

/// Пользовательский выбор языка: системный или один из доступных.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangChoice {
    System,
    Ru,
    En,
}

impl Default for LangChoice {
    fn default() -> Self {
        Self::System
    }
}

impl LangChoice {
    pub fn resolve(self) -> Lang {
        match self {
            Self::System => system_lang(),
            Self::Ru => Lang::Ru,
            Self::En => Lang::En,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Ru => "ru",
            Self::En => "en",
        }
    }

    pub fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "ru" => Self::Ru,
            "en" => Self::En,
            _ => Self::System,
        }
    }

    /// Подпись для выпадающего списка на текущем языке интерфейса.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => t("Системный", "System"),
            Self::Ru => "Русский",
            Self::En => "English",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn init(lang: Lang) {
    CURRENT.store(
        match lang {
            Lang::Ru => 0,
            Lang::En => 1,
        },
        Ordering::SeqCst,
    );
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::SeqCst) == 1 {
        Lang::En
    } else {
        Lang::Ru
    }
}

/// Возвращает строку на текущем языке. Оба варианта должны быть литералами.
pub fn t(ru: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Ru => ru,
        Lang::En => en,
    }
}

/// Заголовок окна сервера — используется и при поиске окна через Win32.
pub fn window_title() -> &'static str {
    t("WIFI audio — сервер", "WIFI audio — Server")
}

fn system_lang() -> Lang {
    use windows::Win32::Globalization::GetUserDefaultUILanguage;

    // Младшие 10 бит — идентификатор основного языка. 0x19 — русский.
    let id = unsafe { GetUserDefaultUILanguage() };
    if id & 0x3ff == 0x19 {
        Lang::Ru
    } else {
        Lang::En
    }
}
