//! Бейджи шапки: локация (pvp/safe/флаги) и статус сервера. Вынесено из
//! `root.rs` (RF3j).

use super::*;

/// Тон бейджа (M14/F1): предупреждение/успех/нейтральный.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BadgeTone {
    Warn,
    Success,
    Muted,
}

/// Бейджи у названия локации в шапке: всегда либо `pvp` (warn), либо
/// `safe` (success) — по флагу локации; прочие флаги — muted-бейджи с их
/// сырым текстом. Токены локализует рендер ([`badge_label`]).
/// Чистая функция — юнит-тест в конце файла.
pub(crate) fn location_badges(flags: &[String]) -> Vec<(BadgeTone, String)> {
    let pvp = flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"));
    let mut out = vec![if pvp {
        (BadgeTone::Warn, "pvp".to_string())
    } else {
        (BadgeTone::Success, "safe".to_string())
    }];
    for flag in flags {
        let known = flag.eq_ignore_ascii_case("pvp") || flag.eq_ignore_ascii_case("safe");
        if !known {
            out.push((BadgeTone::Muted, flag.clone()));
        }
    }
    out
}

/// M14/F16: охраняемая зона — нет PvP-флага (вне мира риска нет → true).
/// Чистая функция — юнит-тест в конце файла.
pub(crate) fn guarded_zone(flags: &[String]) -> bool {
    !flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"))
}

/// Подпись бейджа: известные токены → ключи l10n, прочее — как есть.
pub(crate) fn badge_label(lang: Lang, token: &str) -> String {
    match token {
        "pvp" => t(lang, Key::BadgePvp).to_string(),
        "safe" => t(lang, Key::BadgeSafe).to_string(),
        other => other.to_string(),
    }
}

/// Плашка в шапке: рамка и текст в тоне, фон — цвет фона окна.
pub(crate) fn badge(label: String, color: u32, pal: &Palette) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(color))
        .bg(rgb(pal.bg))
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(color))
        .child(label)
}

/// Бейдж статуса сервера в шапке (M14/F1): цветная точка + текст + RTT
/// (раньше жил в нижнем status_bar, из render убран).
pub(crate) fn status_badge(
    pal: &Palette,
    lang: Lang,
    status: &TransportStatus,
    rtt: Option<u64>,
) -> Div {
    let (color, _) = status_color(pal, status);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(color)))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.text))
                .child(status_text(lang, status)),
        )
        .children(rtt.map(|ms| {
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!("{ms} мс"))
        }))
}
