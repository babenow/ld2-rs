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

use ld2_core::domain;

use ely::forms::{Choice, SearchInput, Switch};
use ely::navigation::Tabs;
use ely::primitives::{DragGhost, Icon, IconName, Tooltip};
use gpui::{
    div, prelude::*, px, rgb, uniform_list, AnyElement, Context, Div, Entity, Stateful, Window,
};
use ld2_core::l10n::{t, Key, Lang};
use ld2_core::store::Screen;
use ld2_protocol::types::EntityKind;

use crate::fa::fa_icon;
use crate::palette::Palette;

use super::backpack::{tab_for_type, BackpackTab};
use super::sheet::{quality_color, rarity_color};
use super::{act_btn, scrollable_tab, Click, PopupKind, Root};

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

        let left = self.admin_left(cx, pal, lang, this.clone());
        let right = self.admin_right(cx, pal, lang, this);
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

    /// Левая колонка админ-окна: поиск, группы, каталог (RF3n).
    fn admin_left(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        lang: ld2_core::l10n::Lang,
        _this: gpui::Entity<Root>,
    ) -> gpui::Div {
        // ── лево: поиск + группы + каталог (uniform_list) ────────
        let query = self.ti(&self.admin_search).read(cx).text().to_lowercase();
        let tab = self.backpack_tab;
        let rarity = self.admin_rarity.clone();
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
                        let matches_rarity =
                            rarity == "all" || it["rarity"].as_str() == Some(rarity.as_str());
                        in_tab && matches_query && matches_rarity
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

        // Фильтр раритета — чипы цветом ранга; состояния независимы
        // по колонкам (id-префиксы разные — уникальность в кадре).
        let rarity_row = rarity_filter_row(
            cx,
            *pal,
            lang,
            &self.admin_rarity,
            "ld2-admin-rarity",
            |root, v| root.admin_rarity = v,
        );

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
                    .map(|ix| catalog_row(row_this.clone(), row_pal, &catalog[ix]))
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
            .child(rarity_row)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(left_body);

        left
    }

    /// Правая колонка админ-окна: цель, фильтры своего рюкзака,
    /// инвентарь цели (RF3n).
    fn admin_right(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        lang: ld2_core::l10n::Lang,
        this: gpui::Entity<Root>,
    ) -> gpui::Div {
        let inv_rarity_row = rarity_filter_row(
            cx,
            *pal,
            lang,
            &self.admin_inv_rarity,
            "ld2-admin-inv-rarity",
            |root, v| root.admin_inv_rarity = v,
        );
        // ── право: цель (свой рюкзак) — СВОИ фильтры предметов ───
        // (вкладки независимы от левого каталога, поиск — своё поле;
        // указание заказчика: фильтры в правом окне тоже нужны).
        let inv_query = self
            .ti(&self.admin_inv_search)
            .read(cx)
            .text()
            .to_lowercase();
        let inv_tab = self.admin_inv_tab;
        let inv_rarity = self.admin_inv_rarity.clone();
        // ── цель правой колонки (F24c): чипы (свой рюкзак + монстры
        // и трупы локации) + ввод имени игрока (в т.ч. оффлайн) ────
        let mut targets: Vec<(String, String)> =
            vec![("self".to_string(), t(lang, Key::BackpackTitle).to_string())];
        if let Some(snap) = self.store.snapshot.as_ref() {
            for e in &snap.location.entities {
                match e.kind {
                    EntityKind::Monster => {
                        targets.push((format!("monster:{}", e.id), e.name.clone()));
                    }
                    EntityKind::Corpse => {
                        targets.push((format!("corpse:{}", e.id), e.name.clone()));
                    }
                    // F24d: сундуки и NPC с магазином (маркер
                    // extra.shop ставит сервер в снапшоте локации).
                    EntityKind::Container => {
                        targets.push((format!("container:{}", e.id), e.name.clone()));
                    }
                    EntityKind::Npc
                        if e.extra.get("shop").and_then(|v| v.as_bool()) == Some(true) =>
                    {
                        targets.push((format!("shop:{}", e.id), e.name.clone()));
                    }
                    _ => {}
                }
            }
        }
        let target_label = if self.admin_target == "self" {
            t(lang, Key::BackpackTitle).to_string()
        } else if let Some(name) = self.admin_target.strip_prefix("char:") {
            name.to_string()
        } else {
            targets
                .iter()
                .find(|(v, _)| *v == self.admin_target)
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| self.admin_target.clone())
        };
        let target_chips = target_chip_row(cx, *pal, &targets, &self.admin_target);
        let player_row = div()
            .flex()
            .flex_row()
            .gap_1()
            .child(SearchInput::new(
                "ld2-admin-player",
                self.ti(&self.admin_player),
            ))
            .child(act_btn(
                cx,
                t(lang, Key::AdminTargetGo),
                pal.accent,
                0xffffff,
                |root, cx| {
                    let name = root
                        .ti(&root.admin_player)
                        .read(cx)
                        .text()
                        .trim()
                        .to_string();
                    if !name.is_empty() {
                        root.on_click(Click::AdminTarget(format!("char:{name}")), cx);
                    }
                },
            ));

        let inv_body =
            self.admin_inv_body(pal, lang, inv_tab, &inv_query, &inv_rarity, this.clone());
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
                    .text_size(px(crate::ui_tokens::FONT_MD))
                    .text_color(rgb(pal.accent))
                    .child(target_label),
            )
            .child(target_chips)
            .child(player_row)
            .child(SearchInput::new(
                "ld2-admin-inv-search",
                self.ti(&self.admin_inv_search),
            ))
            .child(inv_tabs)
            .child(inv_rarity_row)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(drop_target)
            // M24: «Удалить все» — очистка содержимого выбранной цели
            // (инвентарь/труп/сундук/магазин/монстр — на сервере).
            .child(div().flex().flex_row().justify_end().px_1().child(act_btn(
                cx,
                t(lang, Key::AdminClearAll),
                pal.danger,
                0xffffff,
                |root, cx| {
                    let target = root.admin_target.clone();
                    root.on_click(Click::AdminClear(target), cx);
                },
            )));

        right
    }

    /// Содержимое инвентаря цели: снапшот или кэш `admin.target.info`
    /// плюс свои фильтры вкладки/поиска/раритета (RF3n).
    fn admin_inv_body(
        &mut self,
        pal: &Palette,
        lang: ld2_core::l10n::Lang,
        inv_tab: BackpackTab,
        inv_query: &str,
        inv_rarity: &str,
        this: gpui::Entity<Root>,
    ) -> gpui::Div {
        // Содержимое: self — снапшот (события инвентаря), прочие
        // цели — `cmd/admin.target.info` (кэш в сторе).
        let target_loading = self.admin_target != "self" && self.store.admin_target_items.is_none();
        let target_items: Vec<TargetItem> = if self.admin_target == "self" {
            self.store
                .snapshot
                .as_ref()
                .map(|s| {
                    s.char
                        .inventory
                        .iter()
                        .map(|i| TargetItem {
                            r#ref: i.uid.clone(),
                            def_id: i.def_id.clone(),
                            name: i.name.clone(),
                            item_type: i.item_type.clone(),
                            rarity: i.rarity.clone(),
                            qty: i.qty.unwrap_or(1),
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.store
                .admin_target_items
                .as_ref()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|i| {
                            Some(TargetItem {
                                r#ref: i["ref"].as_str()?.to_string(),
                                def_id: i["id"].as_str().map(str::to_string),
                                name: i["name"].as_str()?.to_string(),
                                item_type: i["type"].as_str().unwrap_or("").to_string(),
                                rarity: i["rarity"].as_str().unwrap_or("").to_string(),
                                qty: i["qty"].as_i64().unwrap_or(1),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        // Свои фильтры (вкладки/поиск/раритет) действуют и на цель.
        let filtered: Vec<&TargetItem> = target_items
            .iter()
            .filter(|it| {
                let in_tab = inv_tab == BackpackTab::All || tab_for_type(&it.item_type) == inv_tab;
                let matches_query =
                    inv_query.is_empty() || it.name.to_lowercase().contains(inv_query);
                let matches_rarity = inv_rarity == "all" || it.rarity == inv_rarity;
                in_tab && matches_query && matches_rarity
            })
            .collect();
        let empty_key = if self.admin_target == "self" {
            Key::InventoryEmpty
        } else {
            Key::AdminNothing
        };
        if target_loading {
            div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading))
        } else if filtered.is_empty() {
            div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, empty_key))
        } else {
            let mut col = div().flex().flex_col().gap_1();
            for item in &filtered {
                col = col.child(target_row(this.clone(), *pal, lang, item));
            }
            col
        }
    }
}

/// Строка каталога: двойной клик — выдача, drag — в правую колонку
/// (оба → execute-диалог количества).
fn catalog_row(this: Entity<Root>, pal: Palette, item: &serde_json::Value) -> Stateful<Div> {
    let def_id = item["id"].as_str().unwrap_or_default().to_string();
    let name = item["name"].as_str().unwrap_or("?").to_string();
    let rarity = item["rarity"].as_str().unwrap_or(domain::RARITY_COMMON);
    let qty = item["qty"].as_i64().unwrap_or(1);
    // Отображение — как в правой колонке (указание заказчика): имя
    // цветом качества/ранга, количество справа; без суффикса типа
    // и без отдельного текста ранга.
    let name_color =
        quality_color(Some(def_id.as_str())).unwrap_or_else(|| rarity_color(rarity, &pal));
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
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(rgb(name_color))
                .child(name),
        )
        .child(div().flex_grow_1())
        .child(
            div()
                .text_size(px(crate::ui_tokens::FONT_XS))
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

/// M24: единая строка содержимого цели (self — из снапшота, прочие
/// цели — из `admin.target.info`): ref = uid / `loot:`/`bonus:`.
#[derive(Debug, Clone)]
struct TargetItem {
    r#ref: String,
    def_id: Option<String>,
    name: String,
    item_type: String,
    rarity: String,
    qty: i64,
}

/// Строка содержимого цели: двойной клик — удаление (execute-диалог
/// количества); контейнер — зона приёма drag из каталога.
fn target_row(this: Entity<Root>, pal: Palette, lang: Lang, item: &TargetItem) -> Stateful<Div> {
    let uid = item.r#ref.clone();
    let name = item.name.clone();
    let qty = item.qty;
    let color =
        quality_color(item.def_id.as_deref()).unwrap_or_else(|| rarity_color(&item.rarity, &pal));
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
                .text_size(px(crate::ui_tokens::FONT_XS))
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

/// M24: строка фильтра раритета (чипы цветом ранга) над списком —
/// «Все» + пять редкостей; `id_prefix` различает левую/правую колонку
/// (одинаковые id в одном кадре конфликтовали бы), `set` — установка
/// нужного поля состояния.
fn rarity_filter_row(
    cx: &mut Context<'_, Root>,
    pal: Palette,
    lang: Lang,
    active: &str,
    id_prefix: &str,
    set: fn(&mut Root, String),
) -> Div {
    const OPTIONS: [(&str, Key); 6] = [
        ("all", Key::RarityAll),
        (domain::RARITY_COMMON, Key::RarityCommon),
        ("uncommon", Key::RarityUncommon),
        ("rare", Key::RarityRare),
        ("epic", Key::RarityEpic),
        ("legendary", Key::RarityLegendary),
    ];
    let mut row = div().flex().flex_row().flex_wrap().gap_1().items_center();
    for (value, key) in OPTIONS {
        let is_active = active == value;
        let color = if value == "all" {
            pal.muted
        } else {
            rarity_color(value, &pal)
        };
        let this = cx.entity();
        let selected_bg = pal.selection;
        let border = if is_active { pal.accent } else { pal.border };
        let bg = if is_active { pal.selection } else { pal.panel };
        let value_owned = value.to_string();
        row = row.child(
            div()
                .id(format!("{id_prefix}-{value}"))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(border))
                .bg(rgb(bg))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(selected_bg)))
                .text_size(px(crate::ui_tokens::FONT_XS))
                .text_color(rgb(color))
                .child(t(lang, key))
                .on_click(move |_, _window, acx| {
                    let value = value_owned.clone();
                    this.update(acx, |root, cx| {
                        set(root, value);
                        cx.notify();
                    });
                }),
        );
    }
    row
}

/// M24: чипы выбора цели правой колонки — свой рюкзак + сущности
/// локации (монстры/трупы); активная цель подсвечена.
fn target_chip_row(
    cx: &mut Context<'_, Root>,
    pal: Palette,
    targets: &[(String, String)],
    active: &str,
) -> Div {
    let mut row = div().flex().flex_row().flex_wrap().gap_1().items_center();
    for (value, label) in targets {
        let is_active = active == value;
        let this = cx.entity();
        let selected_bg = pal.selection;
        let border = if is_active { pal.accent } else { pal.border };
        let bg = if is_active { pal.selection } else { pal.panel };
        let value_owned = value.clone();
        row = row.child(
            div()
                .id(format!("ld2-admin-target-{value}"))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(border))
                .bg(rgb(bg))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(selected_bg)))
                .text_size(px(crate::ui_tokens::FONT_XS))
                .child(label.clone())
                .on_click(move |_, _window, acx| {
                    let value = value_owned.clone();
                    this.update(acx, |root, cx| {
                        root.on_click(Click::AdminTarget(value), cx);
                        cx.notify();
                    });
                }),
        );
    }
    row
}
