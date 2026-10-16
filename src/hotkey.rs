use std::fmt;

use crate::localization::tr;

use windows_sys::Win32::UI::Controls::{HOTKEYF_ALT, HOTKEYF_CONTROL, HOTKEYF_EXT, HOTKEYF_SHIFT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F24,
    VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_NUMLOCK, VK_PAUSE, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SCROLL, VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP,
};

const SUPPORTED_MODIFIERS: u32 = MOD_CONTROL | MOD_ALT | MOD_SHIFT;
const CONTROL_FLAGS: u32 = HOTKEYF_CONTROL | HOTKEYF_ALT | HOTKEYF_SHIFT | HOTKEYF_EXT;

const SPECIAL_KEYS: &[(u16, &str, bool)] = &[
    (VK_BACK, "Backspace", false),
    (VK_TAB, "Tab", false),
    (VK_RETURN, "Enter", false),
    (VK_PAUSE, "Pause", false),
    (VK_ESCAPE, "Esc", false),
    (VK_SPACE, "Space", false),
    (VK_PRIOR, "PageUp", true),
    (VK_NEXT, "PageDown", true),
    (VK_END, "End", true),
    (VK_HOME, "Home", true),
    (VK_LEFT, "Left", true),
    (VK_UP, "Up", true),
    (VK_RIGHT, "Right", true),
    (VK_DOWN, "Down", true),
    (VK_INSERT, "Insert", true),
    (VK_DELETE, "Delete", true),
    (VK_SNAPSHOT, "PrintScreen", true),
    (VK_NUMLOCK, "NumLock", true),
    (VK_SCROLL, "ScrollLock", false),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            modifiers: MOD_CONTROL | MOD_SHIFT,
            key: u32::from(b'R'),
        }
    }
}

impl Hotkey {
    pub fn validate(&self) -> Result<(), String> {
        if self.modifiers & !SUPPORTED_MODIFIERS != 0 {
            return Err(tr(
                "快捷键包含不支持的修饰键，只支持 Ctrl、Alt 和 Shift",
                "Only Ctrl, Alt, and Shift modifiers are supported",
            )
            .into());
        }
        if !supported_key(self.key) {
            return Err(tr("快捷键主键只支持字母、数字、F1 到 F24 和常用导航编辑键", "The hotkey must use a letter, a number, F1–F24, or a common navigation or editing key").into());
        }
        Ok(())
    }

    pub fn parse(value: &str) -> Result<Option<Self>, String> {
        Self::parse_value(value, false)
    }

    pub(crate) fn parse_saved(value: &str) -> Result<Option<Self>, String> {
        if value
            .split('+')
            .any(|part| legacy_win_modifier(part.trim()))
        {
            Self::parse_value(value, true)
        } else {
            Self::parse(value)
        }
    }

    fn parse_value(value: &str, allow_legacy_win: bool) -> Result<Option<Self>, String> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("None") {
            return Ok(None);
        }
        if value.is_empty() {
            return Err(tr(
                "快捷键不能为空，禁用时请使用 None",
                "The hotkey cannot be empty; use None to disable it",
            )
            .into());
        }

        let mut modifiers = 0;
        let mut key = None;
        let mut legacy_win = false;
        for part in value.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return Err(tr(
                    "快捷键中存在空按键，请使用 Ctrl+Shift+R 这样的格式",
                    "The hotkey contains an empty key; use a format such as Ctrl+Shift+R",
                )
                .into());
            }
            if legacy_win_modifier(part) {
                if !allow_legacy_win {
                    return Err(tr(
                        "快捷键修饰键只支持 Ctrl、Alt 和 Shift",
                        "Only Ctrl, Alt, and Shift modifiers are supported",
                    )
                    .into());
                }
                if legacy_win {
                    return Err(format!(
                        "{} {part}",
                        tr("快捷键重复定义修饰键", "Duplicate hotkey modifier:")
                    ));
                }
                legacy_win = true;
                continue;
            }
            let modifier = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Some(MOD_CONTROL),
                "alt" => Some(MOD_ALT),
                "shift" => Some(MOD_SHIFT),
                _ => None,
            };
            if let Some(modifier) = modifier {
                if modifiers & modifier != 0 {
                    return Err(format!(
                        "{} {part}",
                        tr("快捷键重复定义修饰键", "Duplicate hotkey modifier:")
                    ));
                }
                modifiers |= modifier;
            } else {
                if key.is_some() {
                    return Err(tr(
                        "快捷键只能包含一个主键",
                        "A hotkey can contain only one main key",
                    )
                    .into());
                }
                key = Some(parse_key(part).ok_or_else(|| {
                    format!(
                        "{}{part}",
                        tr("不支持的快捷键主键：", "Unsupported hotkey key: ")
                    )
                })?);
            }
        }
        let hotkey = Self {
            modifiers,
            key: key.ok_or(tr("快捷键缺少主键", "The hotkey is missing a main key"))?,
        };
        hotkey.validate()?;
        // 旧组合完整校验后替换为默认值，不能仅删除 Win 后留下裸字母
        Ok(Some(if legacy_win { Self::default() } else { hotkey }))
    }

    pub fn to_control(&self) -> Result<u16, String> {
        self.validate()?;

        let mut flags = 0;
        for (modifier, control_flag) in [
            (MOD_CONTROL, HOTKEYF_CONTROL),
            (MOD_ALT, HOTKEYF_ALT),
            (MOD_SHIFT, HOTKEYF_SHIFT),
        ] {
            if self.modifiers & modifier != 0 {
                flags |= control_flag;
            }
        }
        if SPECIAL_KEYS
            .iter()
            .any(|&(key, _, extended)| u32::from(key) == self.key && extended)
        {
            flags |= HOTKEYF_EXT;
        }
        Ok((self.key | (flags << 8)) as u16)
    }

    pub fn from_control(value: u16) -> Result<Option<Self>, String> {
        let key = u32::from(value & 0xff);
        let flags = u32::from(value >> 8);
        if flags & !CONTROL_FLAGS != 0 {
            return Err(tr(
                "快捷键控件返回了不支持的修饰键标记",
                "The hotkey control returned unsupported modifier flags",
            )
            .into());
        }
        if key == 0 {
            return Ok(None);
        }

        let mut modifiers = 0;
        for (control_flag, modifier) in [
            (HOTKEYF_CONTROL, MOD_CONTROL),
            (HOTKEYF_ALT, MOD_ALT),
            (HOTKEYF_SHIFT, MOD_SHIFT),
        ] {
            if flags & control_flag != 0 {
                modifiers |= modifier;
            }
        }
        // 全局快捷键按虚拟键识别，EXT 仅用于原生控件显示导航键
        let hotkey = Self { modifiers, key };
        hotkey.validate()?;
        Ok(Some(hotkey))
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, label) in [
            (MOD_CONTROL, "Ctrl"),
            (MOD_ALT, "Alt"),
            (MOD_SHIFT, "Shift"),
        ] {
            if self.modifiers & modifier != 0 {
                write!(formatter, "{label}+")?;
            }
        }
        if self.key >= u32::from(b'0') && self.key <= u32::from(b'9')
            || self.key >= u32::from(b'A') && self.key <= u32::from(b'Z')
        {
            return write!(formatter, "{}", char::from(self.key as u8));
        }
        if self.key >= u32::from(VK_F1) && self.key <= u32::from(VK_F24) {
            return write!(formatter, "F{}", self.key - u32::from(VK_F1) + 1);
        }
        if let Some(&(_, label, _)) = SPECIAL_KEYS
            .iter()
            .find(|&&(key, _, _)| u32::from(key) == self.key)
        {
            return formatter.write_str(label);
        }
        write!(formatter, "VK0x{:02X}", self.key)
    }
}

pub fn format_hotkey(hotkey: Option<Hotkey>) -> String {
    hotkey.map_or_else(|| "None".into(), |hotkey| hotkey.to_string())
}

fn legacy_win_modifier(value: &str) -> bool {
    value.eq_ignore_ascii_case("Win") || value.eq_ignore_ascii_case("Windows")
}

fn supported_key(key: u32) -> bool {
    (u32::from(b'0')..=u32::from(b'9')).contains(&key)
        || (u32::from(b'A')..=u32::from(b'Z')).contains(&key)
        || (u32::from(VK_F1)..=u32::from(VK_F24)).contains(&key)
        || SPECIAL_KEYS
            .iter()
            .any(|&(special, _, _)| u32::from(special) == key)
}

fn parse_key(value: &str) -> Option<u32> {
    let value = value.to_ascii_uppercase();
    if value.len() == 1 {
        let key = value.as_bytes()[0];
        if key.is_ascii_alphanumeric() {
            return Some(u32::from(key));
        }
    }
    if let Some(function) = value.strip_prefix('F') {
        if let Ok(number @ 1..=24) = function.parse::<u32>() {
            return Some(u32::from(VK_F1) + number - 1);
        }
    }
    if let Some(&(key, _, _)) = SPECIAL_KEYS
        .iter()
        .find(|&&(_, label, _)| label.eq_ignore_ascii_case(&value))
    {
        return Some(u32::from(key));
    }
    match value.as_str() {
        "BACK" => Some(u32::from(VK_BACK)),
        "RETURN" => Some(u32::from(VK_RETURN)),
        "ESCAPE" => Some(u32::from(VK_ESCAPE)),
        "PGUP" => Some(u32::from(VK_PRIOR)),
        "PGDN" => Some(u32::from(VK_NEXT)),
        "INS" => Some(u32::from(VK_INSERT)),
        "DEL" => Some(u32::from(VK_DELETE)),
        "PRINTSCRN" | "PRTSC" => Some(u32::from(VK_SNAPSHOT)),
        _ => None,
    }
}
