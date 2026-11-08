//! Игровой экран gpui (M10.4) — порт `ui/world.rs` egui-клиента:
//! левая колонка (цель/ресурсы/история), центр (локация, сущности с
//! контекстным меню, hotbar, журнал), модалки смерти/осмотра/лута.
//!
//! Правая панель-лист (sheet, 7 вкладок) и соц-окна (пати/сделка) — M10.4b/M10.5.
//! Отличия от egui: модалки заменяют контент (шапка остаётся), контекстное
//! меню — инлайн под строкой (вместо попапа по курсору), тултипы не используются.

use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, MouseButton, Stateful};
use ld2_core::l10n::{format_duration, t, tf, Key};
use ld2_core::store::{crime_kind_title, filter_active_statuses, local_epoch, AppStore, LogKind};
use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::{Entity, EntityKind, LocationSnapshot, Severity};

use crate::palette::Palette;
use ely::forms::Choice;
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::Tooltip;

use super::social::show_party;
use super::{act_btn, btn, Click, LogTab, Root};

/// M14/F4: «Мощный удар» открывается владением мечом 5 (skills.md §4;
/// серверный гейт — конфиг `skills.active.power_strike` → `E_REQUIREMENT_ATTR`).
/// Клиентское зеркало гейта: кнопка и хоткей показываются/срабатывают
/// только когда навык открыт.
pub(crate) fn power_strike_open(skills: &[ld2_protocol::types::SkillSnapshot]) -> bool {
    skills
        .iter()
        .find(|s| s.id == "sword")
        .is_some_and(|s| s.rank >= 5)
}

impl Root {
    /// M14/F4: первая доступная руна посоха (хоткей «руна») — заряды и
    /// мана в порядке, как у первой включённой кнопки рун на панели.
    pub(crate) fn first_rune_click(&self) -> Option<Click> {
        let snap = self.store.snapshot.as_ref()?;
        let rune_cost = self
            .store
            .session
            .as_ref()
            .map(|s| s.magic.mana.rune_cost)
            .unwrap_or(0);
        let staff_uid = snap.char.equip.get("staff").cloned().flatten()?;
        let staff = snap.char.inventory.iter().find(|i| i.uid == staff_uid)?;
        staff
            .sockets
            .iter()
            .flatten()
            .find(|socket| {
                socket.rune.as_ref().is_some_and(|r| r.charges > 0)
                    && (rune_cost <= 0 || snap.char.mp.cur >= rune_cost)
            })
            .map(|socket| Click::RuneCast {
                staff_uid: staff_uid.clone(),
                socket_index: socket.index,
            })
    }

    /// M14/F4: первый расходник боя (свиток/зелье) — хоткей «6».
    pub(crate) fn first_consumable_uid(&self) -> Option<String> {
        self.store
            .snapshot
            .as_ref()?
            .char
            .inventory
            .iter()
            .find(|i| matches!(i.item_type.as_str(), "scroll" | "potion"))
            .map(|i| i.uid.clone())
    }

    /// M11.2: игровые всплывающие диалоги (ФР-03/05/06): death/examine/loot.
    /// Приоритет — прежний диспетчер замены контента (смерть главнее).
    pub(crate) fn game_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if let Some(death) = self.store.death.clone() {
            let title = t(self.lang, Key::DeathTitle).to_string();
            let body = self.death_view(cx, pal, &death).into_any_element();
            // ФР-06: закрытие только подтверждением (акр сервера) — Escape и
            // клик по скриму ничего не делают: on_close пуст.
            return Some(
                Dialog::new("ld2-death", title, |_window, _cx| {})
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(detail) = self.store.entity_detail.clone() {
            let name = detail["entity"]["name"].as_str().unwrap_or("?").to_string();
            let body = self.examine_view(cx, pal, detail).into_any_element();
            let close = self.dismiss(cx, |r| r.store.entity_detail = None);
            return Some(
                Dialog::new("ld2-examine", name, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some((uid, name)) = self.store.loot_confirm.clone() {
            let title = t(self.lang, Key::TakeLoot).to_string();
            let body = self.loot_view(cx, pal, uid, name).into_any_element();
            let close = self.dismiss(cx, |r| r.store.clear_loot_confirm());
            return Some(
                Dialog::new("ld2-loot", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    pub(crate) fn game_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(t(self.lang, Key::Loading));
        };

        div()
            .flex()
            .flex_row()
            .gap_3()
            .size_full()
            .p(px(12.0))
            .child(self.left_column(cx, pal, &snap))
            .child(self.sheet(cx, pal))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_grow_1()
                    // M14/F2: описание локации — всегда первая строка.
                    .child(self.location_block(pal, &snap.location))
                    // Таргет — под описанием (перенос из левой колонки).
                    .child(self.target_block(cx, pal, &snap))
                    .children(self.notice_line(pal))
                    .child(self.entities_block(cx, pal, &snap.location))
                    .child(self.hotbar(cx, pal, &snap))
                    .child(self.exits_row(cx, pal, &snap.location))
                    .children((self.store.target.is_some()).then(|| {
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(self.lang, Key::AltNavHint))
                    }))
                    // Журнал/чат — последним (M14/F2/F7).
                    .child(self.journal_block(cx, pal)),
            )
    }

    // ── левая колонка: цель, ресурсы, история ─────────────────────────────

    fn left_column(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
        let lang = self.lang;
        let mut col = div().flex().flex_col().gap_2().w(px(210.0));

        // Имя + бейджи статусов (ФР-11): серверные сроки, now = лок + offset.
        let mut name_row = div().flex().flex_row().items_center().gap_2().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(16.0))
                .child(snap.char.name.clone()),
        );
        let now = local_epoch() + self.store.time_offset;
        for status in filter_active_statuses(&snap.char.crime.statuses, now) {
            let title = crime_kind_title(&self.store, &status.kind);
            let left = status.expires_at - now;
            name_row = name_row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(severity_rgb(status.severity)))
                    .child(format!("{title} · {}", format_duration(lang, left))),
            );
        }
        col = col.child(name_row);

        // Ресурсы (ФР-04/09).
        col = col.child(bar(
            snap.char.hp.cur as f32 / snap.char.hp.max.max(1) as f32,
            format!(
                "{} {}/{}",
                t(lang, Key::HpLabel),
                snap.char.hp.cur,
                snap.char.hp.max
            ),
            pal.success,
            pal,
        ));
        col = col.child(bar(
            snap.char.mp.cur as f32 / snap.char.mp.max.max(1) as f32,
            format!(
                "{} {}/{}",
                t(lang, Key::MpLabel),
                snap.char.mp.cur,
                snap.char.mp.max
            ),
            pal.accent,
            pal,
        ));
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!(
                    "{}: {}",
                    t(lang, Key::XpPoolLabel),
                    snap.char.xp_pool
                )),
        );

        // M14/F1: «Сменить персонажа»/«Выйти» переехали в меню шапки.

        // M14/F2: окно группы — только при участниках ≥2 (инвайт сам
        // создаёт группу — F5, но до принятия она пустая).
        if self
            .store
            .party
            .as_ref()
            .is_some_and(|p| show_party(&p.members))
        {
            col = col.child(self.party_widget(cx, pal));
        }

        // История посещений (ФР-03). Растягивается на остаток высоты и
        // срезается по ней (overflow_hidden): при группе ≥2 и 10 пунктах
        // на минимуме окна (960×640) список не должен выталкивать чат
        // за нижнюю границу окна.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(13.0))
                .child(t(lang, Key::HistoryLabel)),
        );
        let mut visited_list = div().flex().flex_col().gap_1();
        for visited in self.store.visited.iter().rev().take(10) {
            visited_list = visited_list.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(visited.name.clone()),
            );
        }
        col = col.child(
            div()
                .flex_grow_1()
                .min_h(px(32.0))
                .overflow_hidden()
                .child(visited_list),
        );

        // M14/F7: чат переехал в блок журнала (вкладка «Чат»); место
        // под него занимает история посещений (flex_grow_1 выше).
        col
    }

    /// Блок таргета (M14/F2: перенесён из левой колонки — под описание
    /// локации): имя, HP и кнопки «Атака»/«Побег».
    fn target_block(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
        let lang = self.lang;
        let Some(target_id) = self.store.target.clone() else {
            return div();
        };
        let Some(entity) = snap.location.entities.iter().find(|e| e.id == target_id) else {
            return div();
        };
        let name = entity.name.clone();
        let id = entity.id.clone();
        let hp = entity.hp;
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
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{}: {name}", t(lang, Key::TargetLabel))),
            );
        if let Some(hp) = hp {
            block = block.child(bar(
                hp.cur as f32 / hp.max.max(1) as f32,
                format!("{}/{}", hp.cur, hp.max),
                pal.danger,
                pal,
            ));
        }
        block = block.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(btn(
                    cx,
                    t(lang, Key::Attack).to_string(),
                    pal.danger,
                    0xffffff,
                    Click::Attack(id.clone()),
                ))
                .child(btn(
                    cx,
                    t(lang, Key::BtnFlee).to_string(),
                    pal.panel,
                    pal.text,
                    Click::Flee,
                )),
        );
        block
    }

    // ── центр: локация, выходы, сущности ──────────────────────────────────

    fn location_block(&mut self, pal: &Palette, loc: &LocationSnapshot) -> Div {
        let mut head = div().flex().flex_row().gap_2().items_center().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(16.0))
                .child(loc.name.clone()),
        );
        for flag in &loc.flags {
            let (text, color) = match flag.as_str() {
                "pvp" => ("PvP", pal.warn),
                "safe" => ("safe", pal.success),
                other => (other, pal.muted),
            };
            head = head.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(color))
                    .child(text.to_string()),
            );
        }
        head.child(
            div()
                .id("location_text")
                .flex_grow_1()
                .h(px(110.0))
                .overflow_y_scroll()
                .px_2()
                .rounded_md()
                .bg(rgb(pal.panel))
                .border_1()
                .border_color(rgb(pal.border))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(pal.text))
                        .child(loc.text.clone()),
                ),
        )
    }

    /// Тосты мира (world_error/notice) — как в egui-панели локации.
    fn notice_line(&self, pal: &Palette) -> Option<Div> {
        if let Some(err) = &self.store.world_error {
            return Some(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        if let Some(notice) = &self.store.notice {
            return Some(div().text_color(rgb(pal.warn)).child(notice.clone()));
        }
        None
    }

    fn exits_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        loc: &LocationSnapshot,
    ) -> Div {
        let lang = self.lang;
        let mut row = div()
            .flex()
            .flex_row()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{}:", t(lang, Key::ExitsLabel))),
            );
        if loc.exits.is_empty() {
            row = row.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoExits)),
            );
        }
        for exit in &loc.exits {
            let id = exit.id.clone();
            row = row.child(btn(
                cx,
                format!("→ {}", exit.label),
                pal.panel,
                pal.accent,
                Click::MoveExit(id),
            ));
        }
        row
    }

    fn entities_block(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        loc: &LocationSnapshot,
    ) -> Div {
        let safe = loc.flags.iter().any(|f| f == "safe");
        let mut block = div().flex().flex_col().gap_1();
        for (title, kind) in [
            (Key::PlayersLabel, EntityKind::Player),
            (Key::MonstersLabel, EntityKind::Monster),
            (Key::NpcsLabel, EntityKind::Npc),
            (Key::CorpsesLabel, EntityKind::Corpse),
        ] {
            let group: Vec<Entity> = loc
                .entities
                .iter()
                .filter(|e| e.kind == kind)
                .cloned()
                .collect();
            if group.is_empty() {
                continue;
            }
            block = block.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(self.lang, title)),
            );
            for entity in group {
                block = block.child(self.entity_row(cx, pal, entity, safe));
            }
        }
        block
    }

    /// Строка сущности: левый клик — цель, ПКМ — действия (ФР-03/05).
    fn entity_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        entity: Entity,
        safe: bool,
    ) -> AnyElement {
        let lang = self.lang;
        let player_corpse = entity.kind == EntityKind::Corpse && entity.was_player.unwrap_or(false);
        let selected = self.store.target.as_deref() == Some(entity.id.as_str());
        let id = entity.id.clone();

        let mut row = div()
            .id(format!("ld2-ent-{}", entity.id))
            .tooltip(Tooltip::text(entity_tooltip(&entity)))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(if selected { pal.selection } else { pal.panel }))
            .border_1()
            .border_color(rgb(pal.border))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(pal.selection)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        if matches!(
                            this.store
                                .snapshot
                                .as_ref()
                                .and_then(|s| { s.location.entities.iter().find(|e| e.id == id) })
                                .map(|e| e.kind),
                            Some(EntityKind::Monster) | Some(EntityKind::Player)
                        ) {
                            this.store.select_target(id.clone());
                        }
                        cx.notify();
                    }
                }),
            );

        if player_corpse {
            row = row.child(div().text_color(rgb(pal.warn)).child("⚠"));
        }
        row = row.child(div().flex_grow_1().child(entity.name.clone()));
        if let Some(hp) = &entity.hp {
            row = row.child(bar(
                hp.cur as f32 / hp.max.max(1) as f32,
                format!("{}/{}", hp.cur, hp.max),
                pal.danger,
                pal,
            ));
        }
        for tag in entity.tags.iter().flatten() {
            row = row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.warn))
                    .child(tag.clone()),
            );
        }

        // M11.4: правый клик — контекстное меню Ely (попап по курсору;
        // паритет egui context_menu, инлайн-раскрытие M10 убрано).
        let attackable = !safe && matches!(entity.kind, EntityKind::Monster | EntityKind::Player);
        let this = cx.entity();
        let mk = |label: String, click: Click| {
            let this = this.clone();
            MenuItem::new(label).on_click(move |_window, acx| {
                this.update(acx, |root, cx| root.on_click(click.clone(), cx));
            })
        };
        let mut menu = Menu::new().item(mk(
            t(lang, Key::Examine).to_string(),
            Click::Examine(id.clone()),
        ));
        if attackable {
            menu = menu.item(mk(
                t(lang, Key::Attack).to_string(),
                Click::Attack(id.clone()),
            ));
        }
        if entity.kind == EntityKind::Player {
            menu = menu.item(mk(
                t(lang, Key::PartyInviteBtn).to_string(),
                Click::PartyInvite(id.clone()),
            ));
            menu = menu.item(mk(
                t(lang, Key::TradeBtn).to_string(),
                Click::TradeOpen(id.clone()),
            ));
        }
        if player_corpse {
            menu = menu.item(MenuItem::new(t(lang, Key::CorpsePlayerHint)).disabled(true));
        }
        ContextMenu::new(format!("ld2-ctx-{}", entity.id), menu)
            .child(row)
            .into_any_element()
    }

    // ── hotbar (ФР-05/09) ─────────────────────────────────────────────────

    /// M14/F4: панель действий (бывший hotbar): атака → активки →
    /// побег → руны → свитки/зелья. Каждая кнопка учитывает собственный
    /// кулдаун и глобальный (приоритет — max) и показывает причину
    /// недоступности в тултипе.
    fn hotbar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette, snap: &StateSnapshot) -> Div {
        let lang = self.lang;
        let rune_cost = self
            .store
            .session
            .as_ref()
            .map(|s| s.magic.mana.rune_cost)
            .unwrap_or(0);
        let now_ms = self.now_ms();
        let global_left = self.store.global_cooldown_left(now_ms);
        // Готовность действия: max(собственный кулдаун, глобальный).
        let ready = |own_left: i64| AppStore::action_ready(own_left, global_left);

        let mut row = div()
            .flex()
            .flex_row()
            .gap_2()
            .items_center()
            .flex_wrap()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, Key::HotbarLabel)),
            );

        // ── 1) Обычная атака — всегда первая; без цели — disabled ──
        let attack_label = t(lang, Key::Attack).to_string();
        match self.store.target.clone() {
            None => {
                row = row.child(action_disabled(
                    "ld2-act-attack",
                    attack_label,
                    t(lang, Key::HotbarNoTarget).to_string(),
                    pal,
                ))
            }
            Some(id) => {
                let (ok, left) = ready(0);
                if ok {
                    row = row.child(btn(
                        cx,
                        attack_label,
                        pal.danger,
                        0xffffff,
                        Click::Attack(id),
                    ));
                } else {
                    row = row.child(cd_disabled("ld2-act-attack", attack_label, left, lang, pal));
                }
            }
        }

        // ── 2) Активки: «Мощный удар» (гейт §4); Раскол скрыт ──
        if power_strike_open(&snap.char.skills) {
            let label = t(lang, Key::PowerStrike).to_string();
            let (ok, left) = ready(self.own_cd_left_ms("power_strike", now_ms));
            if ok {
                row = row.child(btn(cx, label, pal.warn, 0xffffff, Click::PowerStrike));
            } else {
                row = row.child(cd_disabled("ld2-act-power-strike", label, left, lang, pal));
            }
        }

        // ── 3) Побег ──
        let flee_label = t(lang, Key::BtnFlee).to_string();
        let (ok, left) = ready(0);
        if ok {
            row = row.child(btn(cx, flee_label, pal.panel, pal.text, Click::Flee));
        } else {
            row = row.child(cd_disabled("ld2-act-flee", flee_label, left, lang, pal));
        }

        // ── 4) Руны экипированного посоха: ×N зарядов (ФР-09) ──
        if let Some(staff_uid) = snap.char.equip.get("staff").cloned().flatten() {
            if let Some(staff) = snap.char.inventory.iter().find(|i| i.uid == staff_uid) {
                for socket in staff.sockets.iter().flatten() {
                    let Some(rune) = &socket.rune else { continue };
                    let charges_ok = rune.charges > 0;
                    let mp_ok = rune_cost <= 0 || snap.char.mp.cur >= rune_cost;
                    let label = format!("{} ×{}", rune.name, rune.charges);
                    // Причина недоступности: сначала требования
                    // (заряды/мана), потом кулдаун (у кулдауна — таймер
                    // и в label, как у остальных кнопок панели).
                    let off: Option<OffReason> = if !charges_ok {
                        Some(OffReason::Req(t(lang, Key::NoCharge).to_string()))
                    } else if !mp_ok {
                        Some(OffReason::Req(t(lang, Key::NotEnoughMp).to_string()))
                    } else {
                        let (ok, left) = ready(0);
                        (!ok).then_some(OffReason::Cd(left))
                    };
                    match off {
                        None => {
                            let slot = btn(
                                cx,
                                label,
                                pal.accent,
                                pal.accent_text,
                                Click::RuneCast {
                                    staff_uid: staff_uid.clone(),
                                    socket_index: socket.index,
                                },
                            );
                            // M11.4: стоимость каста — тултип (egui-паритет).
                            row = row.child(if rune_cost > 0 {
                                slot.id(format!("ld2-rune-{}", socket.index))
                                    .tooltip(Tooltip::text(format!("{rune_cost} MP")))
                                    .into_any_element()
                            } else {
                                slot.into_any_element()
                            });
                        }
                        Some(OffReason::Req(reason)) => {
                            row = row.child(action_disabled(
                                &format!("ld2-rune-off-{}", socket.index),
                                label,
                                reason,
                                pal,
                            ));
                        }
                        Some(OffReason::Cd(left)) => {
                            row = row.child(cd_disabled(
                                &format!("ld2-rune-off-{}", socket.index),
                                label,
                                left,
                                lang,
                                pal,
                            ));
                        }
                    }
                }
            }
        }

        // ── 5) Свитки и зелья (свиток — без маны, D4) ──
        let consumables: Vec<_> = snap
            .char
            .inventory
            .iter()
            .filter(|i| matches!(i.item_type.as_str(), "scroll" | "potion"))
            .cloned()
            .collect();
        for item in consumables {
            let label = format!("{} ×{}", item.name, item.qty.unwrap_or(1));
            let (ok, left) = ready(0);
            if ok {
                row = row.child(btn(
                    cx,
                    label,
                    pal.panel,
                    pal.text,
                    Click::UseItem(item.uid.clone()),
                ));
            } else {
                row = row.child(cd_disabled(
                    &format!("ld2-act-item-{}", item.uid),
                    label,
                    left,
                    lang,
                    pal,
                ));
            }
        }
        row
    }

    // ── журнал мира (ФР-17) ───────────────────────────────────────────────

    /// M14/F7: блок журнала внизу центра — две вкладки **Лог | Чат**.
    /// Лог прокручивается свободно: приклеивание вниз только когда
    /// пользователь и так внизу ([`scroll_at_bottom`]); иначе новые
    /// строки копятся и показываются кнопкой «▼ N новых» в шапке.
    fn journal_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let dark = pal.bg == crate::palette::palette("dark").bg;
        let len = self.store.log.len();

        // Состояние прокрутки (вычисляется каждый тик рендера).
        let at_bottom = scroll_at_bottom(
            f32::from(self.journal_scroll.offset().y),
            f32::from(self.journal_scroll.max_offset().y),
        );
        if at_bottom {
            // Приклеиваемся только при новых строках — и только если
            // мы и так внизу (иначе позицию пользователя не дёргаем).
            if self.journal_seen != len && len > 0 {
                self.journal_scroll.scroll_to_item(len - 1);
            }
            self.journal_seen = len;
            self.journal_unseen = 0;
        } else {
            self.journal_unseen = len.saturating_sub(self.journal_seen);
        }
        let unseen = self.journal_unseen;

        // Шапка: вкладки «Журнал»/«Чат» + счётчик строк + индикатор.
        let active = match self.log_tab {
            LogTab::Log => "log",
            LogTab::Chat => "chat",
        };
        let choices = vec![
            Choice::new("log", t(lang, Key::LogLabel)),
            Choice::new("chat", t(lang, Key::ChLabel)),
        ];
        let this = cx.entity();
        let mut head = div().flex().flex_row().items_center().gap_2();
        head = head.child(Tabs::new("ld2-journal-tabs", choices, active).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                this.update(acx, |root, cx| {
                    root.log_tab = if value == "chat" {
                        LogTab::Chat
                    } else {
                        LogTab::Log
                    };
                    cx.notify();
                });
            },
        ));
        head = head.child(div().flex_grow_1());
        head = head.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(len.to_string()),
        );
        if unseen > 0 {
            let label = tf(lang, Key::LogNewLines, &[("{n}", &unseen.to_string())]);
            head = head.child(act_btn(
                cx,
                label,
                pal.accent,
                pal.accent_text,
                |this, cx| {
                    let len = this.store.log.len();
                    if len > 0 {
                        this.journal_scroll.scroll_to_item(len - 1);
                    }
                    this.journal_seen = len;
                    this.journal_unseen = 0;
                    cx.notify();
                },
            ));
        }

        // Тело активной вкладки.
        let body =
            match self.log_tab {
                LogTab::Log => div().flex_grow_1().min_h_0().child(
                    ScrollArea::new("ld2-log-scroll")
                        .vertical()
                        .track_scroll(&self.journal_scroll)
                        .child(div().flex().flex_col().gap_1().children(
                            self.store.log.iter().map(|entry| {
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(log_color(entry.kind, dark)))
                                    .child(entry.text.clone())
                            }),
                        )),
                ),
                LogTab::Chat => self.chat_body(cx, pal),
            };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            // Блок занимает остаток высоты центра (M14/F7).
            .flex_grow_1()
            .min_h(px(180.0))
            .child(head)
            .child(body)
    }

    // ── модалки (паритет egui-окон) ───────────────────────────────────────

    fn death_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        death: &ld2_protocol::evt::CharDeath,
    ) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::DeathLost)),
            )
            .children(death.lost_items.iter().map(|item| {
                div().child(format!(
                    "• {} ({})",
                    item.name,
                    item.rarity.as_deref().unwrap_or("-")
                ))
            }))
            .child(
                btn(
                    cx,
                    t(lang, Key::DeathConfirm).to_string(),
                    pal.danger,
                    0xffffff,
                    Click::ConfirmDeath,
                )
                .mt(px(10.0)),
            )
    }

    fn examine_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        detail: serde_json::Value,
    ) -> Div {
        let lang = self.lang;
        let kind = detail["entity"]["kind"].as_str().unwrap_or("?").to_string();
        let was_player = detail["was_player"] == true;
        let looted = detail["looted"] == true;
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_color(rgb(pal.muted)).child(kind));
        if let Some(hp) = detail["entity"]["hp"].as_object() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("HP: {}/{}", hp["cur"], hp["max"])),
            );
        }
        if was_player {
            col = col.child(
                div()
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::CorpsePlayerHint)),
            );
        }
        if let Some(loot) = detail["loot"].as_array() {
            if !loot.is_empty() {
                col = col.child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(t(lang, Key::LootLabel)),
                );
                for item in loot {
                    let uid = item["uid"].as_str().unwrap_or_default().to_string();
                    let iname = item["name"].as_str().unwrap_or("?").to_string();
                    let itype = item["type"].as_str().unwrap_or("?");
                    let mut line = div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(format!("• {iname} ({itype})"));
                    if !looted {
                        let uid2 = uid.clone();
                        let iname2 = iname.clone();
                        // Лут трупа ИГРОКА — подтверждение о статусе (ФР-11).
                        let player_corpse = was_player;
                        line = line.child(btn(
                            cx,
                            t(lang, Key::TakeLoot).to_string(),
                            pal.panel,
                            pal.accent,
                            if player_corpse {
                                Click::LootTakeMarker(uid2, iname2)
                            } else {
                                Click::UseItem(uid2)
                            },
                        ));
                    }
                    col = col.child(line);
                }
            }
        }
        if let Some(err) = self
            .store
            .world_error
            .clone()
            .or_else(|| self.store.sheet_error.clone())
        {
            col = col.child(div().text_color(rgb(pal.danger)).child(err));
        }
        col.child(btn(
            cx,
            t(lang, Key::Exit).to_string(),
            pal.panel,
            pal.text,
            Click::CloseDetail,
        ))
    }

    fn loot_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        _uid: String,
        name: String,
    ) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::LootPlayerWarn)),
            )
            .child(name)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .mt(px(8.0))
                    .child(btn(
                        cx,
                        t(lang, Key::Exit).to_string(),
                        pal.panel,
                        pal.text,
                        Click::LootCancel,
                    ))
                    .child(btn(
                        cx,
                        t(lang, Key::TakeLoot).to_string(),
                        pal.danger,
                        0xffffff,
                        Click::LootTake,
                    )),
            )
    }
}

/// Полоса-прогресс (паритет egui ProgressBar).
fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
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

/// M14/F7: лог «прибит» к нижней позиции — offset достиг максимума
/// (допуск 1 px на антиалиасинг/дребезг layout). Только при `true`
/// разрешено stick-to-bottom; иначе позицию пользователя не трогаем.
/// Чистая функция — юнит-тест в конце файла.
fn scroll_at_bottom(offset_y: f32, max_offset_y: f32) -> bool {
    offset_y + 1.0 >= max_offset_y
}

/// M14/F4: почему кнопка панели недоступна: требование (цель/мана/
/// заряды — текст в тултипе) или кулдаун (таймер на кнопке + тултип).
enum OffReason {
    Req(String),
    Cd(i64),
}

/// M14/F4: недоступная кнопка панели — причина в тултипе (тултипы
/// используются и для включённых кнопок — egui-паритет M11.4).
fn action_disabled(id: &str, label: String, reason: String, pal: &Palette) -> Stateful<Div> {
    div()
        .id(id.to_string())
        .tooltip(Tooltip::text(reason))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.border))
        .text_color(rgb(pal.muted))
        .child(label)
}

/// M14/F4: текст причины «кулдаун» — остаток округляется вверх
/// (100 мс до конца → «1 с»), чтобы значение не мигало в 0.
fn cooldown_reason(lang: ld2_core::l10n::Lang, left_ms: i64) -> String {
    let secs = ((left_ms.max(0) + 999) / 1000).max(1);
    tf(lang, Key::HotbarCooldown, &[("{s}", &secs.to_string())])
}

/// M14/F4: кнопка на кулдауне (собственный/глобальный): таймер остатка
/// прямо на кнопке (перерисовка тиком 50 мс) + причина в тултипе.
fn cd_disabled(
    id: &str,
    label: String,
    left_ms: i64,
    lang: ld2_core::l10n::Lang,
    pal: &Palette,
) -> Stateful<Div> {
    let secs = ((left_ms.max(0) + 999) / 1000).max(1);
    let short = tf(lang, Key::HotbarCdShort, &[("{s}", &secs.to_string())]);
    action_disabled(
        id,
        format!("{label} · {short}"),
        cooldown_reason(lang, left_ms),
        pal,
    )
}

/// Цвет severity (палитра §7.4, локальная копия — egui-версия возвращает Color32).
fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Цвет строки журнала (палитра §7.4; светлая тема — затемнение светлых,
/// паритет M9.2 egui-клиента).
fn log_color(kind: LogKind, dark: bool) -> u32 {
    use LogKind::*;
    if !dark {
        return match kind {
            System => 0x5a_5e_66,
            Location => 0x4a_4e_58,
            Chat => 0x2a_2a_2e,
            Xp => 0x2e_7d_91,
            Loot => 0x8a_6d_1f,
            Crime => 0xa5_5a_22,
            _ => log_color(kind, true),
        };
    }
    match kind {
        System => 0x7f_84_8e,
        Location => 0xab_b2_bf,
        CombatOut => 0xe0_6c_75,
        CombatIn => 0x98_c3_79,
        Magic => 0x61_af_ef,
        Loot => 0xe5_c0_7b,
        Crime => 0xd1_9a_66,
        Quest => 0xc6_78_dd,
        Xp => 0x56_b6_c2,
        Chat => 0xeb_eb_eb,
    }
}

/// Подсказка строки сущности: HP и теги (имя — уже в строке).
fn entity_tooltip(entity: &Entity) -> String {
    let hp = entity
        .hp
        .map(|hp| format!(" · {}/{}", hp.cur, hp.max))
        .unwrap_or_default();
    let tags = entity
        .tags
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if tags.is_empty() {
        format!("{}{hp}", entity.name)
    } else {
        format!("{}{hp} · {tags}", entity.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M14/F7: stick-to-bottom разрешён только в нижней позиции.
    #[test]
    fn log_pins_to_bottom_only_when_scrolled_to_end() {
        // Пустой лог: 0 == 0 — считаем «внизу».
        assert!(scroll_at_bottom(0.0, 0.0));
        // Контент влезает без прокрутки — тоже «внизу».
        assert!(scroll_at_bottom(40.0, 40.0));
        // Погрешность layout в 1 px не считается уходом вверх.
        assert!(scroll_at_bottom(99.5, 100.0));
        // Пользователь прокрутил вверх — приклеивать нельзя.
        assert!(!scroll_at_bottom(10.0, 100.0));
        assert!(!scroll_at_bottom(0.0, 40.0));
    }
}
