use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Chinese,
    English,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chinese => "zh-CN",
            Self::English => "en",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" => Ok(Self::Chinese),
            "en" | "en-us" => Ok(Self::English),
            _ => Err(tr("语言必须为 zh-CN 或 en", "Language must be zh-CN or en").into()),
        }
    }
}

thread_local! {
    static LANGUAGE: Cell<Language> = const { Cell::new(Language::Chinese) };
}

pub fn set_language(language: Language) {
    LANGUAGE.with(|current| current.set(language));
}

pub fn tr<'a>(chinese: &'a str, english: &'a str) -> &'a str {
    LANGUAGE.with(|current| match current.get() {
        Language::Chinese => chinese,
        Language::English => english,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct RestoreLanguage(Language);

    impl RestoreLanguage {
        fn new() -> Self {
            Self(LANGUAGE.with(Cell::get))
        }
    }

    impl Drop for RestoreLanguage {
        fn drop(&mut self) {
            set_language(self.0);
        }
    }

    #[test]
    fn chinese_is_the_default_language() {
        assert_eq!(Language::default(), Language::Chinese);
        let initial = std::thread::spawn(|| tr("设置", "Settings"))
            .join()
            .unwrap();
        assert_eq!(initial, "设置");
    }

    #[test]
    fn parses_chinese_aliases_with_case_and_whitespace() {
        for value in ["zh", "zh-CN", "ZH", "ZH-cn", " \tZh-Cn\r\n"] {
            assert_eq!(Language::parse(value).unwrap(), Language::Chinese);
        }
    }

    #[test]
    fn parses_english_aliases_with_case_and_whitespace() {
        for value in ["en", "en-US", "EN", "EN-us", " \tEn-Us\r\n"] {
            assert_eq!(Language::parse(value).unwrap(), Language::English);
        }
    }

    #[test]
    fn rejects_unknown_or_empty_language_values() {
        for value in ["", " ", "fr", "zh-TW", "en-GB", "English", "中文"] {
            assert!(Language::parse(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn canonical_language_names_round_trip() {
        for (language, expected) in [(Language::Chinese, "zh-CN"), (Language::English, "en")] {
            assert_eq!(language.as_str(), expected);
            assert_eq!(Language::parse(language.as_str()).unwrap(), language);
        }
    }

    #[test]
    fn translation_changes_immediately_in_both_directions() {
        let _restore = RestoreLanguage::new();
        set_language(Language::Chinese);
        assert_eq!(tr("设置", "Settings"), "设置");
        set_language(Language::English);
        assert_eq!(tr("设置", "Settings"), "Settings");
        set_language(Language::Chinese);
        assert_eq!(tr("设置", "Settings"), "设置");
    }

    #[test]
    fn languages_are_isolated_between_concurrent_threads() {
        let _restore = RestoreLanguage::new();
        set_language(Language::Chinese);
        let barrier = Arc::new(Barrier::new(2));
        let child_barrier = Arc::clone(&barrier);
        let child = std::thread::spawn(move || {
            let initial = tr("设置", "Settings");
            set_language(Language::English);
            child_barrier.wait();
            let english = tr("设置", "Settings");
            child_barrier.wait();
            (initial, english)
        });
        barrier.wait();
        let parent_language = tr("设置", "Settings");
        barrier.wait();
        assert_eq!(child.join().unwrap(), ("设置", "Settings"));
        assert_eq!(parent_language, "设置");
        assert_eq!(tr("设置", "Settings"), "设置");
    }
}
