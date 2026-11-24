//! M16/F29b: окно рюкзака — вкладки по группам типов предметов,
//! нижняя строка с валютой и последняя вкладка «Валюты» персонажа
//! (решение пользователя 2026-11-10: деньги — обычные предметы-
//! монеты, F29a). Оболочка окна (scrim/drag/resize) — общая
//! `Root::popup_window`; вкладка «Инвентарь» переехала сюда из окна
//! персонажа (правило дубликатов).

use ely::forms::Choice;
use ely::menus::ContextMenu;
use ely::motion::Flip;
use ely::navigation::Tabs;
use ely::primitives::{Icon, IconName};
use gpui::{
    div, prelude::*, px, rgb, Animation, AnimationExt, AnyElement, Context, Div, MouseButton,
    SharedString, Window,
};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::Screen;
use ld2_protocol::cmd::ItemMove;
use ld2_protocol::types::ItemSnapshot;

use crate::palette::Palette;

use super::sheet::{rarity_color, rarity_label};
use super::{scrollable_tab, BpHold, DragSource, ItemDrag, PopupKind, Root, SendSlot};

/// M16/F29b: вкладки рюкзака — группы типов предметов; «Валюты»
/// всегда последняя (спека пользователя), остальные — по маппингу
/// [`tab_for_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BackpackTab {
    /// Все предметы без фильтра (M16/F29g) — дефолтная вкладка.
    #[default]
    All,
    /// Оружие: клинки и посохи (weapon, staff).
    Weapon,
    /// Броня (armor) — шлемы/доспехи/сапоги (M16/F29f).
    Armor,
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
    /// M17/F30b: артефакты (artifact) — 2 слота особых предметов.
    Artifact,
    /// M17/F30c: самоцветы (gem) — вставляются в броню.
    Gem,
    /// Валюты (money) — за них можно покупать.
    Currencies,
}

impl BackpackTab {
    pub(crate) fn all() -> [(BackpackTab, Key); 11] {
        [
            (BackpackTab::All, Key::BpTabAll),
            (BackpackTab::Weapon, Key::BpTabWeapon),
            (BackpackTab::Armor, Key::BpTabArmor),
            (BackpackTab::Potion, Key::BpTabPotion),
            (BackpackTab::Magic, Key::BpTabMagic),
            (BackpackTab::Material, Key::BpTabMaterial),
            (BackpackTab::Tool, Key::BpTabTool),
            (BackpackTab::Other, Key::BpTabOther),
            (BackpackTab::Artifact, Key::BpTabArtifact),
            (BackpackTab::Gem, Key::BpTabGem),
            (BackpackTab::Currencies, Key::BpCurrencies),
        ]
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            BackpackTab::All => IconName::LayoutGrid,
            BackpackTab::Weapon => IconName::Target,
            BackpackTab::Armor => IconName::Shield,
            BackpackTab::Potion => IconName::Pipette,
            BackpackTab::Magic => IconName::WandSparkles,
            BackpackTab::Material => IconName::Box,
            BackpackTab::Tool => IconName::Wrench,
            BackpackTab::Other => IconName::Package,
            BackpackTab::Artifact => IconName::Sparkles,
            BackpackTab::Gem => IconName::Hexagon,
            BackpackTab::Currencies => IconName::Wallet,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BackpackTab::All => "all",
            BackpackTab::Weapon => "weapon",
            BackpackTab::Armor => "armor",
            BackpackTab::Potion => "potion",
            BackpackTab::Magic => "magic",
            BackpackTab::Material => "material",
            BackpackTab::Tool => "tool",
            BackpackTab::Other => "other",
            BackpackTab::Artifact => "artifact",
            BackpackTab::Gem => "gem",
            BackpackTab::Currencies => "currencies",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "all" => BackpackTab::All,
            "armor" => BackpackTab::Armor,
            "potion" => BackpackTab::Potion,
            "magic" => BackpackTab::Magic,
            "material" => BackpackTab::Material,
            "tool" => BackpackTab::Tool,
            "other" => BackpackTab::Other,
            "artifact" => BackpackTab::Artifact,
            "gem" => BackpackTab::Gem,
            "currencies" => BackpackTab::Currencies,
            _ => BackpackTab::Weapon,
        }
    }

    /// Предмет принадлежит вкладке: «Все предметы» — без фильтра,
    /// иначе по типу (валюты — только в «Валюты»).
    pub(crate) fn matches(self, item: &ItemSnapshot) -> bool {
        self == BackpackTab::All || tab_for_type(&item.item_type) == self
    }
}

/// M16/F29b: тип предмета → группа-вкладка — маппинг в одном месте;
/// новые типы контента попадают в «Прочее» (тест покрывает
/// `content/items.json`).
pub(crate) fn tab_for_type(item_type: &str) -> BackpackTab {
    match item_type {
        "weapon" | "staff" => BackpackTab::Weapon,
        "potion" => BackpackTab::Potion,
        "rune" | "scroll" => BackpackTab::Magic,
        "herb" | "wood_oak" | "parchment" | "ink" | "ore_copper" | "branches" | "hide_scraps"
        | "hide" | "meat" | "offal" => BackpackTab::Material,
        // M20/F20b: инструменты добычи (топор/кирка/серп) — к инструментам.
        "flask" | "chisel" | "quill" | "upgrade_stone" | "axe" | "pickaxe" | "sickle"
        | "skinning_knife" => BackpackTab::Tool,
        "money" => BackpackTab::Currencies,
        "armor" | "shield" => BackpackTab::Armor,
        "artifact" => BackpackTab::Artifact,
        "gem" => BackpackTab::Gem,
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
        let grid = self.backpack_grid(cx, pal, "ld2-bp");
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
            ));

        // M16/F29g: нижняя панель — по аналогии с верхней: иконка
        // монет и количество денег персонажа.
        let money: i64 = self
            .store
            .snapshot
            .as_ref()
            .map(|s| {
                money_totals(&s.char.inventory)
                    .into_iter()
                    .map(|(_, q)| q)
                    .sum()
            })
            .unwrap_or(0);
        // M18/F58f (новая фича): заполненность в статус-баре —
        // занято записей-стаков (как серверный count) из лимита
        // `inventory.slots`; при полном — красным.
        let usage: Option<(usize, u32)> = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.inventory.len())
            .zip(self.store.session.as_ref().map(|s| s.inventory.slots));
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
            .child(Icon::new(IconName::Wallet))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(money.to_string()),
            )
            .child(div().flex_grow_1())
            .children(usage.map(|(used, total)| {
                let full = (used as u32) >= total;
                div()
                    .text_color(rgb(if full { pal.danger } else { pal.muted }))
                    .child(tf(
                        lang,
                        Key::BackpackUsage,
                        &[
                            ("{used}", &used.to_string()),
                            ("{total}", &total.to_string()),
                        ],
                    ))
            }));

        Some(self.popup_window(
            window,
            cx,
            pal,
            PopupKind::Backpack,
            t(lang, Key::BackpackTitle),
            None,
            body,
            Some(footer),
        ))
    }

    /// Сетка предметов активной вкладки: плитка + ПКМ-меню — те же
    /// действия, что были во вкладке «Инвентарь» окна персонажа
    /// (M15/F3/F7; тело переехало сюда, M16/F29b).
    /// Сетка предметов активной вкладки: плитка + контекстное меню
    /// (ЛКМ и ПКМ), перетаскивание меняет предметы местами (Reorder →
    /// `cmd/item.move`). Надетое сюда не попадает (M16/F29f).
    /// `id_prefix` — вставка в id виджетов (сетка/строки): окно
    /// подятия (M23) рисует СВОЙ экземпляр рюкзака одновременно с
    /// этим окном — id в кадре обязаны различаться.
    pub(crate) fn backpack_grid(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        id_prefix: &str,
    ) -> Div {
        let lang = self.lang;
        let tab = self.backpack_tab;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        // M16/F29f: надетое «на персонаже» из рюкзака убираем —
        // оно живёт в блоке экипировки окна персонажа.
        let equipped: std::collections::HashSet<&str> = snap
            .char
            .equip
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        let items: Vec<&ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|item| tab.matches(item) && !equipped.contains(item.uid.as_str()))
            .collect();
        if items.is_empty() {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::InventoryEmpty));
        }

        // M16/F29g + R-dnd: ЛКМ/ПКМ открывают меню; drag — свой
        // (ItemDrag): внутри списка меняет порядок (ItemMove), на
        // зоны — экипировка/снятие, вне зон — подтверждение выброса.
        let this = cx.entity();
        let order: Vec<String> = items.iter().map(|i| i.uid.clone()).collect();
        let mut flip = Flip::new(format!("{id_prefix}-flip"));
        for (ix, item) in items.into_iter().enumerate() {
            let uid = item.uid.clone();
            let tile = div()
                .id(format!("{id_prefix}-item-{uid}"))
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
                // Эпик/легендарка: статичное внутреннее свечение в
                // покое; анимация — только по наведению (заказчик).
                .shadow(super::sheet::rarity_glow_inset(&item.rarity, pal))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(pal.selection)))
                .on_mouse_move(cx.listener(|_, _, _, cx| cx.notify()))
                // M18/F58f: подпись в две строки — слоты под названием
                // (указание заказчика); слоты рун — их глифы.
                .child(super::sheet::item_label_stack(
                    item,
                    super::sheet::item_color(item, pal),
                ));
            // Контекстное меню — только ПКМ (заказчик: ЛКМ-меню
            // убрано везде; оно конкурировало с перетаскиванием).
            let tile = tile.on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let uid = uid.clone();
                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                        window.prevent_default();
                        this.bump_bp_menu(uid.clone(), event.position, cx);
                    }
                }),
            );
            // Переливание (заказчик 2026-11-20): эпик/легендарка — на
            // наведении оттенок рамки и свечения циклится (~4 с).
            let hover_key = format!("bp:{uid}");
            let shimmering = matches!(item.rarity.as_str(), "epic" | "legendary")
                && self.hovered_item.as_deref() == Some(hover_key.as_str());
            let hover_this = cx.entity();
            let hover_key_hover = hover_key.clone();
            let tile = tile.on_hover(move |hovered, _, cx| {
                let key = hover_key_hover.clone();
                hover_this.update(cx, |root, cx| {
                    root.hovered_item = hovered.then_some(key);
                    cx.notify();
                });
            });
            let tile = if shimmering {
                let base = rarity_color(&item.rarity, pal) & 0xff_ffff;
                tile.with_animation(
                    SharedString::from(format!("shimmer-{hover_key}")),
                    Animation::new(std::time::Duration::from_secs(4)).repeat(),
                    move |el, t| {
                        let hsla: gpui::Hsla = gpui::rgba((base << 8) | 0xff).into();
                        // Бриллиантовое переливание: играет ЯРКОСТЬ
                        // цвета редкости — оттенок не меняется.
                        let l = (hsla.l + 0.18 * (2.0 * std::f32::consts::PI * t).sin())
                            .clamp(0.0, 1.0);
                        el.border_color(gpui::hsla(hsla.h, hsla.s, l, 1.0))
                            .shadow(vec![gpui::BoxShadow {
                                color: gpui::hsla(hsla.h, hsla.s, l, 0.35),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(8.0),
                                spread_radius: px(0.0),
                                inset: true,
                            }])
                    },
                )
                .into_any_element()
            } else {
                tile.into_any_element()
            };
            let request = match &self.bp_menu {
                Some((u, n, at)) if *u == uid => Some((*n, *at)),
                _ => None,
            };
            let ctx = ContextMenu::new(
                format!("ld2-bp-ctx-{uid}"),
                self.item_actions(cx, pal, &snap.char, item),
            )
            .manual(request)
            .child(tile);
            // Тултип описания (заказчик 2026-11-20): тот же виджет, что
            // и окно описания; появляется через 1с наведения курсора;
            // свечение медленно пульсирует НАРУЖУ (~2.5 с). Карточка —
            // общий helper с предметами на земле (R-dnd, дедуп).
            let tip_detail = serde_json::to_value(item).unwrap_or_default();
            let tip_skills = self.skill_names_for(&tip_detail);
            let ctx = super::sheet::item_tooltip_trigger(
                format!("ld2-bp-tip-{uid}"),
                ctx,
                tip_detail,
                tip_skills,
                self.config.theme.clone(),
                lang,
                item.rarity.clone(),
                format!(
                    "{} · {} · +{}",
                    item.name,
                    rarity_label(lang, &item.rarity),
                    item.upgrade
                ),
                uid.clone(),
            );
            // R-dnd: старт перетаскивания — на САМОЙ внешней строке
            // (Flip-row): единообразно для всех редкостей (заказчик:
            // у эпиков/легендарок анимация-обёртка плитки ломала
            // hit-test жеста — drag не стартовал).
            let drag_payload = ItemDrag {
                uid: uid.clone(),
                from: DragSource::Bp,
                slot_req: item.slot.clone(),
            };
            let drag_this = cx.entity();
            let bp_index = ix;
            let ghost_name = item.name.clone();
            let ghost_color = rarity_color(&item.rarity, pal);
            let ghost_theme = self.config.theme.clone();
            let row_uid = uid.clone();
            let row_el = div()
                .id(format!("{id_prefix}-drag-{uid}"))
                .pb(px(8.0))
                .child(ctx)
                .on_drag(drag_payload, move |drag, offset, _, cx| {
                    tracing::info!("dnd: старт drag {} из рюкзака", drag.uid);
                    drag_this.update(cx, |root, cx| {
                        root.drag_item = Some(ItemDrag {
                            uid: drag.uid.clone(),
                            from: DragSource::Bp,
                            slot_req: drag.slot_req.clone(),
                        });
                        root.popup_scrim_down = false;
                        root.bp_hold = Some(BpHold {
                            uid: drag.uid.clone(),
                            index: bp_index,
                            target: bp_index,
                            top: 0.0,
                            grab: offset.y.as_f32(),
                        });
                        cx.notify();
                    });
                    // Карточка предмета под курсором (заказчик).
                    let name = ghost_name.clone();
                    let color = ghost_color;
                    let theme = ghost_theme.clone();
                    cx.new(move |_| crate::root::ItemDragGhost { name, color, theme })
                });
            flip = flip.row(row_uid, row_el);
        }
        // R-dnd: список на ItemDrag — drag внутри списка меняет порядок
        // (ItemMove), на зоны — экипировка/снятие, вне зон —
        // подтверждение выброса (заказчик 2026-11-21).
        let order_len = order.len();
        let order_drop = order.clone();
        let this_drop = this.clone();
        let list = div()
            .id("ld2-bp-list")
            .flex()
            .flex_col()
            .relative()
            .child(flip)
            .on_drag_move::<ItemDrag>(move |event, _window, cx| {
                // Пересчёт reorder — только когда курсор над списком
                // (слушает все перемещения при активном drag). Маркер
                // зоны (drop_hit) ведёт КАРТОЧКА окна — здесь не трогаем,
                // иначе clear-ы спорили с карточкой по порядку (заказчик:
                // отпускание в окне рюкзака открывало подтверждение).
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                this.update(cx, |root, cx| {
                    let Some(drag) = root.drag_item.clone() else {
                        return;
                    };
                    if matches!(drag.from, DragSource::Bp) {
                        let Some(mut hold) = root.bp_hold.clone() else {
                            return;
                        };
                        let pointer = (event.event.position.y - event.bounds.top()).as_f32();
                        let row_h =
                            (event.bounds.size.height.as_f32() / order_len.max(1) as f32).max(1.0);
                        hold.target = ((pointer / row_h) as usize).min(order_len - 1);
                        hold.top = (pointer - hold.grab).max(0.0);
                        root.bp_hold = Some(hold);
                        cx.notify();
                    }
                });
            })
            // R-dnd: отпускание НАД списком — reorder (hover hit-test);
            // отпускание на подложке окна ловит карточка (on_drop).
            .on_drop::<ItemDrag>(move |drag: &ItemDrag, _, cx| {
                this_drop.update(cx, |root, cx| {
                    if matches!(drag.from, DragSource::Bp) {
                        if let Some(hold) = root.bp_hold.take() {
                            if hold.index != hold.target {
                                if let Some(target_uid) = order_drop.get(hold.target) {
                                    tracing::info!("dnd: drop в списке → reorder");
                                    let cmd = ItemMove {
                                        item_uid: hold.uid.clone(),
                                        target_uid: target_uid.clone(),
                                    };
                                    let _ = root.send_or_notice(&cmd, SendSlot::Notice);
                                }
                            }
                        }
                    }
                    root.drag_item = None;
                    root.bp_hold = None;
                    cx.notify();
                });
            });
        // Горизонтальный простор внешней ауре свечения (кромка
        // скролл-области иначе срезает её слева/справа).
        div().px(px(10.0)).child(list)
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
        assert_eq!(all[0].0, BackpackTab::All, "«Все предметы» — первая");
        assert_eq!(
            all[all.len() - 1].0,
            BackpackTab::Currencies,
            "«Валюты» — последняя вкладка"
        );
        for (tab, _) in all {
            assert_eq!(BackpackTab::parse(tab.as_str()), tab, "round-trip");
        }
        assert_eq!(BackpackTab::default(), BackpackTab::All);
        assert_eq!(tab_for_type("saddle"), BackpackTab::Other, "новый тип");
        assert_eq!(tab_for_type("shield"), BackpackTab::Armor, "щит — в броне");
        assert_eq!(
            tab_for_type("artifact"),
            BackpackTab::Artifact,
            "артефакты — своей вкладкой"
        );
        assert_eq!(
            tab_for_type("gem"),
            BackpackTab::Gem,
            "самоцветы — своей вкладкой"
        );
    }

    /// M16/F29b: строка валюты суммирует стаки монет по названию и
    /// не берёт не-валютные предметы.
    #[test]
    fn currency_row_sums_money_stacks() {
        let item = |name: &str, item_type: &str, qty: i64| ItemSnapshot {
            slot: None,
            hands: None,
            def_id: None,
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
            bonuses: None,
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
