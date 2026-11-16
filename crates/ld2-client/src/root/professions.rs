//! M21: **окно профессий** — отдельное (кнопка в шапке + хоткей «p»,
//! подобно «Квестам»/«Рюкзаку»; вкладка из окна персонажа убрана —
//! правило дубликатов). Показываются **только изученные** профессии:
//! основные — по одной крупной карточке в строке (лимит primary из
//! `SessionConfig.profession_limit`), ниже — дополнительные одной
//! строкой. Карточка: название, название ранга (Новичок…Мастер) и
//! **две шкалы** — уровень (XP-порог растёт: `40 × 1.02^L`) и прогресс
//! границы ранга (кратно 50 уровней); кнопки «Повысить ранг» (у
//! тренера, цена из `extra.rank_up_gold`) и «Сбросить» (двухшаговое
//! подтверждение). Ниже основных — секция дополнительных (только
//! изученные; изучаются параллельно, лимит primary не трогают).
//! По указанию заказчика из окна убраны: список рецептов, выпадающий
//! список обучения и подписи «Уровень x»/надпись ранга под полосами
//! (обучение — у тренера в диалоге).

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Window};
use ld2_core::l10n::{t, tf, Key, Lang};
use ld2_core::store::{Act, Screen};
use ld2_protocol::cmd::{CraftRankUp, CraftReset};
use ld2_protocol::types::ProfessionSnapshot;

use crate::palette::Palette;

use super::{act_btn, scrollable_tab, sheet::bar, PopupKind, Root};

/// M21: основные профессии (окно: по одной карточке в строке; у
/// неизученных — пустая карточка, указание заказчика).
const PRIMARY: [&str; 11] = [
    "herbalist",
    "alchemist",
    "mining",
    "blacksmith",
    "skinning",
    "leatherworking",
    "tailor",
    "scribe",
    "woodcutting",
    "woodcarver",
    "jeweler",
];

/// M21: дополнительные профессии (изучаются параллельно, лимит
/// primary не действуют) — в одну строку под основными.
const SECONDARY: [&str; 3] = ["cook", "fishing", "medic"];

/// Название профессии: l10n-ключ по id, фолбэк — сам id.
fn prof_label(lang: Lang, id: &str) -> String {
    let key = match id {
        "herbalist" => Some(Key::ProfHerbalist),
        "alchemist" => Some(Key::ProfAlchemist),
        "mining" => Some(Key::ProfMining),
        "blacksmith" => Some(Key::ProfBlacksmith),
        "skinning" => Some(Key::ProfSkinning),
        "leatherworking" => Some(Key::ProfLeatherworking),
        "tailor" => Some(Key::ProfTailor),
        "scribe" => Some(Key::ProfScribe),
        "woodcutting" => Some(Key::ProfWoodcutting),
        "woodcarver" => Some(Key::ProfWoodcarver),
        "jeweler" => Some(Key::ProfJeweler),
        "cook" => Some(Key::ProfCook),
        "fishing" => Some(Key::ProfFishing),
        "medic" => Some(Key::ProfMedic),
        _ => None,
    };
    key.map(|k| t(lang, k).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Название ранга (1…5): Новичок … Мастер (контент-границы по 50
/// уровней — M21).
fn rank_key(rank: u32) -> Key {
    match rank {
        0 | 1 => Key::ProfRank1,
        2 => Key::ProfRank2,
        3 => Key::ProfRank3,
        4 => Key::ProfRank4,
        _ => Key::ProfRank5,
    }
}

/// Карточка изученной профессии: заголовок (название + ранг), две
/// шкалы (уровень и граница ранга), кнопки ранга/сброса.
fn prof_card(
    cx: &mut Context<'_, Root>,
    pal: &Palette,
    lang: Lang,
    prof: &ProfessionSnapshot,
    reset_armed: Option<&str>,
) -> Div {
    let span = prof
        .extra
        .get("rank_span")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as u32;
    let rank = prof.rank.max(1);
    let band_start = (rank - 1).saturating_mul(span);
    let in_band = prof.level.saturating_sub(band_start).min(span);
    let level_frac = if prof.xp_to_next > 0 {
        prof.xp as f32 / prof.xp_to_next as f32
    } else {
        1.0
    };
    let band_frac = f32::from(in_band as u16) / f32::from(span.max(1) as u16);

    let mut col = div()
        .flex()
        .flex_col()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border));
    // Шапка: название + название ранга.
    col = col.child(
        div()
            .flex()
            .flex_row()
            .gap_2()
            .items_center()
            .justify_between()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(prof_label(lang, &prof.id)),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.accent))
                    .child(t(lang, rank_key(rank))),
            ),
    );
    // Две шкалы без текстовых подписей (указание заказчика):
    // числа идут рядом с полосами — уровень (XP) и граница ранга.
    let level_nums = if prof.xp_to_next > 0 {
        format!("{}/{}", prof.xp, prof.xp_to_next)
    } else {
        prof.level.to_string()
    };
    col = col.child(bar(level_frac, level_nums, pal.accent, pal));
    col = col.child(bar(
        band_frac,
        format!("{in_band}/{span}"),
        pal.success,
        pal,
    ));
    // Действия: повышение ранга (цена из extra) + сброс.
    let mut btns = div().flex().flex_row().gap_2().items_center().flex_wrap();
    if let Some(cost) = prof.extra.get("rank_up_gold").and_then(|v| v.as_i64()) {
        let need_level = rank.saturating_mul(span);
        if prof.level >= need_level {
            let pid = prof.id.clone();
            btns = btns.child(act_btn(
                cx,
                tf(lang, Key::ProfRankUpBtn, &[("{n}", &cost.to_string())]),
                pal.accent,
                pal.accent_text,
                move |root, cx| {
                    root.send_cmd(
                        &CraftRankUp {
                            profession_id: pid.clone(),
                        },
                        Act::CraftRankUp,
                    );
                    let _ = cx;
                },
            ));
        } else {
            btns = btns.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(tf(
                        lang,
                        Key::ProfRankUpHint,
                        &[("{lvl}", &need_level.to_string())],
                    )),
            );
        }
    }
    // Сброс: первый клик — подтверждение, второй — cmd/craft.reset.
    let pid = prof.id.clone();
    let armed = reset_armed == Some(prof.id.as_str());
    btns = btns.child(act_btn(
        cx,
        if armed {
            t(lang, Key::ResetProfConfirm).to_string()
        } else {
            t(lang, Key::ResetProf).to_string()
        },
        pal.panel,
        pal.danger,
        move |root, cx| {
            if root.prof_reset_armed.as_deref() == Some(pid.as_str()) {
                root.prof_reset_armed = None;
                root.send_cmd(
                    &CraftReset {
                        profession_id: pid.clone(),
                    },
                    Act::ProfessionReset,
                );
            } else {
                root.prof_reset_armed = Some(pid.clone());
            }
            cx.notify();
        },
    ));
    col = col.child(btns);
    col
}

/// Заголовок секции.
fn section_title(text: &str) -> Div {
    div()
        .font_weight(gpui::FontWeight::BOLD)
        .text_size(px(13.0))
        .child(text.to_string())
}

/// Пустая карточка неизученной профессии (только название,
/// приглушённое; указание заказчика — показывать все).
fn empty_card(pal: &Palette, lang: Lang, id: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .px_3()
        .py_3()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border))
        .child(div().text_color(rgb(pal.muted)).child(prof_label(lang, id)))
}

impl Root {
    /// M21: окно профессий (гейт — `professions_popup.open`):
    /// изученные основные строками, ниже — дополнительные.
    pub(super) fn professions_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.professions_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return Some(
                self.popup_window(
                    window,
                    cx,
                    pal,
                    PopupKind::Professions,
                    t(lang, Key::ProfessionsTab),
                    None,
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::Loading)),
                    None,
                ),
            );
        };

        // Все профессии: изученные — полная карточка, нет — пустая
        // (указание заказчика); основные строками, доп — в одну строку.
        let reset_armed = self.prof_reset_armed.clone();
        let mut col = col_gap();
        col = col.child(section_title(t(lang, Key::ProfessionsPrimary)));
        let mut primary_count = 0usize;
        for id in PRIMARY {
            if let Some(prof) = snap.char.professions.iter().find(|p| p.id == id) {
                primary_count += 1;
                col = col.child(prof_card(cx, pal, lang, prof, reset_armed.as_deref()));
            } else {
                col = col.child(empty_card(pal, lang, id));
            }
        }
        col = col.child(section_title(t(lang, Key::ProfessionsSecondary)));
        let mut row = div().flex().flex_row().gap_2();
        for id in SECONDARY {
            let card = match snap.char.professions.iter().find(|p| p.id == id) {
                Some(prof) => prof_card(cx, pal, lang, prof, reset_armed.as_deref()),
                None => empty_card(pal, lang, id),
            };
            row = row.child(div().flex_grow_1().min_w(px(240.0)).child(card));
        }
        col = col.child(row);

        let content = scrollable_tab("ld2-professions-scroll", &self.professions_scroll, col);

        // Статус-бар: основных изучено N / M (лимит §5).
        let max = self
            .store
            .session
            .as_ref()
            .map(|s| s.profession_limit)
            .unwrap_or(2);
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(rgb(pal.selection))
            .border_t_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.text))
                    .child(tf(
                        lang,
                        Key::ProfPrimaryLimit,
                        &[
                            ("{cur}", &primary_count.to_string()),
                            ("{max}", &max.to_string()),
                        ],
                    )),
            );

        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Professions,
            t(lang, Key::ProfessionsTab),
            None,
            content,
            Some(footer),
        ))
    }
}

/// Общий вертикальный контейнер секций.
fn col_gap() -> Div {
    div().flex().flex_col().gap_2().px_1().py_1()
}
