//! Правая панель-лист gpui (M10.4) — порт `ui/sheet.rs` egui-клиента:
//! трекер квестов, 7 вкладок (навыки/атрибуты/инвентарь/статусы/квесты/
//! профессии/гильдия) и модалки предмета/улучшения/инкрустации.
//!
//! Отличия от egui: инвентарь без drag&drop (действия в меню по ПКМ —
//! те же, что в контекстном меню egui), тултипы заменены модалкой
//! описания, combobox обучения — ряд кнопок. Состояние вкладки —
//! `Root::sheet_tab` (персистентно в config, ФР-19).

use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, MouseButton};
use ld2_core::l10n::{format_duration, t, Key, Lang};
use ld2_core::store::{crime_kind_title, filter_active_statuses, local_epoch, Act, UpgradeView};
use ld2_protocol::cmd::{
    CharXpInvest, CraftLearn, CraftRecipes, CraftStart, CrimeLog, GuildBrowse, GuildLeave,
    GuildRankSet, ItemDrop, ItemEquip, ItemUnequip, ItemUse, QuestAbandon, QuestAccept,
    QuestTurnIn, SocketExtract, SocketInsert, UpgradePreview, UpgradeStart,
};
use ld2_protocol::types::{CharacterState, ItemSnapshot, QuestPhase, Severity};

use crate::palette::Palette;
use ely::forms::{Choice, Input, Select};
use ely::navigation::Tabs;

use super::{act_btn, Root};

/// Вкладки листа (паритет egui `SheetTab`; персистентно строкой в config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetTab {
    Skills,
    Attrs,
    Inventory,
    Statuses,
    Quests,
    Professions,
    Guild,
}

impl SheetTab {
    fn all() -> [(SheetTab, Key); 7] {
        [
            (SheetTab::Skills, Key::SheetSkills),
            (SheetTab::Attrs, Key::SheetAttrs),
            (SheetTab::Inventory, Key::SheetInventory),
            (SheetTab::Statuses, Key::StatusesTab),
            (SheetTab::Quests, Key::QuestsTab),
            (SheetTab::Professions, Key::ProfessionsTab),
            (SheetTab::Guild, Key::GuildTab),
        ]
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SheetTab::Skills => "skills",
            SheetTab::Attrs => "attrs",
            SheetTab::Inventory => "inventory",
            SheetTab::Statuses => "statuses",
            SheetTab::Quests => "quests",
            SheetTab::Professions => "professions",
            SheetTab::Guild => "guild",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "attrs" => SheetTab::Attrs,
            "inventory" => SheetTab::Inventory,
            "statuses" => SheetTab::Statuses,
            "quests" => SheetTab::Quests,
            "professions" => SheetTab::Professions,
            "guild" => SheetTab::Guild,
            _ => SheetTab::Skills,
        }
    }
}

/// Демо-каталог профессий для обучения (skills.md §5; сервер валидирует).
const LEARN_CATALOG: [(&str, Key); 7] = [
    ("blacksmith", Key::ProfBlacksmith),
    ("alchemist", Key::ProfAlchemist),
    ("woodcarver", Key::ProfWoodcarver),
    ("tailor", Key::ProfTailor),
    ("cook", Key::ProfCook),
    ("herbalist", Key::ProfHerbalist),
    ("hunter", Key::ProfHunter),
];

impl Root {
    /// M11.2: диалоги листа (ФР-07/09/10): предмет/улучшение/инкрустация.
    pub(crate) fn sheet_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if let Some(detail) = self.store.item_detail.clone() {
            let name = detail["name"].as_str().unwrap_or("?").to_string();
            let body = self.item_view(cx, pal, detail).into_any_element();
            let close = self.dismiss(cx, |r| r.store.item_detail = None);
            return Some(
                Dialog::new("ld2-item", name, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(view) = self.store.upgrade.clone() {
            let title = view.item_name.clone();
            let body = self.upgrade_view(cx, pal, &view).into_any_element();
            let close = self.dismiss(cx, |r| r.store.upgrade = None);
            return Some(
                Dialog::new("ld2-upgrade", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(staff_uid) = self.store.socketing.clone() {
            let title = t(self.lang, Key::SocketingTitle).to_string();
            let body = self.socket_view(cx, pal, staff_uid).into_any_element();
            let close = self.dismiss(cx, |r| r.store.close_socketing());
            return Some(
                Dialog::new("ld2-socket", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    // ── каркас панели ─────────────────────────────────────────────────────

    pub(crate) fn sheet(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        // Автозагрузки вкладок один раз (ФР-11/08/16) — как в egui.
        if self.store.snapshot.is_some() {
            if self.sheet_tab == SheetTab::Statuses && !self.store.crime_log_loaded {
                self.store.crime_log_loaded = true;
                self.send_cmd(&CrimeLog { limit: Some(50) }, Act::CrimeLog);
            }
            if self.sheet_tab == SheetTab::Professions && !self.store.recipes_loaded {
                self.store.recipes_loaded = true;
                self.send_cmd(&CraftRecipes { profession: None }, Act::Craft);
            }
            if self.sheet_tab == SheetTab::Guild && !self.store.guild_browse_loaded {
                self.store.guild_browse_loaded = true;
                self.send_cmd(&GuildBrowse { query: None }, Act::GuildBrowse);
            }
        }

        let lang = self.lang;
        let tab = self.sheet_tab;
        let mut col = div().flex().flex_col().gap_1().w(px(300.0)).flex_none();

        col = col.children(self.tracker_block(pal));

        // M11.4: вкладки листа — Tabs Ely (выбор + клавиатурная навигация).
        let choices: Vec<Choice> = SheetTab::all()
            .iter()
            .map(|(candidate, key)| Choice::new(candidate.as_str(), t(lang, *key)))
            .collect();
        let this = cx.entity();
        col = col.child(
            Tabs::new("ld2-sheet-tabs", choices, tab.as_str()).on_change(
                move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, cx| {
                        root.sheet_tab = SheetTab::parse(&value);
                        root.config.sheet_tab = value;
                        root.save_config();
                        cx.notify();
                    });
                },
            ),
        );
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));

        if let Some(err) = &self.store.sheet_error {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        col = col.child(match tab {
            SheetTab::Skills => self.tab_skills(cx, pal),
            SheetTab::Attrs => self.tab_attrs(pal),
            SheetTab::Inventory => self.tab_inventory(cx, pal),
            SheetTab::Statuses => self.tab_statuses(cx, pal),
            SheetTab::Quests => self.tab_quests(cx, pal),
            SheetTab::Professions => self.tab_professions(cx, pal),
            SheetTab::Guild => self.tab_guild(cx, pal),
        });
        col
    }

    /// Отправка команды листа (паритет egui `send`).
    fn send_cmd<C: ld2_protocol::Command>(&mut self, cmd: &C, act: Act) {
        match self.transport.send(cmd) {
            Ok(_) => self.store.on_sheet_sent(act),
            Err(e) => self.store.sheet_error = Some(format!("нет связи: {e}")),
        }
    }

    // ── трекер квестов (ФР-12) ────────────────────────────────────────────

    fn tracker_block(&self, pal: &Palette) -> Option<Div> {
        if self.store.quest_tracker.is_empty() {
            return None;
        }
        let lang = self.lang;
        let snap = self.store.snapshot.as_ref()?;
        let mut block = div()
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::TrackerLabel)),
            );
        for id in &self.store.quest_tracker {
            if let Some(q) = snap.char.quests.iter().find(|q| &q.id == id) {
                block = block.child(div().text_size(px(13.0)).child(format!(
                    "{} — {}",
                    q.title.clone().unwrap_or_else(|| q.id.clone()),
                    goals_summary(q)
                )));
            }
        }
        Some(block)
    }

    // ── вкладка «Навыки» (ФР-04) ──────────────────────────────────────────

    fn tab_skills(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col =
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(gpui::FontWeight::BOLD).child(format!(
                    "{}: {}",
                    t(lang, Key::XpPoolLabel),
                    snap.char.xp_pool
                )));
        for skill in &snap.char.skills {
            let name = skill.name.clone().unwrap_or_else(|| skill.id.clone());
            let id = skill.id.clone();
            let rank = skill.rank;
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(div().child(name))
                        .child(div().text_color(rgb(pal.muted)).child(format!("·{rank}"))),
                )
                .child(bar(
                    if skill.xp_to_next > 0 {
                        skill.xp as f32 / skill.xp_to_next as f32
                    } else {
                        0.0
                    },
                    format!("{}/{}", skill.xp, skill.xp_to_next),
                    pal.accent,
                    pal,
                ));
            // Вложение очков из пула (D3/D4, ФР-04).
            if snap.char.xp_pool > 0 {
                let pool = snap.char.xp_pool;
                let mut buttons = div().flex().flex_row().gap_1();
                for (label, points) in [("+1", 1_i64), ("+5", 5_i64)] {
                    let sid = id.clone();
                    buttons =
                        buttons.child(act_btn(cx, label, pal.panel, pal.text, move |this, cx| {
                            this.invest(&sid, points.min(pool));
                            let _ = cx;
                        }));
                }
                let sid = id.clone();
                buttons = buttons.child(act_btn(
                    cx,
                    t(lang, Key::MaxLabel),
                    pal.panel,
                    pal.text,
                    move |this, cx| {
                        this.invest(&sid, pool);
                        let _ = cx;
                    },
                ));
                row = row.child(buttons);
            }
            col = col.child(row);
        }
        col
    }

    fn invest(&mut self, skill_id: &str, points: i64) {
        let cmd = CharXpInvest {
            skill_id: skill_id.to_string(),
            points,
        };
        self.send_cmd(&cmd, Act::Invest);
    }

    // ── вкладка «Атрибуты» ────────────────────────────────────────────────

    fn tab_attrs(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_1();
        for (key, value) in &snap.char.attrs {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(div().text_color(rgb(pal.muted)).child(key.clone()))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(value.to_string()),
                    ),
            );
        }
        col
    }

    // ── вкладка «Инвентарь» (ФР-07) ───────────────────────────────────────

    fn tab_inventory(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let equip_slots: Vec<String> = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.equip_slots.clone())
            .unwrap_or_default();

        let mut col = div().flex().flex_col().gap_2();

        // Слоты экипировки (ФР-07): вместо drag&drop — действия в строке.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::EquipLabel)),
        );
        for slot in &equip_slots {
            let uid = snap.char.equip.get(slot).cloned().flatten();
            let item = uid
                .as_ref()
                .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == *u));
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(
                    div()
                        .w(px(72.0))
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(slot.clone()),
                );
            match item {
                Some(item) => {
                    let color = rarity_color(&item.rarity, pal);
                    row = row.child(div().flex_grow_1().text_color(rgb(color)).child(format!(
                        "{} {}",
                        item.name,
                        badges(item)
                    )));
                    let slot = slot.clone();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::MenuUnequip),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            let cmd = ItemUnequip { slot: slot.clone() };
                            this.send_cmd(&cmd, Act::Unequip);
                            let _ = cx;
                        },
                    ));
                    let detail = serde_json::to_value(item).unwrap_or_default();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::MenuDesc),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            this.store.item_detail = Some(detail.clone());
                            let _ = cx;
                        },
                    ));
                }
                None => {
                    row = row.child(
                        div()
                            .flex_grow_1()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(lang, Key::EmptySlot)),
                    );
                }
            }
            col = col.child(row);
        }

        // Сетка инвентаря (ФР-07): клик — описание, ПКМ — действия.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::SheetInventory)),
        );
        if snap.char.inventory.is_empty() {
            col = col.child(div().text_color(rgb(pal.muted)).child("…"));
        }
        let menu_for = self.item_menu.clone();
        for item in &snap.char.inventory {
            let selected = menu_for.as_deref() == Some(item.uid.as_str());
            let uid = item.uid.clone();
            let detail = serde_json::to_value(item).unwrap_or_default();
            let tile = div()
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
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener({
                        let detail = detail.clone();
                        move |this, _, _, cx| {
                            // Левый клик — описание (паритет тултипа egui).
                            this.item_menu = None;
                            this.store.item_detail = Some(detail.clone());
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Right,
                    cx.listener({
                        let uid = uid.clone();
                        move |this, _, _, cx| {
                            this.item_menu = Some(uid.clone());
                            cx.notify();
                        }
                    }),
                )
                .child(
                    div()
                        .text_color(rgb(rarity_color(&item.rarity, pal)))
                        .child(format!("{} {}", item.name, badges(item))),
                );
            col = col.child(tile);

            // Меню действий (паритет контекстного меню egui).
            if selected {
                col = col.child(self.item_actions(cx, pal, &snap.char, item));
            }
        }
        col
    }

    /// Действия предмета — те же, что в контекстном меню egui (ФР-07/09/10).
    fn item_actions(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        ch: &CharacterState,
        item: &ItemSnapshot,
    ) -> Div {
        let lang = self.lang;
        let mut menu = div()
            .flex()
            .flex_col()
            .gap_1()
            .ml(px(12.0))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.accent));

        // Использовать (свиток/зелье/…).
        let uid = item.uid.clone();
        menu = menu.child(act_btn(
            cx,
            t(lang, Key::MenuUse),
            pal.panel,
            pal.text,
            move |this, cx| {
                let cmd = ItemUse {
                    item_uid: uid.clone(),
                    target_id: None,
                };
                this.send_cmd(&cmd, Act::Drop);
                this.item_menu = None;
                let _ = cx;
            },
        ));

        // Экипировать (гейт клиента по типу; сервер — истина, §7).
        if let Some(slot) = slot_for(&item.item_type) {
            let equipped = ch.equip.values().flatten().any(|u| u == &item.uid);
            if !equipped {
                let uid = item.uid.clone();
                let slot = slot.to_string();
                menu = menu.child(act_btn(
                    cx,
                    t(lang, Key::MenuEquip),
                    pal.panel,
                    pal.text,
                    move |this, cx| {
                        let cmd = ItemEquip {
                            item_uid: uid.clone(),
                            slot: slot.clone(),
                        };
                        this.send_cmd(&cmd, Act::Equip);
                        this.item_menu = None;
                        let _ = cx;
                    },
                ));
            }
        }

        // Улучшение (ФР-10): до максимума из конфига сервера.
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let upgradable = matches!(item.item_type.as_str(), "weapon" | "armor" | "staff");
        if upgradable && item.upgrade < max_level {
            let uid = item.uid.clone();
            let name = item.name.clone();
            menu = menu.child(act_btn(
                cx,
                t(lang, Key::UpgradeBtn),
                pal.panel,
                pal.text,
                move |this, cx| {
                    this.store.open_upgrade(uid.clone(), name.clone());
                    let cmd = UpgradePreview {
                        item_uid: uid.clone(),
                    };
                    this.send_cmd(&cmd, Act::Preview);
                    this.item_menu = None;
                    let _ = cx;
                },
            ));
        }

        // Инкрустация (ФР-09): только посох с сокетами.
        if item.item_type == "staff" && item.sockets.is_some() {
            let uid = item.uid.clone();
            menu = menu.child(act_btn(
                cx,
                t(lang, Key::BtnSocketing),
                pal.panel,
                pal.text,
                move |this, cx| {
                    this.store.open_socketing(uid.clone());
                    this.item_menu = None;
                    let _ = cx;
                },
            ));
        }

        // Описание / Выбросить.
        let detail = serde_json::to_value(item).unwrap_or_default();
        menu = menu.child(act_btn(
            cx,
            t(lang, Key::MenuDesc),
            pal.panel,
            pal.text,
            move |this, cx| {
                this.store.item_detail = Some(detail.clone());
                this.item_menu = None;
                let _ = cx;
            },
        ));
        let uid = item.uid.clone();
        menu = menu.child(act_btn(
            cx,
            t(lang, Key::MenuDrop),
            pal.panel,
            pal.danger,
            move |this, cx| {
                let cmd = ItemDrop {
                    item_uid: uid.clone(),
                    qty: None,
                };
                this.send_cmd(&cmd, Act::Drop);
                this.item_menu = None;
                let _ = cx;
            },
        ));
        menu
    }

    // ── вкладка «Статусы» (ФР-11, M6) ────────────────────────────────────

    fn tab_statuses(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        // Отсчёт по серверным срокам: now = локальное + offset (§1.1).
        let now = local_epoch() + self.store.time_offset;
        let active = filter_active_statuses(&snap.char.crime.statuses, now);
        let mut col = div().flex().flex_col().gap_1();

        if active.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoStatuses)),
            );
        }
        for status in &active {
            let title = crime_kind_title(&self.store, &status.kind);
            let left = status.expires_at - now;
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(severity_rgb(status.severity)))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(format_duration(lang, left)),
                    ),
            );
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::CrimeJournal)),
        );
        col = col.child(act_btn(
            cx,
            t(lang, Key::ReloadJournal),
            pal.panel,
            pal.text,
            |this, cx| {
                let cmd = CrimeLog { limit: Some(50) };
                this.send_cmd(&cmd, Act::CrimeLog);
                let _ = cx;
            },
        ));
        if self.store.crime_log.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::JournalEmpty)),
            );
        }
        for entry in self.store.crime_log.iter().rev().take(60) {
            let title = crime_kind_title(&self.store, &entry.kind);
            col = col.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(format!(
                        "{title}: +{} · {} {}",
                        format_duration(lang, entry.delta_s),
                        t(lang, Key::TotalLabel),
                        format_duration(lang, entry.total_s)
                    )),
            );
        }
        col
    }

    // ── вкладка «Квесты» (ФР-12) ──────────────────────────────────────────

    fn tab_quests(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        if snap.char.quests.is_empty() {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::NoQuests));
        }
        let mut col = div().flex().flex_col().gap_2();
        for quest in &snap.char.quests {
            let tracked = self.store.quest_tracker.contains(&quest.id);
            let (state_key, color) = match quest.state {
                QuestPhase::Available => (Key::StateAvailable, 0x98_c3_79),
                QuestPhase::Active => (Key::StateActive, 0xab_b2_bf),
                QuestPhase::TurnIn => (Key::StateTurnIn, 0xd1_9a_66),
                QuestPhase::Done => (Key::StateDone, 0x61_af_ef),
            };
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
                        .child(div().text_color(rgb(color)).child(t(lang, state_key)))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(quest.title.clone().unwrap_or_else(|| quest.id.clone())),
                        ),
                );

            // Трекер (ФР-12): максимум 3, переключение.
            let qid = quest.id.clone();
            card = card.child(act_btn(
                cx,
                if tracked {
                    t(lang, Key::TrackedBtn)
                } else {
                    t(lang, Key::TrackBtn)
                },
                if tracked { pal.accent } else { pal.panel },
                pal.text,
                move |this, cx| {
                    this.store.toggle_quest_tracker(&qid);
                    let _ = cx;
                },
            ));

            // Кнопки состояния (§7).
            let mut actions = div().flex().flex_row().gap_1();
            match quest.state {
                QuestPhase::Available => {
                    let qid = quest.id.clone();
                    actions = actions.child(act_btn(
                        cx,
                        t(lang, Key::BtnAccept),
                        pal.accent,
                        pal.accent_text,
                        move |this, cx| {
                            let cmd = QuestAccept {
                                quest_id: qid.clone(),
                            };
                            this.send_cmd(&cmd, Act::Quest);
                            let _ = cx;
                        },
                    ));
                }
                QuestPhase::Active => {
                    let qid = quest.id.clone();
                    actions = actions.child(act_btn(
                        cx,
                        t(lang, Key::BtnAbandon),
                        pal.panel,
                        pal.danger,
                        move |this, cx| {
                            let cmd = QuestAbandon {
                                quest_id: qid.clone(),
                            };
                            this.send_cmd(&cmd, Act::Quest);
                            let _ = cx;
                        },
                    ));
                }
                QuestPhase::TurnIn => {
                    let qid = quest.id.clone();
                    actions = actions.child(act_btn(
                        cx,
                        t(lang, Key::BtnTurnIn),
                        pal.accent,
                        pal.accent_text,
                        move |this, cx| {
                            let cmd = QuestTurnIn {
                                quest_id: qid.clone(),
                            };
                            this.send_cmd(&cmd, Act::Quest);
                            let _ = cx;
                        },
                    ));
                }
                QuestPhase::Done => {}
            }
            card = card.child(actions);

            if !quest.goals.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", goals_summary(quest))),
                );
            }
            if let Some(reward) = &quest.reward_preview {
                if let Some(xp) = reward.xp {
                    card = card.child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(format!("{}: {xp} XP", t(lang, Key::RewardLabel))),
                    );
                }
            }
            col = col.child(card);
        }
        col
    }

    // ── вкладка «Профессии» (ФР-08) ───────────────────────────────────────

    fn tab_professions(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_2();

        // Изученные профессии: ранги и XP (ФР-08).
        for prof in &snap.char.professions {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(div().child(prof_label(lang, &prof.id)))
                            .child(div().text_color(rgb(pal.muted)).child(
                                t(lang, Key::RankFmt).replace("{r}", &prof.rank.to_string()),
                            )),
                    )
                    .child(bar(
                        if prof.xp_to_next > 0 {
                            prof.xp as f32 / prof.xp_to_next as f32
                        } else {
                            0.0
                        },
                        format!("{}/{}", prof.xp, prof.xp_to_next),
                        pal.accent,
                        pal,
                    )),
            );
        }

        // M11.3: обучение — Select Ely + подтверждение (отклонение
        // «ряд кнопок вместо combobox» снят).
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
                    t(lang, Key::LearnLabel),
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

        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
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

        // Рецепты: требования, ингредиенты (красные при нехватке — ФР-08).
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

            // Крафт: кнопка активна при полном наборе и вне ожидания (§7).
            let pending = self.store.pending == Some(Act::Craft);
            if ingredients_ok && !pending {
                card = card.child(act_btn(
                    cx,
                    t(lang, Key::BtnCraft),
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
            } else {
                card = card.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.border))
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::BtnCraft)),
                );
            }

            if !station.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(station),
                );
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
        col
    }

    // ── вкладка «Гильдия» (ФР-16, M8) ─────────────────────────────────────

    fn tab_guild(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let target = self.store.target.clone();
        let guild = self.store.guild.clone();
        let my_id = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.id.clone())
            .unwrap_or_default();

        let Some(g) = guild else {
            // Не в гильдии: создать или вступить по списку мира (§7).
            let mut col = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::GuildNone)),
                )
                .child(Input::new(self.ti(&self.guild_input)))
                .child(act_btn(
                    cx,
                    t(lang, Key::GuildCreateBtn),
                    pal.accent,
                    pal.accent_text,
                    |this, cx| {
                        let name = this.ti(&this.guild_input).read(cx).text().to_string();
                        let name = name.trim().to_string();
                        if !name.is_empty() {
                            let cmd = ld2_protocol::cmd::GuildCreate { name };
                            this.send_cmd(&cmd, Act::Guild);
                        }
                    },
                ));
            col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
            col = col.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, Key::GuildBrowseLbl)),
            );
            if self.store.guild_browse.is_empty() {
                col = col.child(div().text_color(rgb(pal.muted)).child("…"));
            }
            for entry in self.store.guild_browse.clone() {
                let id = entry["id"].as_str().unwrap_or("").to_string();
                let name = entry["name"].as_str().unwrap_or("?").to_string();
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_2()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .child(div().child(name))
                        .child(act_btn(
                            cx,
                            t(lang, Key::GuildJoinBtn),
                            pal.accent,
                            pal.accent_text,
                            move |this, cx| {
                                let cmd = ld2_protocol::cmd::GuildJoin {
                                    guild_id: Some(id.clone()),
                                };
                                this.send_cmd(&cmd, Act::Guild);
                                let _ = cx;
                            },
                        )),
                );
            }
            if let Some(err) = &self.store.sheet_error.clone() {
                col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
            }
            return col;
        };

        // В гильдии: инфо, состав, операции, журнал.
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(g.name.clone()),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(lang, Key::GuildMembers)),
            );
        let i_am_master = g
            .members
            .iter()
            .any(|m| m.id == my_id && m.rank == "master");
        for member in &g.members {
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(div().flex_grow_1().child(member.name.clone()))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(if member.rank == "master" {
                            pal.warn
                        } else {
                            pal.muted
                        }))
                        .child(member.rank.clone()),
                );
            if member.online {
                row = row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.success))
                        .child("●"),
                );
            }
            if i_am_master && member.id != my_id {
                let pid = member.id.clone();
                let next = match member.rank.as_str() {
                    "master" => "officer",
                    "officer" => "member",
                    _ => "master",
                }
                .to_string();
                let pid2 = pid.clone();
                let rank = next.clone();
                row = row.child(act_btn(
                    cx,
                    format!("{} → {next}", t(lang, Key::GuildRankLbl)),
                    pal.panel,
                    pal.text,
                    move |this, cx| {
                        let cmd = GuildRankSet {
                            player_id: pid2.clone(),
                            rank: rank.clone(),
                        };
                        this.send_cmd(&cmd, Act::Guild);
                        let _ = cx;
                    },
                ));
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::GuildKickBtn),
                    pal.panel,
                    pal.danger,
                    move |this, cx| {
                        let cmd = ld2_protocol::cmd::GuildKick {
                            player_id: pid.clone(),
                        };
                        this.send_cmd(&cmd, Act::Guild);
                        let _ = cx;
                    },
                ));
            }
            col = col.child(row);
        }

        let mut ops = div().flex().flex_row().gap_1();
        if let Some(target) = &target {
            let tid = target.clone();
            ops = ops.child(act_btn(
                cx,
                t(lang, Key::GuildInviteBtn),
                pal.panel,
                pal.text,
                move |this, cx| {
                    let cmd = ld2_protocol::cmd::GuildInvite {
                        player_id: tid.clone(),
                    };
                    this.send_cmd(&cmd, Act::Guild);
                    let _ = cx;
                },
            ));
        }
        let gid = g.id.clone();
        ops = ops.child(act_btn(
            cx,
            t(lang, Key::GuildLeaveBtn),
            pal.panel,
            pal.danger,
            move |this, cx| {
                let cmd = GuildLeave {
                    guild_id: Some(gid.clone()),
                };
                this.send_cmd(&cmd, Act::Guild);
                let _ = cx;
            },
        ));
        col = col.child(ops);

        if !self.store.guild_log.is_empty() {
            col = col.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(lang, Key::GuildLogLbl)),
            );
            for entry in self.store.guild_log.iter().rev().take(5) {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", entry.text)),
                );
            }
        }
        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        col
    }

    // ── модалки (паритет egui-окон; вызываются из game_screen) ────────────

    /// Описание предмета (ФР-07).
    pub(crate) fn item_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        detail: serde_json::Value,
    ) -> Div {
        let lang = self.lang;
        let rarity = detail["rarity"].as_str().unwrap_or("common");
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .text_color(rgb(rarity_color(rarity, pal)))
                .child(format!(
                    "{rarity} · {} · +{}",
                    detail["type"].as_str().unwrap_or("?"),
                    detail["upgrade"].as_i64().unwrap_or(0)
                )),
        );
        if let Some(stats) = detail["stats"].as_object() {
            for (key, value) in stats {
                col = col.child(div().child(format!("{key}: {value}")));
            }
        }
        if let Some(sockets) = detail["sockets"].as_array() {
            for socket in sockets {
                if let Some(rune) = socket["rune"].as_object() {
                    col = col.child(div().child(format!(
                        "◈ {}: {}/{}",
                        rune["name"], rune["charges"], rune["max"]
                    )));
                } else {
                    col = col.child(div().text_color(rgb(pal.muted)).child("○ —"));
                }
            }
        }
        if let Some(value) = detail["gold_value"].as_i64() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("{value} зол.")),
            );
        }
        col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.item_detail = None;
                let _ = cx;
            },
        ))
    }

    /// Улучшение (ФР-10): «сейчас +N → после +M», шанс, стоимость (§7).
    pub(crate) fn upgrade_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        view: &UpgradeView,
    ) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Upgrade);
        let mut col = div().flex().flex_col().gap_1();
        let Some(preview) = view.preview.clone() else {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
            col = col.child(act_btn(
                cx,
                t(lang, Key::Exit),
                pal.panel,
                pal.text,
                |this, cx| {
                    this.store.close_upgrade();
                    let _ = cx;
                },
            ));
            return col;
        };

        let current = self
            .store
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.char
                    .inventory
                    .iter()
                    .find(|i| i.uid == view.item_uid)
                    .map(|i| i.upgrade)
            })
            .unwrap_or(0);
        let delta = preview["delta"]["upgrade"].as_str().unwrap_or("+?");
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("+{current} → {delta}")),
        );
        if let Some(chance) = preview["chance"].as_f64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {:.0}%",
                t(lang, Key::ChanceLabel),
                chance * 100.0
            )));
        }
        if let Some(gold) = preview["cost"]["gold"].as_i64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {} зол.",
                t(lang, Key::CostLabel),
                gold
            )));
        }
        if let Some(materials) = preview["cost"]["materials"].as_object() {
            for (name, qty) in materials {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("  • {name}: {qty}")),
                );
            }
        }
        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        let preview_id = preview["preview_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let can_start = !preview_id.is_empty() && !pending;
        let mut row = div().flex().flex_row().gap_2().mt(px(8.0));
        row = row.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.close_upgrade();
                let _ = cx;
            },
        ));
        let uid = view.item_uid.clone();
        if can_start {
            row = row.child(act_btn(
                cx,
                t(lang, Key::UpgradeBtn),
                pal.warn,
                0xffffff,
                move |this, cx| {
                    let cmd = UpgradeStart {
                        item_uid: uid.clone(),
                        preview_id: preview_id.clone(),
                    };
                    this.send_cmd(&cmd, Act::Upgrade);
                    let _ = cx;
                },
            ));
        } else {
            row = row.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(if pending {
                        t(lang, Key::Loading)
                    } else {
                        t(lang, Key::UpgradeBtn)
                    }),
            );
        }
        col = col.child(row);
        col
    }

    /// Инкрустация посоха (ФР-09).
    pub(crate) fn socket_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        staff_uid: String,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div().size_full();
        };
        let Some(staff) = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == staff_uid)
            .cloned()
        else {
            self.store.close_socketing();
            return div().size_full();
        };
        let runes: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| i.item_type == "rune")
            .cloned()
            .collect();

        let mut col = div().flex().flex_col().gap_1();
        for socket in staff.sockets.iter().flatten() {
            let mut row = div().flex().flex_row().gap_2().items_center();
            row = row.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("◈ {}", socket.index)),
            );
            match &socket.rune {
                Some(rune) => {
                    let index = socket.index;
                    row = row.child(
                        div().child(format!("{} ({}/{})", rune.name, rune.charges, rune.max)),
                    );
                    let suid = staff.uid.clone();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::ExtractRune),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            let cmd = SocketExtract {
                                staff_uid: suid.clone(),
                                socket_index: index,
                            };
                            this.send_cmd(&cmd, Act::Socket);
                            let _ = cx;
                        },
                    ));
                }
                None => {
                    if runes.is_empty() {
                        row = row.child(
                            div()
                                .text_color(rgb(pal.muted))
                                .child(t(lang, Key::NoRunes)),
                        );
                    }
                    for rune in &runes {
                        let index = socket.index;
                        let suid = staff.uid.clone();
                        let ruid = rune.uid.clone();
                        let label = format!("{}: {}", t(lang, Key::InsertRune), rune.name);
                        row =
                            row.child(act_btn(cx, label, pal.panel, pal.text, move |this, cx| {
                                let cmd = SocketInsert {
                                    staff_uid: suid.clone(),
                                    socket_index: index,
                                    rune_uid: ruid.clone(),
                                };
                                this.send_cmd(&cmd, Act::Socket);
                                let _ = cx;
                            }));
                    }
                }
            }
            col = col.child(row);
        }

        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        col = col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.close_socketing();
                let _ = cx;
            },
        ));
        col
    }
}

// ── вспомогательные ────────────────────────────────────────────────────────

/// Сводка целей квеста: «Волков убито 4/5» или текст цели.
fn goals_summary(q: &ld2_protocol::types::QuestState) -> String {
    q.goals
        .iter()
        .map(|g| match (g.cur, g.need) {
            (Some(cur), Some(need)) => format!("{} {cur}/{need}", g.text),
            _ => g.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Название профессии: l10n-ключ по id, фолбэк — сам id.
fn prof_label(lang: Lang, id: &str) -> String {
    let key = match id {
        "blacksmith" => Some(Key::ProfBlacksmith),
        "alchemist" => Some(Key::ProfAlchemist),
        "scribe" => Some(Key::ProfScribe),
        "woodcarver" => Some(Key::ProfWoodcarver),
        "tailor" => Some(Key::ProfTailor),
        "cook" => Some(Key::ProfCook),
        "herbalist" => Some(Key::ProfHerbalist),
        "hunter" => Some(Key::ProfHunter),
        _ => None,
    };
    key.map(|k| t(lang, k).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Бейджи плитки: +N, сокеты ◈○, количество ×N (паритет egui).
fn badges(item: &ItemSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    if item.upgrade > 0 {
        parts.push(format!("+{}", item.upgrade));
    }
    if let Some(sockets) = &item.sockets {
        let filled = sockets.iter().filter(|s| s.rune.is_some()).count();
        let empty = sockets.len() - filled;
        parts.push(format!("{}{}", "◈".repeat(filled), "○".repeat(empty)));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        parts.push(format!("×{qty}"));
    }
    parts.join(" ")
}

/// Редкость → цвет (палитра §7.4).
fn rarity_color(rarity: &str, pal: &Palette) -> u32 {
    match rarity {
        "common" => pal.muted,
        "uncommon" => 0x98_c3_79,
        "rare" => 0x61_af_ef,
        "epic" => 0xc6_78_dd,
        "legendary" => 0xe5_c0_7b,
        _ => pal.text,
    }
}

/// Слот экипировки для типа предмета (гейт клиента; сервер — истина, §7).
fn slot_for(item_type: &str) -> Option<&'static str> {
    match item_type {
        "staff" => Some("staff"),
        "weapon" => Some("main_hand"),
        "armor" => Some("body"),
        _ => None,
    }
}

/// Цвет severity (палитра §7.4).
fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Полоса-прогресс (паритет egui ProgressBar).
pub(crate) fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
                .h(px(10.0))
                .rounded_md()
                .bg(rgb(pal.border))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac.clamp(0.0, 1.0)))
                        .rounded_md()
                        .bg(rgb(fill)),
                ),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(label),
        )
}
