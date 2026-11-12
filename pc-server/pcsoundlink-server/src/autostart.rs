//! Автозапуск: добавление/удаление программы из автозагрузки Windows.

use std::env;

use anyhow::Result;
use winreg::enums::*;
use winreg::RegKey;

const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_NAME: &str = "PcSoundLinkServer";

/// Аргументы, с которыми программа запускается из автозагрузки:
/// сразу стартует сервер и остаётся в трее.
fn autostart_command(exe: &str) -> String {
    format!("\"{exe}\" --autostart --minimized")
}

pub fn is_autostart_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run_key) = hkcu.open_subkey(RUN_KEY_PATH) else {
        return false;
    };
    let Ok(value) = run_key.get_value::<String, _>(APP_NAME) else {
        return false;
    };
    let Ok(exe) = env::current_exe() else {
        return false;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    value.to_lowercase().contains(&exe)
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu.open_subkey_with_flags(RUN_KEY_PATH, KEY_WRITE)?;

    if enabled {
        let exe = env::current_exe()?;
        let exe_str = exe.to_string_lossy().to_string();
        run_key.set_value(APP_NAME, &autostart_command(&exe_str))?;
        crate::applog::log(format!(
            "{}: {exe_str} --autostart --minimized",
            crate::i18n::t("Автозапуск включён", "Autostart enabled")
        ));
    } else {
        let _ = run_key.delete_value(APP_NAME);
        crate::applog::log(
            crate::i18n::t("Автозапуск отключён", "Autostart disabled").to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_has_quotes_and_flags() {
        let cmd = autostart_command(r"C:\Program Files\PcSoundLink\Server.exe");
        assert_eq!(
            cmd,
            r#""C:\Program Files\PcSoundLink\Server.exe" --autostart --minimized"#
        );
    }

    #[test]
    fn registry_roundtrip() {
        // Не трогаем реальную запись, если она уже указывает на другой EXE.
        let was_enabled = is_autostart_enabled();
        set_autostart(true).unwrap();
        assert!(is_autostart_enabled());
        set_autostart(false).unwrap();
        assert!(!is_autostart_enabled());
        if was_enabled {
            set_autostart(true).unwrap();
        }
    }
}
