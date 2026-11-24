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
use ely::primitives::{Icon, IconName, Tooltip};
use ely::theme::IconSize;
use gpui::{
    div, prelude::*, px, rgb, Animation, AnimationExt, AnyElement, Context, Div, MouseButton,
    SharedString, Window,
};
use ld2_core::l10n::{t, tf, Key, Lang};
use ld2_core::store::Screen;
use ld2_protocol::cmd::ItemMove;
use ld2_protocol::types::CharacterState;
use ld2_protocol::types::ItemSnapshot;

use crate::palette::Palette;

/// M28/B4: контейнеры предметов — те же строки, что и на сервере
/// (`db::items::containers::BACKPACK/BAGS`).
const CONTAINER_BACKPACK: &str = "backpack";
const CONTAINER_BAGS: &str = "bags";

/// M28/B8: спец-слоты под сумки после общих — реагенты, колчан,
/// ключница (те же индексы, что у сервера: `bag_slots + 0/1/2`).
const SPECIAL_BAG_SLOTS: usize = 3;

/// M28/B8: роль спец-слота в контейнере `bags` (`None` — обычный).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BagSlotRole {
    Reagent,
    Quiver,
    Keyring,
}

impl BagSlotRole {
    fn at(ix: usize, general: usize) -> Option<Self> {
        match ix.checked_sub(general)? {
            0 => Some(Self::Reagent),
            1 => Some(Self::Quiver),
            2 => Some(Self::Keyring),
            _ => None,
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Reagent => IconName::Package,
            Self::Quiver => IconName::Crosshair,
            Self::Keyring => IconName::Key,
        }
    }

    fn label(self, lang: Lang) -> String {
        let key = match self {
            Self::Reagent => Key::BpBagSlotReagent,
            Self::Quiver => Key::BpBagSlotQuiver,
            Self::Keyring => Key::BpBagSlotKeyring,
        };
        t(lang, key).to_string()
    }
}

/// M28/B8: иконка и подпись слота сумки (общий вид для панели и
/// статус-бара).
fn bag_slot_visual(ix: usize, general: usize, lang: Lang) -> (IconName, String) {
    match BagSlotRole::at(ix, general) {
        Some(role) => (role.icon(), role.label(lang)),
        None => (
            IconName::Archive,
            tf(lang, Key::BpBagSlot, &[("{n}", &(ix + 1).to_string())]),
        ),
    }
}

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
    /// M28/B1: сумки-контейнеры (bag) — своя вкладка.
    Bag,
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
    pub(crate) fn all() -> [(BackpackTab, Key); 12] {
        [
            (BackpackTab::All, Key::BpTabAll),
            (BackpackTab::Weapon, Key::BpTabWeapon),
            (BackpackTab::Armor, Key::BpTabArmor),
            (BackpackTab::Potion, Key::BpTabPotion),
            (BackpackTab::Magic, Key::BpTabMagic),
            (BackpackTab::Material, Key::BpTabMaterial),
            (BackpackTab::Tool, Key::BpTabTool),
            (BackpackTab::Bag, Key::BpTabBag),
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
            BackpackTab::Bag => IconName::Archive,
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
            BackpackTab::Bag => "bag",
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
            "bag" => BackpackTab::Bag,
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
        | "hide" | "meat" | "offal" | "ammo" | "key" => BackpackTab::Material,
        // M20/F20b: инструменты добычи (топор/кирка/серп) — к инструментам.
        "flask" | "chisel" | "quill" | "upgrade_stone" | "axe" | "pickaxe" | "sickle"
        | "skinning_knife" => BackpackTab::Tool,
        "bag" => BackpackTab::Bag,
        "money" => BackpackTab::Currencies,
        "armor" | "shield" => BackpackTab::Armor,
        "artifact" => BackpackTab::Artifact,
        "gem" => BackpackTab::Gem,
        _ => BackpackTab::Other,
    }
}

/// M28: подпись типа предмета для `bag_types` (неизвестный — как есть).
fn bag_type_label(lang: Lang, item_type: &str) -> String {
    match item_type {
        "ammo" => t(lang, Key::BagTypeAmmo).to_string(),
        "herb" => t(lang, Key::BagTypeHerb).to_string(),
        "gem" => t(lang, Key::BagTypeGem).to_string(),
        "key" => t(lang, Key::BagTypeKey).to_string(),
        other => other.to_string(),
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

        // Тело: панель сумок и разделы «Рюкзак» / выбранная сумка
        // (M28/B4) в прокрутке + строка валюты (всегда видна).
        let bag_bar = self.bag_bar(cx, pal);
        let bags = self.equipped_bags();
        let selected_bag = self
            .backpack_bag
            .clone()
            .filter(|uid| bags.iter().any(|b| &b.uid == uid));
        self.backpack_bag = selected_bag.clone();
        let mut sections = div().flex().flex_col().gap_1();
        let bp_head = self.container_header(
            cx,
            pal,
            None,
            t(lang, Key::BpBackpackSection).to_string(),
            false,
        );
        sections = sections.child(bp_head);
        let bp_grid = self.container_grid(cx, pal, "ld2-bp", None);
        sections = sections.child(bp_grid);
        if let Some(uid) = selected_bag.as_deref() {
            let bag_name = bags
                .iter()
                .find(|b| b.uid == uid)
                .map(|b| b.name.clone())
                .unwrap_or_default();
            let bag_head = self.container_header(cx, pal, Some(uid), bag_name, true);
            sections = sections.child(bag_head);
            let bag_grid = self.container_grid(cx, pal, "ld2-bp-in-bag", Some(uid));
            sections = sections.child(bag_grid);
        }
        let body = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_h_0()
            .child(tabs)
            .child(div().h(px(1.0)).bg(rgb(pal.border)))
            .child(bag_bar)
            .child(scrollable_tab(
                "ld2-backpack-scroll",
                &self.backpack_scroll,
                sections,
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
        // M28: занятость — предметы рюкзака и содержимое сумок (сами
        // сумки не занимают ячейки); ёмкость — база + вместимость
        // надетых сумок. M28/B8: та же цифра — в статус-баре.
        let usage = self.inventory_usage();
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
        self.container_grid(cx, pal, id_prefix, None)
    }

    /// M28/B4: сетка контейнера — `None` — базовый рюкзак, иначе
    /// содержимое сумки с этим uid (сами надетые сумки сюда не
    /// попадают — у них своя панель).
    fn container_grid(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        id_prefix: &str,
        container: Option<&str>,
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
            .filter(|item| {
                let is_bag = item.container.as_deref() == Some(CONTAINER_BAGS);
                tab.matches(item)
                    && !equipped.contains(item.uid.as_str())
                    && if is_bag {
                        // Сами сумки — только во вкладке «Сумки».
                        container.is_none() && tab == BackpackTab::Bag
                    } else {
                        Self::in_container(item, container)
                    }
            })
            .collect();
        if items.is_empty() {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::InventoryEmpty));
        }

        // R-dnd: список на ItemDrag — drag внутри списка меняет порядок
        // (ItemMove), на зоны — экипировка/снятие, вне зон —
        // подтверждение выброса (заказчик 2026-11-21).
        let this = cx.entity();
        let order: Vec<String> = items.iter().map(|i| i.uid.clone()).collect();
        let mut flip = Flip::new(format!("{id_prefix}-flip"));
        for (ix, item) in items.iter().enumerate() {
            let uid = item.uid.clone();
            let row_el = self.backpack_row(cx, pal, &snap.char, item, ix, id_prefix);
            flip = flip.row(uid, row_el);
        }
        let order_len = order.len();
        let order_drop = order.clone();
        let list_uids: std::collections::HashSet<String> = order.iter().cloned().collect();
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
                    // M28/B4: предмет из другого контейнера кладётся
                    // зонами разделов/слотов, а не reorder-ом списка.
                    if !list_uids.contains(&drag.uid) {
                        root.drag_item = None;
                        root.bp_hold = None;
                        cx.notify();
                        return;
                    }
                    if matches!(drag.from, DragSource::Bp) {
                        if let Some(hold) = root.bp_hold.take() {
                            if hold.index != hold.target {
                                if let Some(target_uid) = order_drop.get(hold.target) {
                                    tracing::info!("dnd: drop в списке → reorder");
                                    let cmd = ItemMove {
                                        item_uid: hold.uid.clone(),
                                        target_uid: target_uid.clone(),
                                        to_container: None,
                                        to_index: None,
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

    /// Плитка предмета в сетке рюкзака: фон, подпись в две строки,
    /// ПКМ-меню и переливание редкости на наведении. Вынесено из
    /// `backpack_grid` (RF3q), тело перенесено как есть.
    /// M28/B4: предмет лежит в разделе `container` (`None` — рюкзак).
    /// Надетые сумки (`bags`) не показываются ни в одной сетке.
    fn in_container(item: &ItemSnapshot, container: Option<&str>) -> bool {
        match container {
            None => matches!(item.container.as_deref(), None | Some(CONTAINER_BACKPACK)),
            Some(uid) => item.container.as_deref() == Some(uid),
        }
    }

    /// M28/B4: надетые сумки, по порядку слотов.
    fn equipped_bags(&self) -> Vec<ItemSnapshot> {
        let Some(snap) = self.store.snapshot.as_ref() else {
            return Vec::new();
        };
        let mut bags: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| i.container.as_deref() == Some(CONTAINER_BAGS))
            .cloned()
            .collect();
        bags.sort_by_key(|b| b.slot_index.unwrap_or(i32::MAX));
        bags
    }

    /// M28: занято ячеек и общая ёмкость (рюкзак + надетые сумки).
    pub(crate) fn inventory_usage(&self) -> Option<(usize, u32)> {
        let snap = self.store.snapshot.as_ref()?;
        let bags: Vec<&ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| i.container.as_deref() == Some(CONTAINER_BAGS))
            .collect();
        let used = snap
            .char
            .inventory
            .iter()
            .filter(|i| i.container.as_deref() != Some(CONTAINER_BAGS))
            .count();
        let base = self
            .store
            .session
            .as_ref()
            .map(|c| c.inventory.slots)
            .unwrap_or(0);
        let cap = base + bags.iter().map(|b| b.bag_slots.unwrap_or(0)).sum::<u32>();
        Some((used, cap))
    }

    /// M28/B8: сумки в статус-баре — иконки-кнопки левее ёмкости:
    /// наведение — название/занятость, клик открывает окно рюкзака.
    pub(crate) fn status_bags(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let general = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.bag_slots as usize)
            .unwrap_or(0);
        let total = general + SPECIAL_BAG_SLOTS;
        let bags = self.equipped_bags();
        let items: Vec<ItemSnapshot> = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.inventory.clone())
            .unwrap_or_default();
        let mut row = div().flex().flex_row().items_center().gap_1();
        for ix in 0..total {
            let ix_i = i32::try_from(ix).unwrap_or(0);
            let bag = bags.iter().find(|b| b.slot_index == Some(ix_i)).cloned();
            let (icon, _) = bag_slot_visual(ix, general, lang);
            let tip = match &bag {
                Some(b) => {
                    let used = items
                        .iter()
                        .filter(|i| i.container.as_deref() == Some(b.uid.as_str()))
                        .count();
                    tf(
                        lang,
                        Key::BpBagTip,
                        &[
                            ("{name}", b.name.as_str()),
                            ("{used}", &used.to_string()),
                            ("{total}", &b.bag_slots.unwrap_or(0).to_string()),
                        ],
                    )
                }
                None => t(lang, Key::BpBagEmptySlot).to_string(),
            };
            let color = if bag.is_some() { pal.text } else { pal.muted };
            let bag_uid = bag.as_ref().map(|b| b.uid.clone());
            row = row.child(
                div()
                    .id(format!("ld2-status-bag-{ix}"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(pal.selection)))
                    .tooltip(Tooltip::text(tip))
                    .child(Icon::new(icon).size(IconSize::Sm).color(rgb(color)))
                    .on_click(cx.listener(move |root, _, _, cx| {
                        if !root.backpack_popup.open {
                            root.open_popup(PopupKind::Backpack);
                        }
                        root.backpack_bag = bag_uid.clone();
                        cx.notify();
                    })),
            );
        }
        row
    }

    /// M28/B4: первый свободный индекс контейнера (по снапшоту).
    fn first_free_slot(items: &[ItemSnapshot], container: Option<&str>, cap: u32) -> Option<i32> {
        let cap = i32::try_from(cap).unwrap_or(0);
        (0..cap).find(|ix| {
            !items
                .iter()
                .any(|i| Self::in_container(i, container) && i.slot_index == Some(*ix))
        })
    }

    /// M28/B4: заголовок раздела — счётчик и зона приёма drag
    /// («положить в этот контейнер»).
    fn container_header(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        container: Option<&str>,
        title: String,
        closable: bool,
    ) -> Div {
        let lang = self.lang;
        let items: Vec<ItemSnapshot> = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.inventory.clone())
            .unwrap_or_default();
        let cap = match container {
            None => self
                .store
                .session
                .as_ref()
                .map(|s| s.inventory.slots)
                .unwrap_or(0),
            Some(uid) => items
                .iter()
                .find(|i| i.uid == uid)
                .and_then(|i| i.bag_slots)
                .unwrap_or(0),
        };
        let used = items
            .iter()
            .filter(|i| Self::in_container(i, container))
            .count();
        let free = Self::first_free_slot(&items, container, cap);
        let accepts: Option<String> = container
            .and_then(|uid| items.iter().find(|i| i.uid == uid))
            .and_then(|i| i.bag_types.clone())
            .filter(|list| !list.is_empty())
            .map(|list| {
                list.iter()
                    .map(|ty| bag_type_label(lang, ty))
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        // Для drop-замыкания: None — базовый рюкзак.
        let target = container
            .map(str::to_string)
            .unwrap_or_else(|| CONTAINER_BACKPACK.to_string());
        let this = cx.entity();
        let usage = tf(
            lang,
            Key::BackpackUsage,
            &[("{used}", &used.to_string()), ("{total}", &cap.to_string())],
        );
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .bg(rgb(pal.selection))
            .border_b_1()
            .border_color(rgb(pal.border))
            .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(div().text_color(rgb(pal.muted)).child(usage));
        if let Some(accepts) = accepts {
            head = head.child(div().text_color(rgb(pal.muted)).child(tf(
                lang,
                Key::BpBagAccepts,
                &[("types", accepts.as_str())],
            )));
        }
        head = head.child(div().flex_grow_1());
        if closable {
            let this_close = this.clone();
            head = head.child(
                div()
                    .id("ld2-bp-bag-close")
                    .cursor_pointer()
                    .text_color(rgb(pal.muted))
                    .child("×")
                    .on_click(move |_, _, cx| {
                        this_close.update(cx, |root, cx| {
                            root.backpack_bag = None;
                            cx.notify();
                        });
                    }),
            );
        }
        head.on_drop::<ItemDrag>(move |drag: &ItemDrag, _, cx| {
            let (Some(index), false) = (free, matches!(drag.from, DragSource::Equip(_))) else {
                return;
            };
            let target = target.clone();
            this.update(cx, |root, cx| {
                let cmd = ItemMove {
                    item_uid: drag.uid.clone(),
                    target_uid: String::new(),
                    to_container: Some(target),
                    to_index: Some(index),
                };
                let _ = root.send_or_notice(&cmd, SendSlot::Notice);
                root.drag_item = None;
                root.bp_hold = None;
                cx.notify();
            });
        })
    }

    /// M28/B4: панель слотов под сумки. Клик — показать содержимое
    /// вторым разделом; drop — надеть сумку или положить предмет
    /// внутрь (первый свободный слот).
    fn bag_bar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let Some(general) = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.bag_slots as usize)
        else {
            return div();
        };
        let total = general + SPECIAL_BAG_SLOTS;
        let lang = self.lang;
        let bags = self.equipped_bags();
        let items: Vec<ItemSnapshot> = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.inventory.clone())
            .unwrap_or_default();
        let bag_uids: std::collections::HashSet<String> =
            bags.iter().map(|b| b.uid.clone()).collect();
        let selected = self.backpack_bag.clone();
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::BpTabBag)),
            );
        for ix in 0..total {
            let ix_i = i32::try_from(ix).unwrap_or(0);
            let bag = bags.iter().find(|b| b.slot_index == Some(ix_i)).cloned();
            let (icon, empty_label) = bag_slot_visual(ix, general, lang);
            let label = match &bag {
                Some(b) => format!("{} · {}", b.name, b.bag_slots.unwrap_or(0)),
                None => empty_label,
            };
            let is_sel = selected
                .as_deref()
                .zip(bag.as_ref().map(|b| b.uid.as_str()))
                .is_some_and(|(a, b)| a == b);
            let bag_uid = bag.as_ref().map(|b| b.uid.clone());
            let bag_cap = bag.as_ref().and_then(|b| b.bag_slots).unwrap_or(0);
            let free = bag_uid
                .as_deref()
                .and_then(|uid| Self::first_free_slot(&items, Some(uid), bag_cap));
            let bag_for_click = bag_uid.clone();
            let this = cx.entity();
            let this_drop = this.clone();
            let bag_for_drop = bag_uid.clone();
            let bags_drop = bag_uids.clone();
            let free_drop = free;
            let cell_id = format!("ld2-bp-bagslot-{ix}");
            let mut label_el = div().id(format!("ld2-bp-baglabel-{ix}")).child(label);
            if let Some(b) = &bag {
                let ghost = (
                    b.name.clone(),
                    rarity_color(&b.rarity, pal),
                    self.config.theme.clone(),
                );
                let drag_this = this.clone();
                label_el = label_el.on_drag(
                    ItemDrag {
                        uid: b.uid.clone(),
                        from: DragSource::Bp,
                        slot_req: None,
                    },
                    move |drag: &ItemDrag, _offset, _, cx| {
                        drag_this.update(cx, |root, cx| {
                            root.drag_item = Some(ItemDrag {
                                uid: drag.uid.clone(),
                                from: DragSource::Bp,
                                slot_req: None,
                            });
                            root.popup_scrim_down = false;
                            cx.notify();
                        });
                        let (name, color, theme) = ghost.clone();
                        cx.new(move |_| crate::root::ItemDragGhost { name, color, theme })
                    },
                );
            }
            let cell = div()
                .id(cell_id)
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(if is_sel { pal.accent } else { pal.border }))
                .bg(rgb(if is_sel { pal.selection } else { pal.panel }))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(pal.selection)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(Icon::new(icon).size(IconSize::Sm))
                        .child(label_el),
                )
                .on_click(cx.listener(move |root, _, _, cx| {
                    root.backpack_bag = if root.backpack_bag.as_deref() == bag_for_click.as_deref()
                    {
                        None
                    } else {
                        bag_for_click.clone()
                    };
                    cx.notify();
                }))
                .on_drop::<ItemDrag>(move |drag: &ItemDrag, _, cx| {
                    this_drop.update(cx, |root, cx| {
                        if matches!(drag.from, DragSource::Equip(_)) {
                            return;
                        }
                        let cmd = if bags_drop.contains(&drag.uid) {
                            ItemMove {
                                item_uid: drag.uid.clone(),
                                target_uid: String::new(),
                                to_container: Some(CONTAINER_BAGS.to_string()),
                                to_index: Some(ix_i),
                            }
                        } else if let (Some(target), Some(free_ix)) =
                            (bag_for_drop.clone(), free_drop)
                        {
                            ItemMove {
                                item_uid: drag.uid.clone(),
                                target_uid: String::new(),
                                to_container: Some(target),
                                to_index: Some(free_ix),
                            }
                        } else {
                            root.drag_item = None;
                            cx.notify();
                            return;
                        };
                        let _ = root.send_or_notice(&cmd, SendSlot::Notice);
                        root.drag_item = None;
                        cx.notify();
                    });
                });
            row = row.child(cell);
        }
        row
    }

    fn backpack_tile(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        ch: &CharacterState,
        item: &ItemSnapshot,
        id_prefix: &str,
    ) -> AnyElement {
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
                    let l =
                        (hsla.l + 0.18 * (2.0 * std::f32::consts::PI * t).sin()).clamp(0.0, 1.0);
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
        ContextMenu::new(
            format!("ld2-bp-ctx-{uid}"),
            self.item_actions(cx, pal, ch, item),
        )
        .manual(request)
        .child(tile)
        .into_any_element()
    }

    /// Строка списка рюкзака: плитка + тултип описания + drag
    /// (Reorder/экипировка/выброс). Вынесено из `backpack_grid`
    /// (RF3q), тело перенесено как есть.
    fn backpack_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        ch: &CharacterState,
        item: &ItemSnapshot,
        ix: usize,
        id_prefix: &str,
    ) -> impl IntoElement {
        let uid = item.uid.clone();
        let tile = self.backpack_tile(cx, pal, ch, item, id_prefix);
        // Тултип описания (заказчик 2026-11-20): тот же виджет, что
        // и окно описания; появляется через 1с наведения курсора;
        // свечение медленно пульсирует НАРУЖУ (~2.5 с). Карточка —
        // общий helper с предметами на земле (R-dnd, дедуп).
        let tip_detail = serde_json::to_value(item).unwrap_or_default();
        let tip_skills = self.skill_names_for(&tip_detail);
        let ctx = super::sheet::item_tooltip_trigger(
            format!("ld2-bp-tip-{uid}"),
            tile,
            tip_detail,
            tip_skills,
            self.config.theme.clone(),
            self.lang,
            item.rarity.clone(),
            format!(
                "{} · {} · +{}",
                item.name,
                rarity_label(self.lang, &item.rarity),
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
        div()
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
            })
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
            container: None,
            slot_index: None,
            bag_slots: None,
            bag_types: None,
            bag_kind: None,
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
