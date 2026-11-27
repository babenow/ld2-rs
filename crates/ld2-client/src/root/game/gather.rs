//! M39: сбор ресурсов — прогноз опыта по уровню, кнопка «Собрать» в
//! строке жилы и пункт контекстного меню (жирный/тонкий/цвет; иконки
//! опыта — как у снятия шкур).

use ld2_core::domain;
use ld2_core::l10n::Lang;

use super::*;

/// Гейт уровня жилы — как у снятия шкур: не хватает 25+ → сбор закрыт.
pub(super) const HARVEST_LEVEL_GAP: i64 = 25;
/// Опыт: `min(40, round((10 + required × 1.5) × множитель))`.
pub(super) const HARVEST_XP_BASE: f64 = 10.0;
pub(super) const HARVEST_XP_PER_LEVEL: f64 = 1.5;
pub(super) const HARVEST_DIFF_LOW: i64 = -10;
pub(super) const HARVEST_DIFF_MID: i64 = 9;
pub(super) const HARVEST_DIFF_HIGH: i64 = 24;
pub(super) const HARVEST_MULT_LOW: f64 = 1.5;
pub(super) const HARVEST_MULT_NORMAL: f64 = 1.0;
pub(super) const HARVEST_MULT_HIGH: f64 = 0.5;
pub(super) const HARVEST_MULT_ZERO: f64 = 0.0;
pub(super) const HARVEST_XP_MAX: i64 = 40;

/// Прогноз опыта за сбор: `(xp, tier, blocked)`, где tier 1 — повышенный
/// (уровень ниже жилы на 10+), 0 — обычный, -1 — пониженный (выше на
/// 10+), -2 — опыт не начисляется (25+); `blocked` — гейт уровня 25.
/// `None` — не горное дело (у прочих ресурсов опыт от уровня не зависит).
pub(super) fn xp_preview(store: &AppStore, entity: &Entity) -> Option<(i64, i8, bool)> {
    if entity.extra.get("profession").and_then(|v| v.as_str()) != Some(domain::PROFESSION_MINING) {
        return None;
    }
    let required = entity
        .extra
        .get("required_level")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1);
    // Не изучено — подсказки нет (клик покажет E_PROFESSION).
    let prof = store.snapshot.as_ref().and_then(|s| {
        s.char
            .professions
            .iter()
            .find(|p| p.id == domain::PROFESSION_MINING)
    })?;
    let level = i64::from(prof.level);
    let min_rank = entity
        .extra
        .get("min_rank")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    let diff = level - required;
    let (mult, tier) = if diff <= HARVEST_DIFF_LOW {
        (HARVEST_MULT_LOW, 1)
    } else if diff <= HARVEST_DIFF_MID {
        (HARVEST_MULT_NORMAL, 0)
    } else if diff <= HARVEST_DIFF_HIGH {
        (HARVEST_MULT_HIGH, -1)
    } else {
        (HARVEST_MULT_ZERO, -2)
    };
    let base = HARVEST_XP_BASE + required as f64 * HARVEST_XP_PER_LEVEL;
    let xp = ((base * mult).round() as i64).min(HARVEST_XP_MAX);
    let blocked = i64::from(prof.rank) < min_rank || required - level >= HARVEST_LEVEL_GAP;
    Some((xp, tier, blocked))
}

/// Кнопка «Собрать» в строке ресурса: жирная/зелёная при повышенном
/// опыте, приглушённая при пониженном/нулевом, замок при гейте уровня.
pub(super) fn button(
    cx: &mut Context<'_, Root>,
    lang: Lang,
    pal: &Palette,
    id: String,
    preview: Option<(i64, i8, bool)>,
) -> Stateful<Div> {
    let blocked = preview.is_some_and(|(_, _, blocked)| blocked);
    let (weight, color, xp_hint) = match preview {
        Some((_, _, true)) => (gpui::FontWeight::NORMAL, pal.muted, None),
        Some((xp, 1, false)) => (
            gpui::FontWeight::BOLD,
            pal.success,
            (xp > 0).then(|| format!("+{xp}")),
        ),
        Some((xp, tier, false)) => (
            gpui::FontWeight::NORMAL,
            if tier < 0 { pal.muted } else { pal.text },
            (xp > 0).then(|| format!("+{xp}")),
        ),
        None => (gpui::FontWeight::NORMAL, pal.text, None),
    };
    let mut button = div()
        .id(format!("ld2-gather-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.bg))
        .font_weight(weight)
        .text_color(rgb(color))
        .child(t(lang, Key::GatherBtn).to_string());
    if blocked {
        button = button.child(fa_icon("solid/lock", px(11.0), rgb(pal.muted)));
    } else {
        let gather_id = id.clone();
        button = button
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(pal.selection)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |root, _, _, cx| {
                    root.on_click(Click::Gather(gather_id.clone()), cx);
                    cx.notify();
                }),
            );
    }
    if let Some(xp) = xp_hint {
        button = button.child(
            div()
                .text_size(px(crate::ui_tokens::FONT_XS))
                .text_color(rgb(color))
                .child(xp),
        );
    }
    button
}

/// Иконка ожидаемого опыта — общая для «Снять шкуру» и «Собрать»:
/// Lock — гейт уровня, EyeOff — 0 XP, Star/Sparkles/Zap по величине.
pub(super) fn xp_icon(xp: i64, blocked: bool) -> IconName {
    if blocked {
        IconName::Lock
    } else if xp <= 0 {
        IconName::EyeOff
    } else if xp < 15 {
        IconName::Star
    } else if xp < 30 {
        IconName::Sparkles
    } else {
        IconName::Zap
    }
}

/// Пункт меню «Собрать» с иконкой ожидаемого опыта (как у снятия шкур):
/// Lock — гейт уровня (пункт отключается вызывающим), EyeOff — 0 XP,
/// Star/Sparkles/Zap — по величине.
pub(super) fn menu_item(lang: Lang, preview: Option<(i64, i8, bool)>) -> (MenuItem, bool) {
    let item = MenuItem::new(t(lang, Key::GatherBtn).to_string());
    let Some((xp, _, blocked)) = preview else {
        return (item, false);
    };
    (item.icon(xp_icon(xp, blocked)), blocked)
}
