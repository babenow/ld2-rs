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

use ely::primitives::DragGhost;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Stateful, Window};
use ld2_core::l10n::{t, Key};
use ld2_core::store::{Act, Screen};

use crate::palette::Palette;

use super::{act_btn, scrollable_tab, sheet::quality_color, Click, PopupKind, Root};

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

        // ── левая колонка: лут + блок «Внутри» ─────────────────────
        let mut left = div().flex().flex_col().gap_1();
        left = left.child(col_title(t(lang, Key::LootLabel).to_string(), pal));
        if loot.is_empty() {
            left = left.child(
                div()
                    .px_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::PickupEmpty)),
            );
        }
        for item in &loot {
            left = left.child(self.loot_row(cx, pal, item));
        }
        if !inner.is_empty() {
            left = left.child(col_title(t(lang, Key::InnerLabel).to_string(), pal));
            for item in &inner {
                left = left.child(self.loot_row(cx, pal, item));
            }
        }
        let left_scroll = scrollable_tab("ld2-pickup-loot", &self.pickup_scroll, left);

        // ── правая колонка: рюкзак-виджет + кнопка «Забрать» ───────
        // Выбор живёт, только пока вещь есть в луте (после взятия —
        // строка исчезает, кнопка гаснет).
        let sel = self.pickup_sel.clone().filter(|uid| {
            loot.iter()
                .chain(inner.iter())
                .any(|i| i["uid"].as_str() == Some(uid.as_str()))
        });
        let sel_name = sel
            .as_ref()
            .and_then(|uid| {
                loot.iter()
                    .chain(inner.iter())
                    .find(|i| i["uid"].as_str() == Some(uid.as_str()))
                    .and_then(|i| i["name"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();

        let grid = self.backpack_grid(cx, pal, "ld2-pickup");
        // Зона-приёмник: перетаскивание вещи сюда = взять (`item.use`).
        // Замыкания on_drag/on_drop требуют 'static — цвет копируем.
        let accent = pal.accent;
        let drop_this = cx.entity();
        let drop_target = div()
            .id("ld2-pickup-drop")
            .flex()
            .flex_col()
            .flex_grow_1()
            .min_h_0()
            .drag_over::<LootDrag>(move |style, _, _, _| style.border_color(rgb(accent)))
            .on_drop(move |drag: &LootDrag, _window, acx| {
                let uid = drag.0.clone();
                drop_this.update(acx, |root, cx| root.pickup_take(uid, cx));
            })
            .child(scrollable_tab(
                "ld2-pickup-bp",
                &self.pickup_bp_scroll,
                grid,
            ));

        // Кнопка «Забрать» справа (указание заказчика): берёт выбранную
        // строку; без выбора — неактивная заглушка.
        let take = match sel.clone() {
            Some(uid) => act_btn(
                cx,
                t(lang, Key::TakeLoot),
                pal.accent,
                0xffffff,
                move |root, cx| root.pickup_take(uid.clone(), cx),
            ),
            None => div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .border_1()
                .border_color(rgb(pal.border))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::TakeLoot)),
        };
        let right = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_w_0()
            .min_h_0()
            .child(col_title(t(lang, Key::BackpackTitle).to_string(), pal))
            .child(drop_target)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .px_1()
                    .py_1()
                    .child(
                        div()
                            .flex_grow_1()
                            .min_w_0()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(sel_name),
                    )
                    .child(take),
            );

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
            .child(div().flex().flex_col().w(px(340.0)).min_h_0().child(right));

        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::LootPickup,
            t(lang, Key::PickupTitle),
            None,
            body,
            None,
        ))
    }

    /// Строка лута в левой колонке: ЛКМ — выбор (кнопка «Забрать»
    /// справа), двойной клик — взять сразу, drag — в рюкзак.
    fn loot_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        item: &serde_json::Value,
    ) -> Stateful<Div> {
        let uid = item["uid"].as_str().unwrap_or_default().to_string();
        let name = item["name"].as_str().unwrap_or("?").to_string();
        let itype = item["type"].as_str().unwrap_or("?");
        let color = quality_color(item["def_id"].as_str())
            .map(rgb)
            .unwrap_or_else(|| rgb(pal.text));
        let selected = self.pickup_sel.as_deref() == Some(uid.as_str());
        let ghost_title: gpui::SharedString = name.clone().into();
        div()
            .id(format!("ld2-pickup-row-{uid}"))
            .flex()
            .flex_row()
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(if selected { pal.selection } else { pal.panel }))
            .border_1()
            .border_color(rgb(pal.border))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(pal.selection)))
            .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
            .child(div().text_color(color).child(format!("• {name} ({itype})")))
            .child(div().flex_grow_1())
            .on_click(cx.listener({
                // on_drag ниже забирает uid в payload — клик с копией.
                let uid = uid.clone();
                move |root, event: &gpui::ClickEvent, _, cx| {
                    // После drag gpui подавляет клик; guard — на всякий случай.
                    if cx.has_active_drag() {
                        return;
                    }
                    if event.click_count() >= 2 {
                        root.pickup_take(uid.clone(), cx);
                    } else {
                        root.pickup_sel = Some(uid.clone());
                        cx.notify();
                    }
                }
            }))
            .on_drag(LootDrag(uid), move |_, _, _, cx| {
                DragGhost::new(ghost_title.clone(), None, cx)
            })
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

/// Заголовок колонки окна подятия.
fn col_title(text: String, pal: &Palette) -> Div {
    div()
        .font_weight(gpui::FontWeight::BOLD)
        .text_size(px(13.0))
        .text_color(rgb(pal.accent))
        .child(text)
}
