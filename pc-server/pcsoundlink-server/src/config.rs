//! Persistent user settings. Only the computer name and the output device are
//! configurable now: packet size, gain and discovery are fixed and always on.

use anyhow::Result;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub device_name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: hostname::get()
                .ok()
                .and_then(|s| s.to_str().map(str::to_string))
                .unwrap_or_else(|| "PC".to_string()),
            device_name: None,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).or_else(dirs::config_dir)?;
    Some(base.join("PcSoundLink").join("config.txt"))
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let Some(path) = config_path() else {
            return cfg;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return cfg;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "name" if !value.trim().is_empty() => cfg.name = value.trim().to_string(),
                "device" if !value.trim().is_empty() => {
                    cfg.device_name = Some(value.trim().to_string())
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = config_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = format!("name={}\n", self.name);
        if let Some(device) = &self.device_name {
            text.push_str(&format!("device={device}\n"));
        }
        fs::write(path, text)?;
        Ok(())
    }
}
