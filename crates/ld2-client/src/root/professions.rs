//! M21: **окно профессий** — отдельное (кнопка в шапке + хоткей «p»,
//! подобно «Квестам»/«Рюкзаку»; вкладка из окна персонажа убрана —
//! правило дубликатов). Показываются **только изученные** профессии:
//! основные — по одной крупной карточке в строке (лимит primary из
//! `SessionConfig.profession_limit`), ниже — дополнительные одной
//! строкой. Карточка: название, название ранга (Новичок…Мастер) и
//! **две шкалы** — уровень (XP-порог растёт: `40 × 1.02^L`) и прогресс
//! границы ранга (кратно 50 уровней); кнопки «Повысить ранг» (у
//! тренера, цена из `extra.rank_up_gold`) и «Сбросить» (двухшаговое
//! подтверждение). Внизу — demo-обучение и список рецептов (переехали
//! из окна персонажа).

use ely::forms::{Choice, Select};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Window};
use ld2_core::l10n::{t, tf, Key, Lang};
use ld2_core::store::{Act, Screen};
use ld2_protocol::cmd::{CraftLearn, CraftRankUp, CraftRecipes, CraftReset, CraftStart};
use ld2_protocol::types::ProfessionSnapshot;

use crate::palette::Palette;

use super::{act_btn, scrollable_tab, sheet::bar, PopupKind, Root};

/// M21: дополнительные профессии (лимит primary на них не действует);
/// всё остальное изученное — основная (в т.ч. новые id контента).
const SECONDARY: [&str; 3] = ["cook", "fishing", "medic"];

/// Демо-каталог профессий для обучения (skills.md §5; сервер
/// валидирует тренером и лимитом primary).
const LEARN_CATALOG: [(&str, Key); 14] = [
    ("herbalist", Key::ProfHerbalist),
    ("alchemist", Key::ProfAlchemist),
    ("mining", Key::ProfMining),
    ("blacksmith", Key::ProfBlacksmith),
    ("skinning", Key::ProfSkinning),
    ("leatherworking", Key::ProfLeatherworking),
    ("tailor", Key::ProfTailor),
    ("scribe", Key::ProfScribe),
    ("woodcutting", Key::ProfWoodcutting),
    ("woodcarver", Key::ProfWoodcarver),
    ("jeweler", Key::ProfJeweler),
    ("cook", Key::ProfCook),
    ("fishing", Key::ProfFishing),
    ("medic", Key::ProfMedic),
];

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

    let scale = |name: String, frac: f32, nums: String, fill: u32| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(name)
                    .child(nums),
            )
            .child(bar(frac, String::new(), fill, pal))
    };

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
    // Шкала1: уровень (XP-порог растёт; на потолке — без дроби).
    let level_nums = if prof.xp_to_next > 0 {
        format!("{}/{}", prof.xp, prof.xp_to_next)
    } else {
        prof.level.to_string()
    };
    col = col.child(scale(
        format!("{} {}", t(lang, Key::ProfLevelLabel), prof.level),
        level_frac,
        level_nums,
        pal.accent,
    ));
    // Шкала2: прогресс до следующего ранга (уровни внутри границы).
    col = col.child(scale(
        t(lang, rank_key(rank)).to_string(),
        band_frac,
        format!("{in_band}/{span}"),
        pal.success,
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

impl Root {
    /// M21: окно профессий (гейт — `professions_popup.open`).
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
        // Автозагрузка рецептов — один раз (как во вкладке раньше).
        if self.store.snapshot.is_some() && !self.store.recipes_loaded {
            self.store.recipes_loaded = true;
            self.send_cmd(&CraftRecipes { profession: None }, Act::Craft);
        }
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

        // Только изученные: основные строками, доп — одной строкой.
        let mut primary: Vec<&ProfessionSnapshot> = Vec::new();
        let mut secondary: Vec<&ProfessionSnapshot> = Vec::new();
        for prof in &snap.char.professions {
            if SECONDARY.contains(&prof.id.as_str()) {
                secondary.push(prof);
            } else {
                primary.push(prof);
            }
        }
        let reset_armed = self.prof_reset_armed.clone();
        let mut col = col_gap();
        if snap.char.professions.is_empty() {
            col = col.child(
                div()
                    .px_3()
                    .py_2()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ProfessionsNone)),
            );
        } else {
            if !primary.is_empty() {
                col = col.child(section_title(t(lang, Key::ProfessionsPrimary)));
                for prof in &primary {
                    col = col.child(prof_card(cx, pal, lang, prof, reset_armed.as_deref()));
                }
            }
            if !secondary.is_empty() {
                col = col.child(section_title(t(lang, Key::ProfessionsSecondary)));
                let mut row = div().flex().flex_row().gap_2().flex_wrap();
                for prof in &secondary {
                    row = row.child(prof_card(cx, pal, lang, prof, reset_armed.as_deref()));
                }
                col = col.child(row);
            }
        }
        let primary_count = primary.len();

        // Демо-обучение (Select + подтверждение — как во вкладке).
        let available: Vec<(&'static str, Key)> = LEARN_CATALOG
            .iter()
            .copied()
            .filter(|(id, _)| !snap.char.professions.iter().any(|p| p.id == *id))
            .collect();
        if !available.is_empty() {
            let choices: Vec<Choice> = available
                .iter()
                .map(|(id, key)| Choice::new(*id, t(lang, *key)))
                .collect();
            let pick = self.learn_pick.clone();
            let this = cx.entity();
            let mut row = div().flex().flex_row().gap_2().items_center();
            row = row.child(div().flex_grow_1().child({
                let this = this.clone();
                let sel =
                    Select::new("ld2-learn-prof", choices).placeholder(t(lang, Key::LearnLabel));
                match pick {
                    Some(value) => sel.selected(value),
                    None => sel,
                }
                .on_change(move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, _cx| root.learn_pick = Some(value));
                })
            }));
            if let Some(id) = self.learn_pick.clone() {
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::LearnLabel).to_string(),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = CraftLearn {
                            profession_id: id.clone(),
                        };
                        this.send_cmd(&cmd, Act::Learn);
                        let _ = cx;
                    },
                ));
            }
            col = col.child(row);
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));

        // Рецепты (переехали из окна персонажа; требования/ингредиенты).
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::RecipesLabel)),
        );
        if self.store.recipes.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoRecipes)),
            );
        }
        let recipes = self.store.recipes.clone();
        for recipe in &recipes {
            let recipe_id = recipe["id"].as_str().unwrap_or_default().to_string();
            let name = recipe["name"].as_str().unwrap_or("?").to_string();
            let profession = recipe["profession"].as_str().unwrap_or("").to_string();
            let rank = recipe["rank"].as_i64().unwrap_or(0);
            let station = recipe["station_label"].as_str().unwrap_or("").to_string();

            let mut ingredients_ok = true;
            let mut ingredient_lines: Vec<(String, bool)> = Vec::new();
            if let Some(inputs) = recipe["inputs"].as_array() {
                for input in inputs {
                    let need_type = input["type"].as_str().unwrap_or_default();
                    let need_qty = input["qty"].as_i64().unwrap_or(1);
                    let have: i64 = snap
                        .char
                        .inventory
                        .iter()
                        .filter(|i| i.item_type == need_type)
                        .map(|i| i.qty.unwrap_or(1))
                        .sum();
                    let ok = have >= need_qty;
                    if !ok {
                        ingredients_ok = false;
                    }
                    ingredient_lines.push((format!("{need_type}: {have}/{need_qty}"), ok));
                }
            }

            let mut card = div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .border_1()
                .border_color(rgb(pal.border))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(div().child(name.clone()))
                        .child(div().text_size(px(12.0)).text_color(rgb(pal.muted)).child(
                            format!(
                                "· {} · {}",
                                prof_label(lang, &profession),
                                t(lang, Key::RankFmt).replace("{r}", &rank.to_string())
                            ),
                        )),
                );
            if !station.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(station),
                );
            }

            // Крафт: кнопка активна при полном наборе и вне ожидания (§7).
            let pending = self.store.pending == Some(Act::Craft);
            if ingredients_ok && !pending {
                card = card.child(act_btn(
                    cx,
                    t(lang, Key::BtnCraft).to_string(),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = CraftStart {
                            recipe_id: recipe_id.clone(),
                            qty: 1,
                        };
                        this.send_cmd(&cmd, Act::Craft);
                        let _ = cx;
                    },
                ));
            }
            for (line, ok) in ingredient_lines {
                card = card.child(if ok {
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {line}"))
                } else {
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.danger))
                        .child(format!("• {}: {}", t(lang, Key::MissingIngredients), line))
                });
            }
            col = col.child(card);
        }

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
