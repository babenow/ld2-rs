//! M23: **окно подятия предмета** — popup-диалог по ЛКМ на труп в
//! списке сущностей (ПКМ остаётся контекстным меню). Левая колонка —
//! лут трупа + отдельный блок «Внутри» (мясо/потроха от освежевания);
//! правая — **рюкзак-виджет** (`Root::backpack_grid`, общий с окном
//! рюкзака, свои id). Вещь берётся тремя способами (указание
//! заказчика): перетаскиванием в рюкзак, двойным кликом по строке и
//! кнопкой «Забрать» справа (для трупа игрока — через подтверждение
//! статуса ФР-11, как в модалке осмотра).
//!
//! Данные — отдельный `cmd/entity.info` → `Act::PickupInfo` →
//! `store.pickup` (модалка осмотра не перетирается); после взятия
//! лут перезапрашивается из `tick` (см. `Root::pump_pickup`).
//! Колонки окна (лут, рюкзак, «Забрать») — `pickup/columns.rs`.

use ld2_core::domain;

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, MouseButton, SharedString, Window};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::{Act, Screen};

use crate::fa::fa_icon;
use crate::palette::Palette;

use super::{btn, Click, PopupKind, Root};

use std::cell::RefCell;

mod columns;

/// M23: payload перетаскивания вещи из лута в рюкзак — отдельный
/// newtype (не `String`), чтобы не конфликтовать с перетаскиванием
/// слотов внутри `backpack_grid` (Reorder использует свой тип).
#[derive(Debug, Clone)]
struct LootDrag(String);

impl Root {
    /// Окно подятия: гейт — флаг `pickup_popup` + данные в
    /// `store.pickup`; без данных (первый запрос) — «Загрузка…»,
    /// ушедшие данные (смена локации/ошибка) — окно закрывается само.
    pub(super) fn pickup_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.pickup_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        let lang = self.lang;
        let Some(detail) = self.store.pickup.clone() else {
            // Данных нет: либо ждём ответ, либо они ушли навсегда.
            if !self.pickup_request && self.store.pending != Some(Act::PickupInfo) {
                self.close_pickup();
                return None;
            }
            let body = div().flex().flex_col().gap_2().child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
            return Some(self.popup_window(
                window,
                cx,
                pal,
                PopupKind::LootPickup,
                t(lang, Key::PickupTitle),
                None,
                body,
                None,
            ));
        };

        let loot = detail["loot"].as_array().cloned().unwrap_or_default();
        let inner = detail["inner"].as_array().cloned().unwrap_or_default();

        let left_scroll = self.pickup_left(cx, &loot, &inner, lang, pal);
        let (right, sel_name, take) = self.pickup_right(cx, pal, lang, &loot, &inner);

        // ── статус-бар окна (указание заказчика): счётчик предметов,
        // выбранная вещь, кнопки «Забрать» (выбранное) и «Взять всё»
        // (партия в очередь — см. `Root::pump_take_queue`).
        let items_count = loot.len() + inner.len();
        let taking_all = !self.pickup_queue.is_empty();
        let take_all = if items_count == 0 || taking_all {
            disabled_btn(t(lang, Key::TakeAllBtn), pal)
        } else {
            btn(
                cx,
                t(lang, Key::TakeAllBtn),
                pal.accent,
                0xffffff,
                Click::LootTakeAll,
            )
        };
        // M24: кнопки разделки в статус-баре — когда изучена
        // профессия снятия шкур и труп разделываемый (флаги как в
        // контекстном меню; сделанные действия скрыты).
        let entity = &detail["entity"];
        let entity_id = entity["id"].as_str().unwrap_or_default().to_string();
        let harvest = entity["harvest"].as_object();
        let learned = self.store.snapshot.as_ref().is_some_and(|s| {
            s.char
                .professions
                .iter()
                .any(|p| p.id == domain::PROFESSION_SKINNING)
        });
        let flag = |h: &serde_json::Map<String, serde_json::Value>, key: &str| {
            h.get(key).and_then(|v| v.as_bool()) == Some(true)
        };
        let skin_ready = learned
            && harvest.is_some_and(|h| flag(h, domain::ACTION_SKIN) && !flag(h, "skin_done"));
        let gut_ready =
            learned && harvest.is_some_and(|h| flag(h, domain::ACTION_GUT) && !flag(h, "gut_done"));
        let mut harvest_btns = div().flex().flex_row().gap_1();
        // M24: глифы выбраны заказчиком — FA scissors (снятие шкур) и
        // FA drumstick-bite (освежевание, «мясо» — результат).
        let skin_id = entity_id.clone();
        if skin_ready {
            harvest_btns = harvest_btns.child(icon_act_btn(
                cx,
                "solid/scissors",
                t(lang, Key::SkinBtn),
                pal.warn,
                0xffffff,
                move |root, cx| {
                    root.on_click(Click::Harvest(skin_id, domain::ACTION_SKIN.into()), cx);
                },
            ));
        }
        if gut_ready {
            harvest_btns = harvest_btns.child(icon_act_btn(
                cx,
                "solid/drumstick-bite",
                t(lang, Key::GutBtn),
                pal.warn,
                0xffffff,
                move |root, cx| {
                    root.on_click(Click::Harvest(entity_id, domain::ACTION_GUT.into()), cx);
                },
            ));
        }

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
                    .text_color(rgb(pal.muted))
                    .child(tf(
                        lang,
                        Key::PickupCount,
                        &[("{n}", &items_count.to_string())],
                    )),
            )
            .child(
                div()
                    .flex_grow_1()
                    .min_w_0()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(sel_name),
            )
            .child(harvest_btns)
            .child(take)
            .child(take_all);

        let body = div()
            .flex()
            .flex_row()
            .gap_3()
            .flex_grow_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_grow_1()
                    .min_w_0()
                    .min_h_0()
                    .child(left_scroll),
            )
            .child(div().w(px(1.0)).bg(rgb(pal.border)))
            .child(div().flex().flex_col().w(px(440.0)).min_h_0().child(right));

        // M24: заголовок — имя трупа (указание заказчика); пока
        // данных нет — прежний текст (ветка загрузки).
        let title = detail["entity"]["name"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| t(lang, Key::PickupTitle).to_string());
        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::LootPickup,
            title,
            None,
            body,
            Some(footer),
        ))
    }

    /// M23: взять вещь из окна подятия. Труп игрока — подтверждение
    /// статуса (ФР-11, как в осмотре); иначе обычное `cmd/item.use`
    /// (сервер распознаёт `loot:`/`inner:` → `take_loot`).
    pub(super) fn pickup_take(&mut self, uid: String, cx: &mut Context<'_, Self>) {
        let detail = self.store.pickup.clone();
        let was_player = detail.as_ref().is_some_and(|d| d["was_player"] == true);
        if was_player {
            let name = detail
                .as_ref()
                .and_then(|d| {
                    d["loot"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .chain(d["inner"].as_array().into_iter().flatten())
                        .find(|i| i["uid"].as_str() == Some(uid.as_str()))
                        .and_then(|i| i["name"].as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            self.store.set_loot_confirm(uid, name);
        } else {
            self.on_click(Click::UseItem(uid), cx);
        }
    }
}

/// M23: uid-ы всех вещей трупа (общий лут + блок «Внутри») в порядке
/// выдачи — очередь кнопки «Взять всё».
pub(super) fn pickup_uids(detail: &serde_json::Value) -> Vec<String> {
    detail["loot"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(detail["inner"].as_array().into_iter().flatten())
        .filter_map(|i| i["uid"].as_str().map(str::to_string))
        .collect()
}

/// Неактивная кнопка статус-бара (нет выбора/уже берём партию).
fn disabled_btn(text: &'static str, pal: &Palette) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border))
        .text_color(rgb(pal.muted))
        .child(text)
}

/// M24: кнопка действия с глифом Font Awesome слева — как `act_btn`,
/// но с иконкой (глифы разделки: scissors / drumstick-bite).
fn icon_act_btn(
    cx: &mut Context<'_, Root>,
    icon: &'static str,
    label: impl Into<SharedString>,
    bg: u32,
    fg: u32,
    f: impl FnOnce(&mut Root, &mut Context<'_, Root>) + 'static,
) -> Div {
    let cell = RefCell::new(Some(f));
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .hover(move |s| s.bg(rgb(bg)))
        .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
        .child(fa_icon(icon, px(14.0), rgb(fg)))
        .child(label.into())
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if let Some(f) = cell.borrow_mut().take() {
                    f(this, cx);
                }
                cx.notify();
            }),
        )
}

/// Заголовок колонки окна подятия.
fn col_title(text: String, pal: &Palette) -> Div {
    div()
        .font_weight(gpui::FontWeight::BOLD)
        .text_size(px(13.0))
        .text_color(rgb(pal.accent))
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M23: «Взять всё» собирает uid-ы общего лута и блока «Внутри»
    /// в порядке выдачи.
    #[test]
    fn pickup_uids_collects_loot_and_inner() {
        let detail = serde_json::json!({
            "loot": [
                {"uid": "loot:c:0", "name": "Шкура"},
                {"uid": "loot:c:1", "name": "Коготь"}
            ],
            "inner": [{"uid": "inner:c:0", "name": "Мясо"}]
        });
        assert_eq!(
            pickup_uids(&detail),
            vec!["loot:c:0", "loot:c:1", "inner:c:0"],
            "общий лут, затем «Внутри»"
        );
        let empty = serde_json::json!({"loot": [], "inner": []});
        assert!(pickup_uids(&empty).is_empty(), "пусто — пустая очередь");
        assert!(
            pickup_uids(&serde_json::json!({})).is_empty(),
            "без полей — пустая очередь"
        );
    }
}
