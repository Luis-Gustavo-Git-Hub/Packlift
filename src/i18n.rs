use std::{collections::HashMap, sync::LazyLock};

static TRANSLATIONS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    // The English text is both the key and the fallback. GTK supplies language
    // preferences in order, including language-only variants of locale names.
    let column = glib::language_names()
        .iter()
        .find_map(|locale| match locale.split(['_', '-', '.', '@']).next()? {
            "en" | "C" | "POSIX" => Some(0),
            "pt" => Some(1),
            "es" => Some(2),
            "ru" => Some(3),
            "zh" => Some(4),
            _ => None,
        })
        .unwrap_or(0);
    if column == 0 {
        return HashMap::new();
    }

    include_str!("ui-translations.txt")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('|');
            let english = fields.next()?;
            let translated = fields.nth(column - 1)?;
            (!translated.is_empty()).then_some((english, translated))
        })
        .collect()
});

pub fn tr(english: &'static str) -> &'static str {
    TRANSLATIONS.get(english).copied().unwrap_or(english)
}
