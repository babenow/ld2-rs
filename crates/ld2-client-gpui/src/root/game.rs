//! Игровой экран gpui (M10.4) — порт `ui/world.rs` egui-клиента:
//! левая колонка (цель/ресурсы/история), центр (локация, сущности с
//! контекстным меню, hotbar, журнал), модалки смерти/осмотра/лута.
//!
//! Правая панель-лист (sheet, 7 вкладок) и соц-окна (пати/сделка) — M10.4b/M10.5.
//! Отличия от egui: модалки заменяют контент (шапка остаётся), контекстное
//! меню — инлайн под строкой (вместо попапа по курсору), тултипы не используются.

use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, MouseButton};
use ld2_core::l10n::{format_duration, t, Key};
use ld2_core::store::{crime_kind_title, filter_active_statuses, local_epoch, LogKind};
use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::{Entity, EntityKind, LocationSnapshot, Severity};

use crate::palette::Palette;

use super::{btn, Click, Root};

impl Root {
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
                    .child(self.location_block(pal, &snap.location))
                    .children(self.notice_line(pal))
                    .child(self.exits_row(cx, pal, &snap.location))
                    .children((self.store.target.is_some()).then(|| {
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(self.lang, Key::AltNavHint))
                    }))
                    .child(self.entities_block(cx, pal, &snap.location))
                    .child(self.hotbar(cx, pal, &snap))
                    .child(self.chat_panel(cx, pal))
                    .child(self.log_block(pal)),
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

        // Виджет группы (ФР-14) — сверху слева, как в egui.
        col = col.child(self.party_widget(cx, pal));

        // Цель (ФР-05): HP + атака/отступ.
        if let Some(target_id) = self.store.target.clone() {
            if let Some(entity) = snap.location.entities.iter().find(|e| e.id == target_id) {
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
                col = col.child(block);
            }
        }

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

        col = col.child(btn(
            cx,
            t(lang, Key::BtnSwitchChar).to_string(),
            pal.panel,
            pal.text,
            Click::SwitchChar,
        ));
        col = col.child(btn(
            cx,
            t(lang, Key::BtnLogout).to_string(),
            pal.panel,
            pal.danger,
            Click::Logout,
        ));

        // История посещений (ФР-03).
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(13.0))
                .child(t(lang, Key::HistoryLabel)),
        );
        for visited in self.store.visited.iter().rev().take(10) {
            col = col.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(visited.name.clone()),
            );
        }
        col
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
    ) -> Div {
        let lang = self.lang;
        let player_corpse = entity.kind == EntityKind::Corpse && entity.was_player.unwrap_or(false);
        let selected = self.store.target.as_deref() == Some(entity.id.as_str());
        let menu_open = self.ctx_menu.as_deref() == Some(entity.id.as_str());
        let id = entity.id.clone();

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(if selected || menu_open {
                pal.selection
            } else {
                pal.panel
            }))
            .border_1()
            .border_color(rgb(pal.border))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(pal.selection)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.ctx_menu = None;
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
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        // ПКМ — меню действий (паритет egui context_menu).
                        this.ctx_menu = Some(id.clone());
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

        if !menu_open {
            return row;
        }

        // Меню действий (инлайн под строкой; egui — попап по курсору).
        let attackable = !safe && matches!(entity.kind, EntityKind::Monster | EntityKind::Player);
        let mut menu = div()
            .flex()
            .flex_col()
            .gap_1()
            .ml(px(16.0))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.accent));
        menu = menu.child(btn(
            cx,
            t(lang, Key::Examine).to_string(),
            pal.panel,
            pal.text,
            Click::Examine(id.clone()),
        ));
        if attackable {
            menu = menu.child(btn(
                cx,
                t(lang, Key::Attack).to_string(),
                pal.panel,
                pal.danger,
                Click::Attack(id.clone()),
            ));
        }
        if entity.kind == EntityKind::Player {
            menu = menu.child(btn(
                cx,
                t(lang, Key::PartyInviteBtn).to_string(),
                pal.panel,
                pal.text,
                Click::PartyInvite(id.clone()),
            ));
            menu = menu.child(btn(
                cx,
                t(lang, Key::TradeBtn).to_string(),
                pal.panel,
                pal.text,
                Click::TradeOpen(id.clone()),
            ));
        }
        if player_corpse {
            menu = menu.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::CorpsePlayerHint)),
            );
        }
        div().flex().flex_col().child(row).child(menu)
    }

    // ── hotbar (ФР-05/09) ─────────────────────────────────────────────────

    fn hotbar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette, snap: &StateSnapshot) -> Div {
        let lang = self.lang;
        let rune_cost = self
            .store
            .session
            .as_ref()
            .map(|s| s.magic.mana.rune_cost)
            .unwrap_or(0);

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

        // Руны экипированного посоха: ×N зарядов (ФР-09).
        if let Some(staff_uid) = snap.char.equip.get("staff").cloned().flatten() {
            if let Some(staff) = snap.char.inventory.iter().find(|i| i.uid == staff_uid) {
                for socket in staff.sockets.iter().flatten() {
                    let Some(rune) = &socket.rune else { continue };
                    let charges_ok = rune.charges > 0;
                    let mp_ok = rune_cost <= 0 || snap.char.mp.cur >= rune_cost;
                    let enabled = charges_ok && mp_ok;
                    let label = format!("{} ×{}", rune.name, rune.charges);
                    if enabled {
                        row = row.child(btn(
                            cx,
                            label,
                            pal.accent,
                            pal.accent_text,
                            Click::RuneCast {
                                staff_uid: staff_uid.clone(),
                                socket_index: socket.index,
                            },
                        ));
                    } else {
                        // Причина недоступности — в тексте (тултипы egui не
                        // портились, см. отклонения M10.4).
                        let reason = if !charges_ok {
                            t(lang, Key::NoCharge)
                        } else {
                            t(lang, Key::NotEnoughMp)
                        };
                        row = row.child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(pal.border))
                                .text_color(rgb(pal.muted))
                                .child(format!("{label} · {reason}")),
                        );
                    }
                }
            }
        }

        // Свитки и зелья (свиток — без маны, D4).
        let consumables: Vec<_> = snap
            .char
            .inventory
            .iter()
            .filter(|i| matches!(i.item_type.as_str(), "scroll" | "potion"))
            .cloned()
            .collect();
        for item in consumables {
            let label = format!("{} ×{}", item.name, item.qty.unwrap_or(1));
            row = row.child(btn(
                cx,
                label,
                pal.panel,
                pal.text,
                Click::UseItem(item.uid.clone()),
            ));
        }

        // «Мощный удар» (ФР-05): только кулдаун (D4).
        let now = local_epoch() + self.store.time_offset;
        let cd_left = self
            .store
            .combat_cooldowns
            .get("power_strike")
            .copied()
            .unwrap_or(0)
            - now;
        let ready = cd_left <= 0;
        let label = if ready {
            t(lang, Key::PowerStrike).to_string()
        } else {
            format!("{} ({cd_left}с)", t(lang, Key::PowerStrike))
        };
        if ready {
            row = row.child(btn(cx, label, pal.warn, 0xffffff, Click::PowerStrike));
        } else {
            row = row.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(label),
            );
        }
        row
    }

    // ── журнал мира (ФР-17) ───────────────────────────────────────────────

    fn log_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let dark = pal.bg == crate::palette::palette("dark").bg;
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
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(format!(
                        "{} · {}",
                        t(lang, Key::LogLabel),
                        self.store.log.len()
                    )),
            )
            .child(
                div()
                    .id("log_scroll")
                    .h(px(150.0))
                    .overflow_y_scroll()
                    .children(self.store.log.iter().map(|entry| {
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(log_color(entry.kind, dark)))
                            .child(entry.text.clone())
                    })),
            )
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
