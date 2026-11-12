//! Автоматическое открытие портов в брандмауэре Windows.
//!
//! Правила брандмауэра нельзя создать без прав администратора, поэтому при
//! первом запуске приложение перезапускает само себя с повышением прав
//! (`--install-firewall`). После этого правила сохраняются в системе и больше
//! ничего спрашивать не будут.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use windows::core::{BSTR, PCWSTR};
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, NetFwPolicy2, NetFwRule, NET_FW_ACTION_ALLOW,
    NET_FW_IP_PROTOCOL_ANY, NET_FW_IP_PROTOCOL_UDP, NET_FW_PROFILE2_ALL, NET_FW_RULE_DIR_IN,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::applog::log;
use crate::i18n::t;
use crate::protocol::{CONTROL_PORT, DISCOVERY_PORT};

const PROGRAM_RULE: &str = "PC Sound Link (программа)";
const UDP_RULE: &str = "PC Sound Link (UDP)";

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Unknown = 0,
    Checking = 1,
    Ok = 2,
    Declined = 3,
    Failed = 4,
}

impl Status {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Checking,
            2 => Self::Ok,
            3 => Self::Declined,
            4 => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

static STATUS: AtomicU8 = AtomicU8::new(Status::Unknown as u8);
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    Status::from_u8(STATUS.load(Ordering::SeqCst))
}

/// Правила на месте, включены и указывают на текущий файл?
pub fn rules_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    with_rules(|rules| {
        for name in [PROGRAM_RULE, UDP_RULE] {
            let rule = unsafe { rules.Item(&BSTR::from(name)) }
                .map_err(|_| anyhow!("правило не найдено"))?;
            let enabled = unsafe { rule.Enabled() }
                .map(|value| value.0 != 0)
                .unwrap_or(false);
            if !enabled {
                return Err(anyhow!("правило выключено"));
            }
            if name == PROGRAM_RULE {
                let application = unsafe { rule.ApplicationName() }?;
                if !application.to_string().to_lowercase().eq(&exe) {
                    return Err(anyhow!("правило указывает на другой файл"));
                }
            }
        }
        Ok(())
    })
    .is_ok()
}

/// Создаёт (или пересоздаёт) правила брандмауэра. Требует прав администратора.
pub fn install_rules() -> Result<()> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy().to_string();

    with_rules(|rules| {
        for name in [PROGRAM_RULE, UDP_RULE] {
            let _ = unsafe { rules.Remove(&BSTR::from(name)) };
        }

        let program: INetFwRule =
            unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_ALL) }?;
        unsafe {
            program.SetName(&BSTR::from(PROGRAM_RULE))?;
            program.SetDescription(&BSTR::from(
                "PC Sound Link: приём команд и звука с телефона",
            ))?;
            program.SetApplicationName(&BSTR::from(exe.as_str()))?;
            program.SetProtocol(NET_FW_IP_PROTOCOL_ANY.0)?;
            program.SetDirection(NET_FW_RULE_DIR_IN)?;
            program.SetAction(NET_FW_ACTION_ALLOW)?;
            program.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
            program.SetEnabled(VARIANT_TRUE)?;
        }
        unsafe { rules.Add(&program) }?;

        let udp: INetFwRule = unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_ALL) }?;
        unsafe {
            udp.SetName(&BSTR::from(UDP_RULE))?;
            udp.SetDescription(&BSTR::from(
                "PC Sound Link: UDP-порты управления и поиска",
            ))?;
            udp.SetProtocol(NET_FW_IP_PROTOCOL_UDP.0)?;
            udp.SetLocalPorts(&BSTR::from(format!(
                "{CONTROL_PORT},{DISCOVERY_PORT}"
            )))?;
            udp.SetDirection(NET_FW_RULE_DIR_IN)?;
            udp.SetAction(NET_FW_ACTION_ALLOW)?;
            udp.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
            udp.SetEnabled(VARIANT_TRUE)?;
        }
        unsafe { rules.Add(&udp) }?;
        Ok(())
    })
}

/// Проверяет правила и, если их нет, один раз запрашивает повышение прав.
/// Работает в фоне, чтобы не блокировать окно.
pub fn ensure_async() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    STATUS.store(Status::Checking as u8, Ordering::SeqCst);
    thread::Builder::new()
        .name("firewall".into())
        .spawn(|| {
            let result = ensure();
            STATUS.store(result as u8, Ordering::SeqCst);
            RUNNING.store(false, Ordering::SeqCst);
        })
        .ok();
}

fn ensure() -> Status {
    if rules_installed() {
        return Status::Ok;
    }

    // Если приложение уже запущено от администратора — просто добавим правила.
    if install_rules().is_ok() {
        log(t("Брандмауэр: порты открыты", "Firewall: ports are open"));
        return Status::Ok;
    }

    log(t(
        "Брандмауэр: открываю порты, подтвердите запрос Windows…",
        "Firewall: opening ports, please confirm the Windows prompt…",
    ));
    match request_elevation() {
        Elevation::Started => {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if rules_installed() {
                    log(t("Брандмауэр: порты открыты", "Firewall: ports are open"));
                    return Status::Ok;
                }
                thread::sleep(Duration::from_millis(400));
            }
            log(t(
                "Брандмауэр: не удалось открыть порты автоматически",
                "Firewall: could not open the ports automatically",
            ));
            Status::Failed
        }
        Elevation::Cancelled => {
            log(t(
                "Брандмауэр: открытие портов отменено. Телефон может не подключиться.",
                "Firewall: opening ports was cancelled. The phone may not connect.",
            ));
            Status::Declined
        }
        Elevation::Error(err) => {
            log(format!("{}: {err}", t("Брандмауэр", "Firewall")));
            Status::Failed
        }
    }
}

enum Elevation {
    Started,
    Cancelled,
    Error(String),
}

fn request_elevation() -> Elevation {
    let Ok(exe) = std::env::current_exe() else {
        return Elevation::Error(
            t(
                "не удалось определить путь к программе",
                "failed to determine the program path",
            )
            .to_string(),
        );
    };
    let file = windows::core::HSTRING::from(exe.as_os_str());
    let verb = windows::core::HSTRING::from("runas");
    let parameters = windows::core::HSTRING::from("--install-firewall");

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR::null(),
            SW_HIDE,
        )
    };
    let code = result.0 as isize;
    // ShellExecuteW возвращает значение <= 32 при ошибке, в том числе когда
    // пользователь отказался от запроса UAC (ERROR_ACCESS_DENIED = 5).
    if code > 32 {
        Elevation::Started
    } else if code == 5 {
        Elevation::Cancelled
    } else {
        Elevation::Error(format!(
            "{} {code}",
            t("код ошибки запуска", "launch error code")
        ))
    }
}

fn with_rules<T>(action: impl FnOnce(&windows::Win32::NetworkManagement::WindowsFirewall::INetFwRules) -> Result<T>) -> Result<T> {
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
    let result = (|| -> Result<T> {
        let policy: INetFwPolicy2 = unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_ALL) }?;
        let rules = unsafe { policy.Rules() }?;
        action(&rules)
    })();
    if initialized {
        unsafe { CoUninitialize() };
    }
    result
}
