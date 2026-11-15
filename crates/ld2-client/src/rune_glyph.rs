//! M18/F58f: знаки рун — глифы шрифта **Dwarven Runes**
//! (`assets/fonts/dwarven-runes.ttf`, embedded в `main.rs`).
//!
//! Шрифт маппит латиницу `a–z`/`A–Z` на рунные глифы (62 код-поинта).
//! Семейство — `"Dwarven Runes"` (name-таблица шрифта). Здесь — одна
//! точка правды: какой глиф и каким шрифтом рисовать знаки руна-слотов
//! (окно предмета, окно инкрустации; F58f переиспользует).

use gpui::{div, prelude::*, Div, SharedString};

/// Семейство шрифта рун (name-таблица `dwarven-runes.ttf`).
pub(crate) const RUNE_FONT: &str = "Dwarven Runes";

/// Резолвер знака руны: **поле `glyph` из контента** (параметры руны,
/// `stats.glyph` → wire `RuneInfo.glyph`) — если это латинская буква
/// (глиф шрифта `a..=z`/`A..=Z`); иначе/нет поля — стабильный хеш
/// имени (старые руны/сервер без поля, §11).
pub(crate) fn resolve_glyph(glyph: Option<&str>, key: &str) -> char {
    glyph
        .and_then(|g| g.chars().find(|c| c.is_ascii_alphabetic()))
        .unwrap_or_else(|| rune_glyph(key))
}

/// Div со знаком руны из контента (с фолбэком на хеш имени).
pub(crate) fn glyph_div_for(glyph: Option<&str>, key: &str) -> Div {
    glyph_div(resolve_glyph(glyph, key))
}

/// Стабильный рунный глиф по ключу (def_id/имя руны): djb2 → `a..=z`.
/// Одинаковый ключ всегда даёт одинаковый знак — у каждой руны своя
/// метка в описании и в списке инкрустации.
pub(crate) fn rune_glyph(key: &str) -> char {
    let mut h: u32 = 5381;
    for b in key.as_bytes() {
        h = h.wrapping_mul(33).wrapping_add(u32::from(*b));
    }
    (b'a' + (h % 26) as u8) as char
}

/// Div с рунным глифом (шрифт Dwarven Runes) — знак руны/слота.
pub(crate) fn glyph_div(glyph: char) -> Div {
    div()
        .font_family(RUNE_FONT)
        .child(SharedString::from(format!("{glyph}")))
}

/// Пустой руна-слот — **пустой квадрат** (указание заказчика).
/// Обычный шрифт: Dwarven Runes не содержит квадратов, а □ (U+25A1)
/// есть в системных шрифтах всех платформ.
pub(crate) fn empty_slot_glyph_div() -> Div {
    div().child("\u{25A1}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ключ даёт стабильный глиф в диапазоне шрифта `a..=z`.
    #[test]
    fn rune_glyph_is_stable_and_in_font_range() {
        for key in ["rune_fire", "rune_frost", "", "Меч тысячи истин", "😀"] {
            let g = rune_glyph(key);
            assert!(g.is_ascii_lowercase(), "глиф {g:?} вне a..=z для {key:?}");
            assert_eq!(g, rune_glyph(key), "детерминирован: {key:?}");
        }
    }

    /// Поле `glyph` из контента имеет приоритет; мусорное значение
    /// (не латинская буква) и отсутствие поля → фолбэк на хеш имени.
    #[test]
    fn resolve_glyph_prefers_content_field() {
        assert_eq!(resolve_glyph(Some("x"), "Руна огня"), 'x');
        assert_eq!(resolve_glyph(Some("F"), "Руна огня"), 'F');
        // Не буква (кириллица/символ) — хеш имени.
        assert_eq!(
            resolve_glyph(Some("ф"), "rune_fire"),
            rune_glyph("rune_fire")
        );
        assert_eq!(
            resolve_glyph(Some(""), "rune_fire"),
            rune_glyph("rune_fire")
        );
        assert_eq!(resolve_glyph(None, "rune_fire"), rune_glyph("rune_fire"));
    }

    /// Разные руны, как правило, получают разные знаки (для ключей из
    /// контента коллизии неприемлемы в UI-тестах списка).
    #[test]
    fn rune_glyphs_differ_for_content_runes() {
        let keys = ["rune_fire", "rune_frost", "rune_storm"];
        let glyphs: Vec<char> = keys.iter().map(|k| rune_glyph(k)).collect();
        for (i, a) in glyphs.iter().enumerate() {
            for b in glyphs.iter().skip(i + 1) {
                assert_ne!(a, b, "коллизия глифов: {keys:?}");
            }
        }
    }
}
