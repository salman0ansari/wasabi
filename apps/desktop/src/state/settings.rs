//! Device-local desktop preferences. These intentionally live outside the
//! account database so visual and startup preferences survive logout.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct DeviceSettings {
    pub version: u32,
    pub language: String,
    pub launch_at_startup: bool,
    pub diagnostics: bool,
    pub theme: ThemePreference,
    pub text_scale: u16,
    pub enter_to_send: bool,
    pub spellcheck: bool,
    pub link_previews: bool,
    pub desktop_notifications: bool,
    pub notification_sound: bool,
    pub notification_previews: bool,
    pub suppress_when_focused: bool,
    pub download_path: String,
    pub cache_quota_mb: u64,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        let download_path = dirs::download_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .into_owned();
        Self {
            version: 1,
            language: "System default".into(),
            launch_at_startup: false,
            diagnostics: false,
            theme: ThemePreference::System,
            text_scale: 100,
            enter_to_send: true,
            spellcheck: true,
            link_previews: true,
            desktop_notifications: true,
            notification_sound: true,
            notification_previews: true,
            suppress_when_focused: true,
            download_path,
            cache_quota_mb: 1024,
        }
    }
}

impl DeviceSettings {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("wasabi")
            .join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("settings path has no parent"))?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        self.sync_autostart()
    }

    fn sync_autostart(&self) -> io::Result<()> {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("autostart")
            .join("wasabi.desktop");
        if !self.launch_at_startup {
            match fs::remove_file(path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            }
        }

        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("autostart path has no parent"))?;
        fs::create_dir_all(parent)?;
        let executable = std::env::current_exe()?;
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Wasabi\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            executable.to_string_lossy().replace('"', "\\\"")
        );
        let temporary = path.with_extension("desktop.tmp");
        fs::write(&temporary, entry)?;
        fs::rename(temporary, path)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Account,
    Privacy,
    #[default]
    Chats,
    Notifications,
    Storage,
    Shortcuts,
    Help,
}

impl SettingsSection {
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Account,
        Self::Privacy,
        Self::Chats,
        Self::Notifications,
        Self::Storage,
        Self::Shortcuts,
        Self::Help,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Account => "Account",
            Self::Privacy => "Privacy",
            Self::Chats => "Chats",
            Self::Notifications => "Notifications",
            Self::Storage => "Storage and data",
            Self::Shortcuts => "Keyboard shortcuts",
            Self::Help => "Help",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceSettings;

    #[test]
    fn settings_json_is_backward_compatible_via_defaults() {
        let parsed: DeviceSettings = serde_json::from_str(r#"{"enter_to_send":false}"#).unwrap();
        assert!(!parsed.enter_to_send);
        assert_eq!(parsed.text_scale, 100);
        assert!(parsed.desktop_notifications);
    }
}
