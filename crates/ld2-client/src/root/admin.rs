//! M24: окно админ-панели — **листы по кнопкам в шапке** (первая
//! «Предметы» — просто иконка с тултипом), справа от заголовка,
//! рядом с крестиком закрытия — тумблер **«режим бога»** (иконка +
//! switch, только admin; слот `trailing` оболочки `popup_window`).
//!
//! Левая панель: поиск + группы-вкладки рюкзака + каталог в
//! **`uniform_list`** (виртуализация — вариант A: каталог одним
//! запросом кэшируется в сторе, фильтрация на клиенте). Правая —
//! цель выдачи (F24b: свой рюкзак): перетаскивание из каталога и
//! двойной клик — выдача, двойной клик по предмету цели — удаление;
//! оба действия открывают execute-диалог количества (`*.ask` →
//! `evt/dialog/open` → `cmd/dialog.submit`).

use ely::forms::{Choice, SearchInput, Switch};
use ely::navigation::Tabs;
use ely::primitives::{DragGhost, Icon, IconName, Tooltip};
use gpui::{
    div, prelude::*, px, rgb, uniform_list, AnyElement, Context, Div, Entity, Stateful, Window,
};
use ld2_core::l10n::{t, Key, Lang};
use ld2_core::store::Screen;
use ld2_protocol::types::ItemSnapshot;

use crate::fa::fa_icon;
use crate::palette::Palette;

use super::backpack::{tab_for_type, BackpackTab};
use super::sheet::{item_color, rarity_color, rarity_label};
use super::{scrollable_tab, Click, PopupKind, Root};

/// M24: payload перетаскивания предмета из каталога в цель — свой
/// newtype (не конфликтует с `LootDrag` окна подятия и Reorder
/// внутри `backpack_grid`).
#[derive(Debug, Clone)]
struct AdminDrag(String);

impl Root {
    /// Окно админ-панели: гейт — флаг `admin_popup` + роль `admin`
    /// (кнопка в шапке рисуется только админу; сервер гейтит все
    /// команды повторно — при смене роли окно закрывается).
    pub(super) fn admin_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.admin_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        if !self
            .store
            .session
            .as_ref()
            .is_some_and(|s| s.role == "admin")
        {
            self.close_admin();
            return None;
        }
        let lang = self.lang;
        let this = cx.entity();

        // ── шапка: лист «Предметы» — просто иконка (активен) ────
        let leading = div()
            .id("ld2-admin-sheet-items")
            .px_1()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.accent))
            .tooltip(Tooltip::text(t(lang, Key::AdminItemsSheet)))
            .child(Icon::new(IconName::Package));

        // ── шапка справа: тумблер «режим бога» слева от крестика ─
        let god_on = self.store.admin_god;
        let god_this = cx.entity();
        let trailing = div()
            .id("ld2-admin-god")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_1()
            .tooltip(Tooltip::text(t(lang, Key::AdminGodMode)))
            .child(fa_icon(
                "solid/bolt",
                px(14.0),
                rgb(if god_on { pal.accent } else { pal.muted }),
            ))
            .child(Switch::new("ld2-admin-god-switch", god_on).on_change(
                move |enabled, _, acx| {
                    god_this.update(acx, |root, cx| {
                        root.on_click(Click::AdminGod(enabled), cx);
                        cx.notify();
                    });
                },
            ));

        // ── лево: поиск + группы + каталог (uniform_list) ────────
        let query = self.ti(&self.admin_search).read(cx).text().to_lowercase();
        let tab = self.backpack_tab;
        let catalog: Vec<serde_json::Value> = self
            .store
            .admin_catalog
            .as_ref()
            .map(|items| {
                items
                    .iter()
                    .filter(|it| {
                        let ty = it["type"].as_str().unwrap_or("");
                        let in_tab = tab == BackpackTab::All || tab_for_type(ty) == tab;
                        let matches_query = query.is_empty()
                            || it["name"]
                                .as_str()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&query);
                        in_tab && matches_query
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        // Группы-как-в-рюкзаке (общий фильтр из конфига) + поиск.
        let choices: Vec<Choice> = BackpackTab::all()
            .iter()
            .map(|(candidate, key)| {
                Choice::new(candidate.as_str(), t(lang, *key)).icon(candidate.icon())
            })
            .collect();
        let tabs = Tabs::new("ld2-admin-tabs", choices, tab.as_str()).on_change({
            let this = cx.entity();
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    root.backpack_tab = BackpackTab::parse(&value);
                    root.config.backpack_tab = value;
                    root.save_config();
                    cx.notify();
                });
            }
        });

        let left_body = if self.store.admin_catalog.is_none() {
            div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading))
        } else if catalog.is_empty() {
            div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::AdminNothing))
        } else {
            // Виртуализация: uniform_list рисует только видимый
            // диапазон — 10k предметов ведут себя как 20.
            let count = catalog.len();
            let row_this = cx.entity();
            // 'static-замыкание: палитра копируется owned (Copy).
            let row_pal = *pal;
            let list = uniform_list("ld2-admin-list", count, move |range, _window, _acx| {
                range
                    .map(|ix| catalog_row(row_this.clone(), row_pal, lang, &catalog[ix]))
                    .collect::<Vec<Stateful<Div>>>()
            })
            .track_scroll(&self.admin_scroll)
            .flex_grow_1()
            .min_h_0();
            div().flex().flex_col().flex_grow_1().min_h_0().child(list)
        };
        let left = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_w_0()
            .min_h_0()
            .child(SearchInput::new(
                "ld2-admin-search",
                self.ti(&self.admin_search),
            ))
            .child(tabs)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(left_body);

        // ── право: цель (свой рюкзак) — СВОИ фильтры предметов ───
        // (вкладки независимы от левого каталога, поиск — своё поле;
        // указание заказчика: фильтры в правом окне тоже нужны).
        let inv_query = self
            .ti(&self.admin_inv_search)
            .read(cx)
            .text()
            .to_lowercase();
        let inv_tab = self.admin_inv_tab;
        let inv: Vec<ItemSnapshot> = self
            .store
            .snapshot
            .as_ref()
            .map(|s| {
                s.char
                    .inventory
                    .iter()
                    .filter(|item| {
                        let in_tab =
                            inv_tab == BackpackTab::All || tab_for_type(&item.item_type) == inv_tab;
                        let matches_query =
                            inv_query.is_empty() || item.name.to_lowercase().contains(&inv_query);
                        in_tab && matches_query
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut inv_col = div().flex().flex_col().gap_1();
        let inv_body = if inv.is_empty() {
            div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::InventoryEmpty))
        } else {
            for item in &inv {
                inv_col = inv_col.child(target_row(this.clone(), *pal, lang, item));
            }
            inv_col
        };
        let accent = pal.accent;
        let drop_this = cx.entity();
        let drop_target = div()
            .id("ld2-admin-drop")
            .flex()
            .flex_col()
            .flex_grow_1()
            .min_h_0()
            .drag_over::<AdminDrag>(move |style, _, _, _| style.border_color(rgb(accent)))
            .on_drop(move |drag: &AdminDrag, _window, acx| {
                let def_id = drag.0.clone();
                drop_this.update(acx, |root, cx| {
                    root.on_click(Click::AdminGive(def_id), cx);
                    cx.notify();
                });
            })
            .child(scrollable_tab(
                "ld2-admin-inv",
                &self.admin_inv_scroll,
                inv_body,
            ));
        let inv_choices: Vec<Choice> = BackpackTab::all()
            .iter()
            .map(|(candidate, key)| {
                Choice::new(candidate.as_str(), t(lang, *key)).icon(candidate.icon())
            })
            .collect();
        let inv_tabs = Tabs::new("ld2-admin-inv-tabs", inv_choices, inv_tab.as_str()).on_change({
            let this = cx.entity();
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    root.admin_inv_tab = BackpackTab::parse(&value);
                    cx.notify();
                });
            }
        });
        let right = div()
            .flex()
            .flex_col()
            .gap_1()
            .w(px(440.0))
            .min_h_0()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .text_color(rgb(pal.accent))
                    .child(t(lang, Key::BackpackTitle)),
            )
            .child(SearchInput::new(
                "ld2-admin-inv-search",
                self.ti(&self.admin_inv_search),
            ))
            .child(inv_tabs)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(drop_target);

        let body = div()
            .flex()
            .flex_row()
            .gap_3()
            .flex_grow_1()
            .min_h_0()
            .child(left)
            .child(div().w(px(1.0)).bg(rgb(pal.border)))
            .child(right);

        Some(self.popup_window_full(
            window,
            cx,
            pal,
            PopupKind::AdminPanel,
            t(lang, Key::AdminPanelTitle),
            Some(leading.into_any_element()),
            body,
            None,
            Some(trailing.into_any_element()),
        ))
    }
}

/// Строка каталога: двойной клик — выдача, drag — в правую колонку
/// (оба → execute-диалог количества).
fn catalog_row(
    this: Entity<Root>,
    pal: Palette,
    lang: Lang,
    item: &serde_json::Value,
) -> Stateful<Div> {
    let def_id = item["id"].as_str().unwrap_or_default().to_string();
    let name = item["name"].as_str().unwrap_or("?").to_string();
    let itype = item["type"].as_str().unwrap_or("?");
    let rarity = item["rarity"].as_str().unwrap_or("common");
    let qty = item["qty"].as_i64().unwrap_or(1);
    // Имя — цветом ранга (указание заказчика), ранг — справа.
    let name_color = rarity_color(rarity, &pal);
    let ghost_title: gpui::SharedString = name.clone().into();
    let drag_id = def_id.clone();
    div()
        .id(format!("ld2-admin-item-{def_id}"))
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(pal.selection)))
        // Имя занимает весь столбец (ellipses), ранг — по правому
        // краю, тот же цвет — у самого имени.
        .child(
            div()
                .flex_grow_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(rgb(name_color))
                .child(format!("{name} ({itype})")),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_right()
                .text_color(rgb(pal.muted))
                .child(rarity_label(lang, rarity)),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(pal.muted))
                .child(format!("×{qty}")),
        )
        .on_click(move |event: &gpui::ClickEvent, _window, acx| {
            if acx.has_active_drag() {
                return;
            }
            if event.click_count() >= 2 {
                let def_id = def_id.clone();
                this.update(acx, |root, cx| {
                    root.on_click(Click::AdminGive(def_id), cx);
                    cx.notify();
                });
            }
        })
        .on_drag(AdminDrag(drag_id), move |_, _, _, cx| {
            DragGhost::new(ghost_title.clone(), None, cx)
        })
}

/// Строка содержимого цели: двойной клик — удаление (execute-диалог
/// количества); контейнер — зона приёма drag из каталога.
fn target_row(this: Entity<Root>, pal: Palette, lang: Lang, item: &ItemSnapshot) -> Stateful<Div> {
    let uid = item.uid.clone();
    let name = item.name.clone();
    let qty = item.qty.unwrap_or(1);
    let color = item_color(item, &pal);
    let _ = lang;
    div()
        .id(format!("ld2-admin-inv-{uid}"))
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border))
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(pal.selection)))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(rgb(color))
                .child(name),
        )
        .child(div().flex_grow_1())
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(pal.muted))
                .child(format!("×{qty}")),
        )
        .on_click(move |event: &gpui::ClickEvent, _window, acx| {
            if acx.has_active_drag() {
                return;
            }
            if event.click_count() >= 2 {
                let uid = uid.clone();
                this.update(acx, |root, cx| {
                    root.on_click(Click::AdminRemove(uid), cx);
                    cx.notify();
                });
            }
        })
}
