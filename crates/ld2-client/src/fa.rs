//! M24: **Font Awesome Free** — второй источник иконок рядом с Lucide
//! (Ely `IconName`). SVG лежат в `assets/fa/<стиль>/<имя>.svg`,
//! таблица генерируется `build.rs` (`OUT_DIR/fa_icons.rs`); рисуются
//! gpui-элементом `svg().data(...)` — тем же путём, что иконки Ely
//! (масштаб в бокс, тон — цветом текста; fill-стиль FA ложится:
//! растеризация идёт по альфа-маске).
//!
//! Лицензия иконок — CC BY 4.0: атрибуция в `assets/fa/LICENSE.txt`
//! и в embedded-комментариях самих SVG — файлы не править.
//!
//! Путь иконки: `<стиль>/<имя>` — `solid/house`, `brands/github`,
//! `regular/clock`.

include!(concat!(env!("OUT_DIR"), "/fa_icons.rs"));

use gpui::{prelude::*, svg, Hsla, Pixels, Svg};

/// Иконка Font Awesome в боксе `size` цветом `color`. Неизвестный
/// путь — пустой бокс и ошибка в лог (паритет с broken-иконкой Ely).
// API подключён для M24 (админ-панель) — до первого применения.
#[allow(dead_code)]
pub(crate) fn fa_icon(path: &str, size: Pixels, color: impl Into<Hsla>) -> Svg {
    match get(path) {
        Some(bytes) => svg().data(bytes).size(size).flex_none().text_color(color),
        None => {
            tracing::error!("fa: иконка {path} не найдена в assets/fa");
            svg().size(size).flex_none().text_color(color)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Набор подключён: три стиля, тысячи иконок, без дублей путей.
    #[test]
    fn fa_table_covers_three_styles() {
        assert!(ICONS.len() > 2_000, "мало иконок: {}", ICONS.len());
        for style in ["solid/", "regular/", "brands/"] {
            assert!(
                ICONS.iter().any(|(p, _)| p.starts_with(style)),
                "нет стиля {style}"
            );
        }
        let mut paths: Vec<&str> = ICONS.iter().map(|(p, _)| *p).collect();
        paths.sort_unstable();
        let before = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), before, "пути уникальны");
    }

    /// Известные иконки на месте; неизвестная — None.
    #[test]
    fn fa_known_icons_present() {
        for path in [
            "solid/house",
            "solid/maximize",
            "brands/github",
            "regular/clock",
        ] {
            assert!(get(path).is_some_and(|bytes| !bytes.is_empty()), "{path}");
        }
        assert!(get("solid/definitely-missing").is_none());
    }

    /// SVG валиден и атрибуция CC BY 4.0 сохранена (embedded-комментарий).
    #[test]
    fn fa_svg_keeps_license_comment() {
        let text = std::str::from_utf8(get("solid/house").expect("house")).expect("utf8");
        assert!(text.contains("<svg"), "это SVG");
        assert!(text.contains("Font Awesome"), "атрибуция в файле");
        assert!(text.contains("CC BY 4.0"), "лицензия в файле");
    }
}
