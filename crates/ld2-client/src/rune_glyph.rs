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

/// Глиф **пустого** руна-слота — любой `a..=z` (подбор визуальный,
/// F58f может заменить одной правкой константы).
pub(crate) const EMPTY_SLOT_GLYPH: char = 'a';

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

/// Заполненный руна-слот: глиф по ключу руны (def_id или имя).
pub(crate) fn rune_glyph_div(key: &str) -> Div {
    glyph_div(rune_glyph(key))
}

/// Пустой руна-слот.
pub(crate) fn empty_slot_glyph_div() -> Div {
    glyph_div(EMPTY_SLOT_GLYPH)
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
