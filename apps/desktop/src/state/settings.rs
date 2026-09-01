//! Device-local desktop preferences. These intentionally live outside the
//! account database so visual and startup preferences survive logout.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use wasabi_domain::{MediaAvailability, MessageKind, parse_push_name};

pub const SETTINGS_VERSION: u32 = 1;
pub const CACHE_QUOTA_CHOICES_MB: [u64; 3] = [256, 1024, 4096];
pub const AUTO_DOWNLOAD_VIDEO_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const AUTO_DOWNLOAD_DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;

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
    pub theme: ThemePreference,
    pub text_scale: u16,
    pub reduce_motion: bool,
    pub enter_to_send: bool,
    pub desktop_notifications: bool,
    pub notification_sound: bool,
    pub notification_previews: bool,
    pub suppress_when_focused: bool,
    pub download_path: String,
    pub cache_quota_mb: u64,
    pub auto_download_photos: bool,
    pub auto_download_audio: bool,
    pub auto_download_video: bool,
    pub auto_download_documents: bool,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        let download_path = dirs::download_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .into_owned();
        Self {
            version: SETTINGS_VERSION,
            language: "System default".into(),
            launch_at_startup: false,
            theme: ThemePreference::System,
            text_scale: 100,
            reduce_motion: false,
            enter_to_send: true,
            desktop_notifications: true,
            notification_sound: true,
            notification_previews: true,
            suppress_when_focused: true,
            download_path,
            cache_quota_mb: 1024,
            auto_download_photos: true,
            auto_download_audio: true,
            auto_download_video: false,
            auto_download_documents: false,
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

    pub fn data_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("wasabi")
    }

    pub fn load() -> Self {
        Self::load_from(Self::path())
    }

    fn load_from(path: impl AsRef<std::path::Path>) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default()
            .normalized()
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

    fn normalized(mut self) -> Self {
        self.version = SETTINGS_VERSION;
        if !matches!(self.text_scale, 100 | 125 | 150) {
            self.text_scale = 100;
        }
        if !CACHE_QUOTA_CHOICES_MB.contains(&self.cache_quota_mb) {
            self.cache_quota_mb = 1024;
        }
        if self.download_path.trim().is_empty() {
            self.download_path = DeviceSettings::default().download_path;
        }
        self
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
            "[Desktop Entry]\nType=Application\nName=wasabi\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            executable.to_string_lossy().replace('"', "\\\"")
        );
        let temporary = path.with_extension("desktop.tmp");
        fs::write(&temporary, entry)?;
        fs::rename(temporary, path)
    }
}

/// Whether a newly received payload should start downloading on this computer.
/// Unknown sizes are treated as unbounded and skipped.
pub fn should_auto_download(kind: &MessageKind, settings: &DeviceSettings) -> bool {
    let (enabled, media, cap) = match kind {
        MessageKind::Image { media, .. } => (
            settings.auto_download_photos,
            media,
            AUTO_DOWNLOAD_DEFAULT_MAX_BYTES,
        ),
        MessageKind::Audio { media, .. } => (
            settings.auto_download_audio,
            media,
            AUTO_DOWNLOAD_DEFAULT_MAX_BYTES,
        ),
        MessageKind::Video { media, .. } => (
            settings.auto_download_video,
            media,
            AUTO_DOWNLOAD_VIDEO_MAX_BYTES,
        ),
        MessageKind::Document { media } => (
            settings.auto_download_documents,
            media,
            AUTO_DOWNLOAD_DEFAULT_MAX_BYTES,
        ),
        _ => return false,
    };
    enabled
        && media.availability == MediaAvailability::Remote
        && media.file_size.is_some_and(|size| size > 0 && size <= cap)
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
    use super::{
        AUTO_DOWNLOAD_DEFAULT_MAX_BYTES, DeviceSettings, SETTINGS_VERSION, should_auto_download,
    };
    use wasabi_domain::{
        MediaAvailability, MediaDescriptor, MediaId, MessageKind, PrivacyCategory, PrivacyValue,
        parse_push_name,
    };

    fn image(availability: MediaAvailability, file_size: Option<u64>) -> MessageKind {
        MessageKind::Image {
            caption: None,
            media: MediaDescriptor {
                id: MediaId::new("photo-1"),
                mime_type: Some("image/jpeg".into()),
                file_name: None,
                file_size,
                duration_seconds: None,
                width: None,
                height: None,
                availability,
            },
        }
    }

    fn video(availability: MediaAvailability, file_size: Option<u64>) -> MessageKind {
        MessageKind::Video {
            caption: None,
            video_note: false,
            media: MediaDescriptor {
                id: MediaId::new("video-1"),
                mime_type: Some("video/mp4".into()),
                file_name: None,
                file_size,
                duration_seconds: None,
                width: None,
                height: None,
                availability,
            },
        }
    }

    #[test]
    fn settings_json_is_backward_compatible_via_defaults() {
        let parsed: DeviceSettings = serde_json::from_str(r#"{"enter_to_send":false}"#).unwrap();
        assert!(!parsed.enter_to_send);
        assert_eq!(parsed.text_scale, 100);
        assert!(!parsed.reduce_motion);
        assert!(parsed.desktop_notifications);
        assert!(parsed.auto_download_photos);
        assert!(parsed.auto_download_audio);
        assert!(!parsed.auto_download_video);
        assert!(!parsed.auto_download_documents);
    }

    #[test]
    fn auto_download_predicate_respects_kind_size_and_availability() {
        let settings = DeviceSettings::default();
        assert!(should_auto_download(
            &image(
                MediaAvailability::Remote,
                Some(AUTO_DOWNLOAD_DEFAULT_MAX_BYTES)
            ),
            &settings
        ));
        assert!(!should_auto_download(
            &video(MediaAvailability::Remote, Some(1024)),
            &settings
        ));
        assert!(!should_auto_download(
            &image(MediaAvailability::Unavailable, Some(1024)),
            &settings
        ));
        assert!(!should_auto_download(
            &image(MediaAvailability::Remote, None),
            &settings
        ));
    }

    #[test]
    fn privacy_value_mapping_roundtrips_last_and_read_receipts() {
        assert_eq!(PrivacyCategory::Last.as_wire(), "last");
        assert_eq!(
            PrivacyCategory::from_wire(PrivacyCategory::Last.as_wire()),
            Some(PrivacyCategory::Last)
        );
        assert_eq!(
            PrivacyValue::from_wire(PrivacyValue::All.as_wire()),
            Some(PrivacyValue::All)
        );
        assert_eq!(
            PrivacyValue::from_wire(PrivacyValue::None.as_wire()),
            Some(PrivacyValue::None)
        );
        assert!(PrivacyCategory::Last.accepts(PrivacyValue::Contacts));
        assert!(PrivacyCategory::ReadReceipts.accepts(PrivacyValue::All));
        assert!(PrivacyCategory::ReadReceipts.accepts(PrivacyValue::None));
        assert!(!PrivacyCategory::ReadReceipts.accepts(PrivacyValue::Contacts));
        assert_eq!(
            PrivacyCategory::from_wire(PrivacyCategory::ReadReceipts.as_wire()),
            Some(PrivacyCategory::ReadReceipts)
        );
    }

    #[test]
    fn profile_set_push_name_rejects_empty() {
        assert_eq!(parse_push_name(""), Err("Name cannot be empty"));
        assert_eq!(parse_push_name("\t  \n"), Err("Name cannot be empty"));
        assert_eq!(parse_push_name("  Maya  "), Ok("Maya".to_string()));
    }

    #[test]
    fn corrupt_settings_recover_to_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(&path, b"{ definitely not json").unwrap();

        assert_eq!(DeviceSettings::load_from(path).version, SETTINGS_VERSION);
    }

    #[test]
    fn invalid_bounded_values_are_normalized() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"version":99,"text_scale":999,"cache_quota_mb":7,"download_path":""}"#,
        )
        .unwrap();

        let settings = DeviceSettings::load_from(path);
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.text_scale, 100);
        assert_eq!(settings.cache_quota_mb, 1024);
        assert!(!settings.download_path.is_empty());
    }
}
