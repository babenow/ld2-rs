//! M16/F29b: окно рюкзака — вкладки по группам типов предметов,
//! нижняя строка с валютой и последняя вкладка «Валюты» персонажа
//! (решение пользователя 2026-11-10: деньги — обычные предметы-
//! монеты, F29a). Оболочка окна (scrim/drag/resize) — общая
//! `Root::popup_window`; вкладка «Инвентарь» переехала сюда из окна
//! персонажа (правило дубликатов).

use ely::forms::Choice;
use ely::menus::ContextMenu;
use ely::navigation::Tabs;
use ely::primitives::{IconName, Tooltip};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Window};
use ld2_core::l10n::{t, Key};
use ld2_core::store::Screen;
use ld2_protocol::types::ItemSnapshot;

use crate::palette::Palette;

use super::sheet::{badges, rarity_color};
use super::{scrollable_tab, PopupKind, Root};

/// M16/F29b: вкладки рюкзака — группы типов предметов; «Валюты»
/// всегда последняя (спека пользователя), остальные — по маппингу
/// [`tab_for_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BackpackTab {
    /// Оружие: клинки и посохи (weapon, staff) — дефолтная вкладка
    /// (сохранённое значение конфига по умолчанию).
    #[default]
    Weapon,
    /// Зелья (potion).
    Potion,
    /// Магия: руны и свитки (rune, scroll).
    Magic,
    /// Материалы ремёсел (herb, wood_oak, parchment, ink).
    Material,
    /// Инструменты (flask, chisel, quill).
    Tool,
    /// Неизвестные типы контента — будущие новые типы.
    Other,
    /// Валюты (money) — за них можно покупать.
    Currencies,
}

impl BackpackTab {
    pub(crate) fn all() -> [(BackpackTab, Key); 7] {
        [
            (BackpackTab::Weapon, Key::BpTabWeapon),
            (BackpackTab::Potion, Key::BpTabPotion),
            (BackpackTab::Magic, Key::BpTabMagic),
            (BackpackTab::Material, Key::BpTabMaterial),
            (BackpackTab::Tool, Key::BpTabTool),
            (BackpackTab::Other, Key::BpTabOther),
            (BackpackTab::Currencies, Key::BpCurrencies),
        ]
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            BackpackTab::Weapon => IconName::Target,
            BackpackTab::Potion => IconName::Pipette,
            BackpackTab::Magic => IconName::WandSparkles,
            BackpackTab::Material => IconName::Box,
            BackpackTab::Tool => IconName::Wrench,
            BackpackTab::Other => IconName::Package,
            BackpackTab::Currencies => IconName::Wallet,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BackpackTab::Weapon => "weapon",
            BackpackTab::Potion => "potion",
            BackpackTab::Magic => "magic",
            BackpackTab::Material => "material",
            BackpackTab::Tool => "tool",
            BackpackTab::Other => "other",
            BackpackTab::Currencies => "currencies",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "potion" => BackpackTab::Potion,
            "magic" => BackpackTab::Magic,
            "material" => BackpackTab::Material,
            "tool" => BackpackTab::Tool,
            "other" => BackpackTab::Other,
            "currencies" => BackpackTab::Currencies,
            _ => BackpackTab::Weapon,
        }
    }

    /// Предмет принадлежит вкладке (валюты — только в «Валюты»).
    fn matches(self, item: &ItemSnapshot) -> bool {
        tab_for_type(&item.item_type) == self
    }
}

/// M16/F29b: тип предмета → группа-вкладка — маппинг в одном месте;
/// новые типы контента попадают в «Прочее» (тест покрывает
/// `content/items.json`).
fn tab_for_type(item_type: &str) -> BackpackTab {
    match item_type {
        "weapon" | "staff" => BackpackTab::Weapon,
        "potion" => BackpackTab::Potion,
        "rune" | "scroll" => BackpackTab::Magic,
        "herb" | "wood_oak" | "parchment" | "ink" => BackpackTab::Material,
        "flask" | "chisel" | "quill" => BackpackTab::Tool,
        "money" => BackpackTab::Currencies,
        // Броня — своя группа по спеке не предусмотрена (M16/F29b):
        // до появления вкладки «Броня» живёт в «Прочем».
        "armor" => BackpackTab::Other,
        _ => BackpackTab::Other,
    }
}

/// M16/F29b: суммы стаков валюты (`type=money`) по названию —
/// данные строки валюты внизу окна.
fn money_totals(items: &[ItemSnapshot]) -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = Vec::new();
    for item in items.iter().filter(|i| i.item_type == "money") {
        let qty = item.qty.unwrap_or(1);
        match out.iter_mut().find(|(name, _)| *name == item.name) {
            Some((_, total)) => *total += qty,
            None => out.push((item.name.clone(), qty)),
        }
    }
    out
}

impl Root {
    /// M16/F29b: **окно рюкзака** — всплывающий диалог (аналогично
    /// окну персонажа): вкладки групп типов, сетка предметов, внизу
    /// строка валюты; последняя вкладка — «Валюты» персонажа.
    pub(crate) fn backpack_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.backpack_popup.open || self.store.screen != Screen::Game {
            return None;
        }
        let lang = self.lang;
        let tab = self.backpack_tab;

        // Вкладки: группы типов + «Валюты» последней (спека F29b).
        let choices: Vec<Choice> = BackpackTab::all()
            .iter()
            .map(|(candidate, key)| {
                Choice::new(candidate.as_str(), t(lang, *key)).icon(candidate.icon())
            })
            .collect();
        let this = cx.entity();
        let tabs = Tabs::new("ld2-backpack-tabs", choices, tab.as_str()).on_change(
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

        // Тело: сетка активной группы в прокрутке + строка валюты
        // (всегда видна, независимо от вкладки).
        let grid = self.backpack_grid(cx, pal);
        let body = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_h_0()
            .child(tabs)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(scrollable_tab(
                "ld2-backpack-scroll",
                &self.backpack_scroll,
                grid,
            ))
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(self.currency_row(pal));

        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Backpack,
            t(lang, Key::BackpackTitle),
            body,
        ))
    }

    /// Сетка предметов активной вкладки: плитка + ПКМ-меню — те же
    /// действия, что были во вкладке «Инвентарь» окна персонажа
    /// (M15/F3/F7; тело переехало сюда, M16/F29b).
    fn backpack_grid(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let tab = self.backpack_tab;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let items: Vec<&ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|item| tab.matches(item))
            .collect();
        if items.is_empty() {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::InventoryEmpty));
        }
        let mut col = div().flex().flex_col().gap_2();
        for item in items {
            let uid = item.uid.clone();
            let detail = serde_json::to_value(item).unwrap_or_default();
            let tile = div()
                .id(format!("ld2-bp-item-{uid}"))
                .tooltip(Tooltip::text(format!(
                    "{} · {} · +{}",
                    item.name, item.rarity, item.upgrade
                )))
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
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener({
                        let detail = detail.clone();
                        move |this, _, _, cx| {
                            // Левый клик — описание (паритет тултипа egui).
                            this.store.item_detail = Some(detail.clone());
                            cx.notify();
                        }
                    }),
                )
                .child(
                    div()
                        .text_color(rgb(rarity_color(&item.rarity, pal)))
                        .child(format!("{} {}", item.name, badges(item))),
                );
            // ПКМ по предмету — контекстное меню Ely (те же действия).
            col = col.child(
                ContextMenu::new(
                    format!("ld2-bp-ctx-{uid}"),
                    self.item_actions(cx, pal, &snap.char, item),
                )
                .child(tile),
            );
        }
        col
    }

    /// M16/F29b: нижняя строка валюты — суммы стаков `money`
    /// (параллельно с вкладкой «Валюты»: строка — всегда, вкладка —
    /// детализация).
    fn currency_row(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let money = self
            .store
            .snapshot
            .as_ref()
            .map(|s| money_totals(&s.char.inventory))
            .unwrap_or_default();
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, Key::BpCurrencies)),
            );
        if money.is_empty() {
            row = row.child(div().text_color(rgb(pal.muted)).child("—"));
        } else {
            for (name, total) in money {
                row = row.child(div().text_size(px(13.0)).child(format!("{name} ×{total}")));
            }
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M16/F29b: все типы текущего контента ложатся в группы (в
    /// «Прочее» для них пусто), деньги — в «Валюты».
    #[test]
    fn backpack_groups_cover_content_types() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/items.json");
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("чтение items.json"))
                .expect("разбор items.json");
        let items = raw["items"].as_array().expect("массив items");
        assert!(!items.is_empty(), "контент есть");
        for item in items {
            let item_type = item["type"].as_str().expect("type");
            // «Прочее» — документированные исключения (без своей
            // группы по спеке пользователя).
            if item_type == "armor" {
                assert_eq!(tab_for_type(item_type), BackpackTab::Other);
                continue;
            }
            assert_ne!(
                tab_for_type(item_type),
                BackpackTab::Other,
                "тип {item_type} не замаплен — добавить в tab_for_type"
            );
        }
        assert_eq!(tab_for_type("money"), BackpackTab::Currencies);
    }

    /// M16/F29b: порядок вкладок («Валюты» — последние), round-trip
    /// parse, дефолт конфига — первая вкладка, «Прочее» ловит новые
    /// типы контента.
    #[test]
    fn backpack_tabs_order_and_round_trip() {
        let all = BackpackTab::all();
        assert_eq!(
            all[all.len() - 1].0,
            BackpackTab::Currencies,
            "«Валюты» — последняя вкладка"
        );
        for (tab, _) in all {
            assert_eq!(BackpackTab::parse(tab.as_str()), tab, "round-trip");
        }
        assert_eq!(BackpackTab::default(), BackpackTab::Weapon);
        assert_eq!(tab_for_type("saddle"), BackpackTab::Other, "новый тип");
    }

    /// M16/F29b: строка валюты суммирует стаки монет по названию и
    /// не берёт не-валютные предметы.
    #[test]
    fn currency_row_sums_money_stacks() {
        let item = |name: &str, item_type: &str, qty: i64| ItemSnapshot {
            uid: format!("i_{name}_{qty}"),
            item_type: item_type.into(),
            name: name.into(),
            rarity: "common".into(),
            upgrade: 0,
            stats: Default::default(),
            requires: None,
            sockets: None,
            qty: Some(qty),
            gold_value: Some(1),
            extra: Default::default(),
        };
        let items = vec![
            item("Золотая монета", "money", 30),
            item("Золотая монета", "money", 70),
            item("Кинжал", "weapon", 1),
        ];
        assert_eq!(
            money_totals(&items),
            vec![("Золотая монета".to_string(), 100)],
            "стаки монет сложились, не-валюта отсеяна"
        );
        assert!(money_totals(&[]).is_empty(), "пусто без монет");
    }
}
