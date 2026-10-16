use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hotkey::{format_hotkey, Hotkey};
use crate::localization::{tr, Language};

pub const MAX_SELF_TRANSPARENCY: u8 = 80;
pub const DEFAULT_PRESET_LEVELS: [u8; 5] = [0, 25, 50, 75, 100];

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Debug)]
pub struct Settings {
    pub language: Language,
    pub default_container: bool,
    pub close_to_tray: bool,
    pub self_transparency: u8,
    pub show_icons: bool,
    pub privacy_mode: bool,
    pub restore_hotkey: Option<Hotkey>,
    pub preset_levels: [u8; 5],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::default(),
            default_container: false,
            close_to_tray: true,
            self_transparency: 0,
            show_icons: false,
            privacy_mode: false,
            restore_hotkey: Some(Hotkey::default()),
            preset_levels: DEFAULT_PRESET_LEVELS,
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self, String> {
        let path = settings_path()?;
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(tr(
                    &format!("无法读取设置 {}：{error}", path.display()),
                    &format!("Could not read settings {}: {error}", path.display()),
                )
                .to_owned());
            }
        };
        parse_settings(&content).map_err(|error| {
            tr(
                &format!("设置文件 {} 无效：{error}", path.display()),
                &format!("Invalid settings file {}: {error}", path.display()),
            )
            .to_owned()
        })
    }

    pub fn save(&self) -> Result<(), String> {
        if self.self_transparency > MAX_SELF_TRANSPARENCY {
            return Err(tr(
                &format!("自身透明度必须在 0 到 {MAX_SELF_TRANSPARENCY} 之间"),
                &format!("Trans transparency must be between 0 and {MAX_SELF_TRANSPARENCY}"),
            )
            .to_owned());
        }
        if let Some(hotkey) = self.restore_hotkey {
            hotkey.validate()?;
        }
        for (index, level) in self.preset_levels.iter().enumerate() {
            if *level > 100 {
                return Err(tr(
                    &format!("预设挡位 {} 必须为 0 到 100 的整数", index + 1),
                    &format!("Preset {} must be an integer between 0 and 100", index + 1),
                )
                .to_owned());
            }
        }
        let path = settings_path()?;
        let directory = path.parent().ok_or(tr(
            "无法确定设置文件目录",
            "Could not determine the settings file directory",
        ))?;
        fs::create_dir_all(directory).map_err(|error| {
            tr(
                &format!("无法创建设置目录 {}：{error}", directory.display()),
                &format!(
                    "Could not create settings directory {}: {error}",
                    directory.display()
                ),
            )
            .to_owned()
        })?;
        let (temporary, mut file) = create_temporary_file(directory)?;
        let content = format!(
            "; Trans settings\nlanguage={}\ndefault_container={}\nclose_to_tray={}\nself_transparency={}\nshow_icons={}\nprivacy_mode={}\nrestore_hotkey={}\npreset_1={}\npreset_2={}\npreset_3={}\npreset_4={}\npreset_5={}\n",
            self.language.as_str(),
            self.default_container,
            self.close_to_tray,
            self.self_transparency,
            self.show_icons,
            self.privacy_mode,
            format_hotkey(self.restore_hotkey),
            self.preset_levels[0],
            self.preset_levels[1],
            self.preset_levels[2],
            self.preset_levels[3],
            self.preset_levels[4],
        );

        if let Err(error) = file
            .write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(tr(
                &format!("无法写入设置临时文件 {}：{error}", temporary.display()),
                &format!(
                    "Could not write temporary settings file {}: {error}",
                    temporary.display()
                ),
            )
            .to_owned());
        }
        drop(file);

        // 同目录替换成功后才更新配置，不能先删除已有设置文件
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(tr(
                &format!("无法替换设置文件 {}：{error}", path.display()),
                &format!(
                    "Could not replace settings file {}: {error}",
                    path.display()
                ),
            )
            .to_owned());
        }
        Ok(())
    }
}

fn settings_path() -> Result<PathBuf, String> {
    let directory = env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(tr(
            "无法确定设置目录：APPDATA 环境变量未设置",
            "Could not determine the settings directory: APPDATA is not set",
        ))?;
    if !directory.is_absolute() {
        return Err(tr(
            "无法确定设置目录：APPDATA 必须是绝对路径",
            "Could not determine the settings directory: APPDATA must be an absolute path",
        )
        .to_owned());
    }
    Ok(directory.join("Trans").join("settings.ini"))
}

fn parse_settings(content: &str) -> Result<Settings, String> {
    let mut settings = Settings::default();
    let mut found_keys = HashSet::new();
    for (index, line) in content.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            if known_key(line) {
                return Err(tr(
                    &format!("第 {} 行缺少设置值", index + 1),
                    &format!("Missing setting value on line {}", index + 1),
                )
                .to_owned());
            }
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if known_key(key) && !found_keys.insert(key) {
            return Err(tr(
                &format!("第 {} 行重复定义 {key}", index + 1),
                &format!("Duplicate {key} definition on line {}", index + 1),
            )
            .to_owned());
        }
        match key {
            "language" => {
                settings.language = Language::parse(value).map_err(|error| {
                    tr(
                        &format!("第 {} 行 language 无效：{error}", index + 1),
                        &format!("Invalid language on line {}: {error}", index + 1),
                    )
                    .to_owned()
                })?;
            }
            "default_container" => {
                settings.default_container = parse_bool(value, key, index + 1)?;
            }
            "close_to_tray" => {
                settings.close_to_tray = parse_bool(value, key, index + 1)?;
            }
            "self_transparency" => {
                settings.self_transparency = value
                    .parse::<u8>()
                    .ok()
                    .filter(|value| *value <= MAX_SELF_TRANSPARENCY)
                    .ok_or_else(|| {
                        tr(
                            &format!("第 {} 行 self_transparency 必须为 0 到 {MAX_SELF_TRANSPARENCY} 的整数", index + 1),
                            &format!("self_transparency on line {} must be an integer between 0 and {MAX_SELF_TRANSPARENCY}", index + 1),
                        )
                        .to_owned()
                    })?;
            }
            "show_icons" => settings.show_icons = parse_bool(value, key, index + 1)?,
            "privacy_mode" => settings.privacy_mode = parse_bool(value, key, index + 1)?,
            "restore_hotkey" => {
                settings.restore_hotkey = Hotkey::parse_saved(value).map_err(|error| {
                    tr(
                        &format!("第 {} 行 restore_hotkey 无效：{error}", index + 1),
                        &format!("Invalid restore_hotkey on line {}: {error}", index + 1),
                    )
                    .to_owned()
                })?;
            }
            "preset_1" | "preset_2" | "preset_3" | "preset_4" | "preset_5" => {
                let preset_index = match key {
                    "preset_1" => 0,
                    "preset_2" => 1,
                    "preset_3" => 2,
                    "preset_4" => 3,
                    _ => 4,
                };
                settings.preset_levels[preset_index] = value
                    .parse::<u8>()
                    .ok()
                    .filter(|value| *value <= 100)
                    .ok_or_else(|| {
                        tr(
                            &format!("第 {} 行 {key} 必须为 0 到 100 的整数", index + 1),
                            &format!(
                                "{key} on line {} must be an integer between 0 and 100",
                                index + 1
                            ),
                        )
                        .to_owned()
                    })?;
            }
            _ => {}
        }
    }
    Ok(settings)
}

fn known_key(key: &str) -> bool {
    matches!(
        key,
        "language"
            | "default_container"
            | "close_to_tray"
            | "self_transparency"
            | "show_icons"
            | "privacy_mode"
            | "restore_hotkey"
            | "preset_1"
            | "preset_2"
            | "preset_3"
            | "preset_4"
            | "preset_5"
    )
}

fn parse_bool(value: &str, key: &str, line: usize) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(tr(
            &format!("第 {line} 行 {key} 必须为 true 或 false"),
            &format!("{key} on line {line} must be true or false"),
        )
        .to_owned()),
    }
}

fn create_temporary_file(directory: &Path) -> Result<(PathBuf, File), String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for _ in 0..16 {
        let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".settings.ini.{}.{timestamp}.{serial}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(tr(
                    &format!("无法创建设置临时文件 {}：{error}", path.display()),
                    &format!(
                        "Could not create temporary settings file {}: {error}",
                        path.display()
                    ),
                )
                .to_owned());
            }
        }
    }
    Err(tr(
        "无法创建唯一的设置临时文件，请稍后重试",
        "Could not create a unique temporary settings file. Please try again later",
    )
    .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, VK_F1};

    #[test]
    fn empty_configuration_uses_chinese_and_existing_defaults() {
        let settings = parse_settings("").unwrap();
        assert_eq!(settings.language, Language::Chinese);
        assert!(!settings.default_container);
        assert!(settings.close_to_tray);
        assert_eq!(settings.self_transparency, 0);
        assert!(!settings.show_icons);
        assert!(!settings.privacy_mode);
        assert_eq!(settings.restore_hotkey, Some(Hotkey::default()));
        assert_eq!(settings.preset_levels, DEFAULT_PRESET_LEVELS);
    }

    #[test]
    fn legacy_configuration_without_language_preserves_other_values() {
        let settings = parse_settings(
            "close_to_tray=false\nself_transparency=35\nshow_icons=true\nprivacy_mode=true\nrestore_hotkey=Alt+F12\npreset_1=10\npreset_2=30\npreset_3=45\npreset_4=70\npreset_5=90\n",
        )
        .unwrap();
        assert_eq!(settings.language, Language::Chinese);
        assert!(!settings.default_container);
        assert!(!settings.close_to_tray);
        assert_eq!(settings.self_transparency, 35);
        assert!(settings.show_icons);
        assert!(settings.privacy_mode);
        assert_eq!(
            settings.restore_hotkey,
            Some(Hotkey {
                modifiers: MOD_ALT,
                key: u32::from(VK_F1) + 11,
            })
        );
        assert_eq!(settings.preset_levels, [10, 30, 45, 70, 90]);
    }

    #[test]
    fn english_configuration_preserves_presets_and_hotkey() {
        let settings = parse_settings(
            "language=EN-us\ndefault_container=true\nclose_to_tray=false\nself_transparency=80\nshow_icons=true\nprivacy_mode=true\nrestore_hotkey=Ctrl+Alt+F24\npreset_1=100\npreset_2=0\npreset_3=60\npreset_4=60\npreset_5=15\n",
        )
        .unwrap();
        assert_eq!(settings.language, Language::English);
        assert!(settings.default_container);
        assert!(!settings.close_to_tray);
        assert_eq!(settings.self_transparency, 80);
        assert!(settings.show_icons);
        assert!(settings.privacy_mode);
        assert_eq!(
            settings.restore_hotkey,
            Some(Hotkey {
                modifiers: MOD_CONTROL | MOD_ALT,
                key: u32::from(VK_F1) + 23,
            })
        );
        assert_eq!(settings.preset_levels, [100, 0, 60, 60, 15]);
    }

    #[test]
    fn changing_only_language_keeps_all_other_defaults() {
        let settings = parse_settings("language=en\n").unwrap();
        let defaults = Settings::default();
        assert_eq!(settings.language, Language::English);
        assert_eq!(settings.default_container, defaults.default_container);
        assert_eq!(settings.close_to_tray, defaults.close_to_tray);
        assert_eq!(settings.self_transparency, defaults.self_transparency);
        assert_eq!(settings.show_icons, defaults.show_icons);
        assert_eq!(settings.privacy_mode, defaults.privacy_mode);
        assert_eq!(settings.restore_hotkey, defaults.restore_hotkey);
        assert_eq!(settings.preset_levels, defaults.preset_levels);
    }

    #[test]
    fn accepts_bom_comments_whitespace_and_unknown_keys() {
        let settings = parse_settings(
            "\u{feff}; Trans settings\r\n# migration\r\n\r\n language = en \r\n show_icons = true \r\n future_option=anything\r\nunknown_section\r\n",
        )
        .unwrap();
        assert_eq!(settings.language, Language::English);
        assert!(settings.show_icons);
        assert_eq!(settings.preset_levels, DEFAULT_PRESET_LEVELS);
    }

    #[test]
    fn rejects_duplicate_known_keys_including_language() {
        for (key, value) in [
            ("language", "en"),
            ("default_container", "true"),
            ("close_to_tray", "true"),
            ("self_transparency", "0"),
            ("show_icons", "false"),
            ("privacy_mode", "false"),
            ("restore_hotkey", "None"),
            ("preset_1", "0"),
            ("preset_2", "25"),
            ("preset_3", "50"),
            ("preset_4", "75"),
            ("preset_5", "100"),
        ] {
            let content = format!("{key}={value}\n {key} = {value}\n");
            assert!(
                parse_settings(&content).is_err(),
                "accepted duplicate {key}"
            );
        }
    }

    #[test]
    fn rejects_language_without_assignment_or_value() {
        for content in [
            "language",
            "language=",
            "language = \t",
            "language\nenable=true",
        ] {
            assert!(parse_settings(content).is_err(), "accepted {content:?}");
        }
    }

    #[test]
    fn rejects_unknown_language_in_configuration() {
        for value in ["fr", "zh-TW", "English", "en=zh", "123"] {
            let content = format!("language={value}\n");
            assert!(parse_settings(&content).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn rejects_missing_assignment_for_other_known_keys() {
        for key in [
            "default_container",
            "close_to_tray",
            "self_transparency",
            "show_icons",
            "privacy_mode",
            "restore_hotkey",
            "preset_1",
            "preset_2",
            "preset_3",
            "preset_4",
            "preset_5",
        ] {
            assert!(parse_settings(key).is_err(), "accepted missing {key}");
        }
    }

    #[test]
    fn accepts_self_transparency_boundaries() {
        for value in [0, MAX_SELF_TRANSPARENCY] {
            let content = format!("self_transparency={value}\n");
            assert_eq!(parse_settings(&content).unwrap().self_transparency, value);
        }
    }

    #[test]
    fn rejects_out_of_range_or_non_integer_self_transparency() {
        for value in ["81", "100", "255", "256", "-1", "0.5", "NaN", ""] {
            let content = format!("self_transparency={value}\n");
            assert!(parse_settings(&content).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn accepts_each_preset_boundary_and_keeps_missing_presets() {
        for index in 0..5 {
            for value in [0, 100] {
                let content = format!("preset_{}={value}\n", index + 1);
                let mut expected = DEFAULT_PRESET_LEVELS;
                expected[index] = value;
                assert_eq!(parse_settings(&content).unwrap().preset_levels, expected);
            }
        }
    }

    #[test]
    fn rejects_invalid_values_in_each_preset() {
        for index in 1..=5 {
            for value in ["101", "255", "256", "-1", "1.5", "NaN", ""] {
                let content = format!("preset_{index}={value}\n");
                assert!(parse_settings(&content).is_err(), "accepted {content:?}");
            }
        }
    }

    #[test]
    fn rejects_invalid_boolean_values() {
        for key in [
            "default_container",
            "close_to_tray",
            "show_icons",
            "privacy_mode",
        ] {
            for value in ["", "1", "0", "yes", "TRUE", "False"] {
                let content = format!("{key}={value}\n");
                assert!(parse_settings(&content).is_err(), "accepted {content:?}");
            }
        }
    }

    #[test]
    fn disabled_hotkey_and_legacy_win_hotkey_load_correctly() {
        let disabled = parse_settings("language=en\nrestore_hotkey=None\n").unwrap();
        assert_eq!(disabled.restore_hotkey, None);
        assert_eq!(disabled.language, Language::English);
        let legacy = parse_settings("restore_hotkey=Win+Alt+T\n").unwrap();
        assert_eq!(legacy.restore_hotkey, Some(Hotkey::default()));
        assert_eq!(legacy.language, Language::Chinese);
    }

    #[test]
    fn rejects_invalid_hotkeys_in_configuration() {
        for value in [
            "",
            "Ctrl",
            "Ctrl+Ctrl+R",
            "Ctrl+R+S",
            "Win+Windows+R",
            "Alt+F25",
        ] {
            let content = format!("restore_hotkey={value}\n");
            assert!(parse_settings(&content).is_err(), "accepted {value:?}");
        }
    }
}
