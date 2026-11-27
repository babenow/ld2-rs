//! M18/F58f: символы самоцветов по раритету — шрифт **Symbola**
//! (`assets/fonts/symbola.ttf`, embedded в `main.rs`).
//!
//! Указание заказчика: у гемов знак в отображении (рюкзак и окно
//! информации — предмет и слот) зависит от раритета: закрашенные
//! фигуры **по числу сторон**. Код-поинты проверены по cmap шрифта.

use ld2_core::domain;

use gpui::{div, prelude::*, Div, SharedString};

/// Семейство шрифта знаков (name-таблица `symbola.ttf`).
pub(crate) const GEM_FONT: &str = "Symbola";

/// Закрашенная фигура раритета: common ▲ (3 стороны), uncommon ■ (4),
/// rare ⬟ (5), epic ⬢ (6), legendary — **BLACK OCTAGON** U+2BC4 (8,
/// код от заказчика; U+2CB4 был неверен — это коптский знак, выглядит
/// как руна). Неизвестный раритет — `None` (без знака).
pub(crate) fn gem_symbol(rarity: &str) -> Option<char> {
    match rarity {
        domain::RARITY_COMMON => Some('\u{25B2}'),
        domain::RARITY_UNCOMMON => Some('\u{25A0}'),
        domain::RARITY_RARE => Some('\u{2B1F}'),
        domain::RARITY_EPIC => Some('\u{2B22}'),
        domain::RARITY_LEGENDARY => Some('\u{2BC4}'),
        _ => None,
    }
}

/// Div со знаком раритета (шрифт Symbola); неизвестный раритет —
/// пустой Div (ничего не показываем).
pub(crate) fn gem_symbol_div(rarity: &str) -> Div {
    match gem_symbol(rarity) {
        Some(c) => div()
            .font_family(GEM_FONT)
            .child(SharedString::from(format!("{c}"))),
        None => div(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M18/F58f: у каждого раритета свой знак (по числу сторон);
    /// неизвестное значение — без знака.
    #[test]
    fn gem_symbol_covers_all_rarities() {
        assert_eq!(
            gem_symbol(domain::RARITY_COMMON),
            Some('\u{25B2}'),
            "треугольник"
        );
        assert_eq!(
            gem_symbol(domain::RARITY_UNCOMMON),
            Some('\u{25A0}'),
            "квадрат"
        );
        assert_eq!(
            gem_symbol(domain::RARITY_RARE),
            Some('\u{2B1F}'),
            "пентагон"
        );
        assert_eq!(
            gem_symbol(domain::RARITY_EPIC),
            Some('\u{2B22}'),
            "гексагон"
        );
        assert_eq!(
            gem_symbol(domain::RARITY_LEGENDARY),
            Some('\u{2BC4}'),
            "BLACK OCTAGON (код заказчика)"
        );
        assert_eq!(gem_symbol("mythical"), None, "неизвестный — без знака");
        assert_eq!(gem_symbol(""), None);
    }
}
