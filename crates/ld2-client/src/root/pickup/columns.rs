//! Колонки окна подятия (RF3n): левая — лут трупа и блок «Внутри»,
//! правая — рюкзак-виджет с вкладками и кнопкой «Забрать». Строка
//! лута (`loot_row`) общая для обеих частей левой колонки.

use ely::forms::Choice;
use ely::navigation::Tabs;
use ely::primitives::DragGhost;
use gpui::{div, prelude::*, px, rgb, Context, Div, Stateful};
use ld2_core::l10n::{t, Key};

use crate::palette::Palette;

use super::super::{act_btn, backpack::BackpackTab, scrollable_tab, sheet::quality_color, Root};
use super::{col_title, LootDrag};

impl Root {
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
        // M24: количество в стаке — справа (показываем при >1).
        let qty = item["qty"].as_i64().filter(|q| *q > 1);
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
            .children(qty.map(|q| {
                div()
                    .text_size(px(crate::ui_tokens::FONT_XS))
                    .text_color(rgb(pal.muted))
                    .child(format!("×{q}"))
            }))
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

    /// Левая колонка окна подятия: лут и блок «Внутри» (RF3n).
    pub(super) fn pickup_left(
        &mut self,
        cx: &mut Context<'_, Self>,
        loot: &[serde_json::Value],
        inner: &[serde_json::Value],
        lang: ld2_core::l10n::Lang,
        pal: &Palette,
    ) -> gpui::Div {
        // ── левая колонка: лут + блок «Внутри» ─────────────────────
        let mut left = div().flex().flex_col().gap_1();
        // M24: подпись колонки — «Предметы» (вместо «Лут»).
        left = left.child(col_title(t(lang, Key::AdminItemsSheet).to_string(), pal));
        if loot.is_empty() {
            left = left.child(
                div()
                    .px_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::PickupEmpty)),
            );
        }
        for item in loot {
            left = left.child(self.loot_row(cx, pal, item));
        }
        if !inner.is_empty() {
            left = left.child(col_title(t(lang, Key::InnerLabel).to_string(), pal));
            for item in inner {
                left = left.child(self.loot_row(cx, pal, item));
            }
        }
        scrollable_tab("ld2-pickup-loot", &self.pickup_scroll, left)
    }

    /// Правая колонка окна подятия: выбор, рюкзак, кнопка «Забрать»
    /// и зона-приёмник перетаскивания (RF3n).
    pub(super) fn pickup_right(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        lang: ld2_core::l10n::Lang,
        loot: &[serde_json::Value],
        inner: &[serde_json::Value],
    ) -> (gpui::Div, String, gpui::AnyElement) {
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

        // Вкладки-фильтры рюкзака (как в окне рюкзака): общий
        // `backpack_tab` — выбор живёт в конфиге и синхронен с главным
        // окном; свой id, чтобы не конфликтовать с открытым окном
        // рюкзака (уникальность id gpui в кадре).
        let tab = self.backpack_tab;
        let choices: Vec<Choice> = BackpackTab::all()
            .iter()
            .map(|(candidate, key)| {
                Choice::new(candidate.as_str(), t(lang, *key)).icon(candidate.icon())
            })
            .collect();
        let this = cx.entity();
        let tabs = Tabs::new("ld2-pickup-tabs", choices, tab.as_str()).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    root.backpack_tab = BackpackTab::parse(&value);
                    root.config.backpack_tab = value;
                    root.save_config();
                    cx.notify();
                });
            },
        );

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
            .child(tabs)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(drop_target);
        (right, sel_name, take.into_any_element())
    }
}
