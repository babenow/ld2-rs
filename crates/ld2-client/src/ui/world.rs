//! Игровой экран M3 (ФР-03, ФР-17): панель локации, навигация (мышь +
//! Alt+стрелки), списки сущностей с HP/тегами/⚠, контекстные меню, осмотр,
//! колонка истории и журнал мира. Игровые панели по ФР ТЗ §7.1 — с M6.

use eframe::egui;
use ld2_protocol::cmd::{
    CombatAttack, CombatFlee, EntityInfo, ItemUse, LocationMove, MagicRuneCast, SkillUse,
};
use ld2_protocol::evt::{CharDeath, StateSnapshot};
use ld2_protocol::types::{Entity, EntityKind, LocationSnapshot};

use crate::l10n::{t, tf, Key};
use crate::store::{filter_active_statuses, local_epoch, Act};
use crate::ui::{
    crime_kind_title, format_duration, log_panel, severity_color, sheet, ScreenEffects, UiCtx,
};

const RED: egui::Color32 = egui::Color32::from_rgb(0xE0, 0x6C, 0x75);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xD1, 0x9A, 0x66);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x98, 0xC3, 0x79);

pub fn game(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui) -> ScreenEffects {
    let mut effects = vec![];
    let lang = ctx.lang;

    // ФР-06: пока открыта модалка смерти — мир закрыт, закрыть можно
    // только подтверждением («Возродиться»).
    if let Some(death) = ctx.store.death.clone() {
        death_modal(ctx, ui, death);
        return effects;
    }

    // M8: приглашение в группу (ФР-14) — модалка с ответом до входа в мир.
    if let Some(invite) = ctx.store.party_invite.clone() {
        party_invite_modal(ctx, ui, &invite);
    }

    // M8: приглашение в гильдию (ФР-16) — модалка с ответом.
    if let Some(invite) = ctx.store.guild_invite.clone() {
        guild_invite_modal(ctx, ui, &invite);
    }

    // M8: окно сделки (ФР-15) — две колонки, версия с сервера.
    if let Some(trade) = ctx.store.trade.clone() {
        trade_modal(ctx, ui, &trade);
    }

    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.weak(t(lang, Key::Loading));
        });
        return effects;
    };

    // Навигация с клавиатуры: Alt+↑↓←→ (ФР-03).
    alt_nav(ctx, ui, &snap.location);

    ui.add_space(6.0);
    ui.columns(3, |cols| {
        let left = &mut cols[0];
        left.set_max_width(210.0);
        // ФР-14: виджет группы сверху слева (сворачиваемый).
        party_widget(left, ctx);
        left.add_space(6.0);
        resources(left, ctx, &snap, &mut effects);
        left.add_space(10.0);
        history(left, ctx);

        let center = &mut cols[1];
        location_panel(center, ctx, &snap.location);

        // Правая колонка — лист персонажа (ФР-04/ФР-07).
        let right = &mut cols[2];
        sheet::show(right, ctx);
    });

    ui.add_space(4.0);
    hotbar(ui, ctx, &snap);

    ui.add_space(4.0);
    // Нижняя панель чата (ФР-13, M8): каналы, история, ввод.
    crate::ui::chat_panel::show(ui, ctx);

    ui.add_space(4.0);
    log_panel::show(ui, lang, &ctx.store.log);

    if let Some(detail) = ctx.store.entity_detail.clone() {
        examine_modal(ctx, ui, detail);
    }

    // ФР-11: подтверждение забора лута трупа игрока (предупреждение о статусе).
    if let Some((uid, name)) = ctx.store.loot_confirm.clone() {
        loot_confirm_modal(ctx, ui, &uid, &name);
    }
    effects
}

// ─── левая колонка: ресурсы и история ──────────────────────────────────────

fn resources(
    ui: &mut egui::Ui,
    ctx: &mut UiCtx<'_>,
    snap: &ld2_protocol::evt::StateSnapshot,
    effects: &mut ScreenEffects,
) {
    let lang = ctx.lang;
    // Цель (ФР-05): клик по строке сущности или Tab.
    if let Some(target_id) = ctx.store.target.clone() {
        if let Some(entity) = snap.location.entities.iter().find(|e| e.id == target_id) {
            ui.group(|ui| {
                ui.strong(format!("{}: {}", t(lang, Key::TargetLabel), entity.name));
                if let Some(hp) = &entity.hp {
                    let frac = (hp.cur as f32 / hp.max.max(1) as f32).clamp(0.0, 1.0);
                    ui.add(egui::ProgressBar::new(frac).text(format!("{}/{}", hp.cur, hp.max)));
                }
                ui.horizontal(|ui| {
                    if ui.small_button(t(lang, Key::Attack)).clicked() {
                        send_attack(ctx, &entity.id);
                    }
                    if ui.small_button(t(lang, Key::BtnFlee)).clicked() {
                        let cmd = CombatFlee {};
                        match ctx.transport.send(&cmd) {
                            Ok(_) => ctx.store.on_world_sent(Act::Attack),
                            Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
                        }
                    }
                });
            });
            ui.add_space(6.0);
        }
    }

    ui.horizontal(|ui| {
        ui.heading(snap.char.name.as_str());
        // Бейджи статусов с обратным отсчётом (ФР-11): серверные сроки,
        // now = локальное + offset (§1.1); истёвшие отфильтрованы.
        let now = local_epoch() + ctx.store.time_offset;
        for status in filter_active_statuses(&snap.char.crime.statuses, now) {
            let title = crime_kind_title(ctx.store, &status.kind);
            let left = status.expires_at - now;
            ui.colored_label(
                severity_color(status.severity),
                format!("{title} · {}", format_duration(lang, left)),
            );
        }
    });
    ui.add_space(4.0);

    let hp = (snap.char.hp.cur as f32 / snap.char.hp.max.max(1) as f32).clamp(0.0, 1.0);
    ui.add(egui::ProgressBar::new(hp).text(format!(
        "{} {}/{}",
        t(lang, Key::HpLabel),
        snap.char.hp.cur,
        snap.char.hp.max
    )));
    let mp = (snap.char.mp.cur as f32 / snap.char.mp.max.max(1) as f32).clamp(0.0, 1.0);
    ui.add(egui::ProgressBar::new(mp).text(format!(
        "{} {}/{}",
        t(lang, Key::MpLabel),
        snap.char.mp.cur,
        snap.char.mp.max
    )));
    ui.weak(format!(
        "{}: {}",
        t(lang, Key::XpPoolLabel),
        snap.char.xp_pool
    ));
    ui.add_space(8.0);

    if ui.button(t(lang, Key::BtnSwitchChar)).clicked() {
        // Hot-switch (ФР-02): чистый сброс снимка, без ре-логина.
        ctx.store.back_to_characters();
    }
    if ui.button(t(lang, Key::BtnLogout)).clicked() {
        let _ = ctx.transport.send(&ld2_protocol::cmd::AuthLogout {});
        effects.extend(ctx.store.logout());
    }
}

fn history(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    ui.strong(t(lang, Key::HistoryLabel));
    for visited in ctx.store.visited.iter().rev().take(10) {
        ui.weak(&visited.name);
    }
}

// ─── центр: панель локации ────────────────────────────────────────────────

fn location_panel(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>, loc: &LocationSnapshot) {
    let lang = ctx.lang;
    let safe = loc.flags.iter().any(|f| f == "safe");

    ui.horizontal(|ui| {
        ui.heading(loc.name.as_str());
        for flag in &loc.flags {
            match flag.as_str() {
                "pvp" => {
                    ui.colored_label(ORANGE, "PvP");
                }
                "safe" => {
                    // Флаги — игровые данные сервера; показываем как есть.
                    ui.colored_label(GREEN, "safe");
                }
                other => {
                    ui.weak(other);
                }
            };
        }
    });

    ui.group(|ui| {
        egui::ScrollArea::vertical()
            .id_salt("location_text")
            .max_height(110.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(egui::Label::new(loc.text.as_str()).wrap());
            });
    });

    if let Some(err) = &ctx.store.world_error {
        ui.colored_label(RED, err);
    }
    if let Some(notice) = &ctx.store.notice {
        // ФР-11: тосты мира (в т.ч. «⚠ PvP-зона: активны статусы …»).
        ui.colored_label(ORANGE, notice);
    }

    // Выходы: кнопки + подсказка хоткеев (ФР-03).
    ui.horizontal_wrapped(|ui| {
        ui.strong(t(lang, Key::ExitsLabel));
        for exit in &loc.exits {
            if ui.button(format!("→ {}", exit.label)).clicked() {
                send_move(ctx, &exit.id);
            }
        }
        if loc.exits.is_empty() {
            ui.weak(t(lang, Key::NoExits));
        }
    });
    ui.weak(t(lang, Key::AltNavHint));

    entity_group(ui, ctx, Key::PlayersLabel, loc, safe, |e| {
        e.kind == EntityKind::Player
    });
    entity_group(ui, ctx, Key::MonstersLabel, loc, safe, |e| {
        e.kind == EntityKind::Monster
    });
    entity_group(ui, ctx, Key::NpcsLabel, loc, safe, |e| {
        e.kind == EntityKind::Npc
    });
    entity_group(ui, ctx, Key::CorpsesLabel, loc, safe, |e| {
        e.kind == EntityKind::Corpse
    });
}

fn entity_group(
    ui: &mut egui::Ui,
    ctx: &mut UiCtx<'_>,
    title: Key,
    loc: &LocationSnapshot,
    safe: bool,
    pred: impl Fn(&Entity) -> bool,
) {
    let entities: Vec<Entity> = loc.entities.iter().filter(|e| pred(e)).cloned().collect();
    if entities.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.strong(t(ctx.lang, title));
    for entity in &entities {
        entity_row(ui, ctx, entity, safe);
    }
}

fn entity_row(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>, entity: &Entity, safe: bool) {
    let lang = ctx.lang;
    let player_corpse = entity.kind == EntityKind::Corpse && entity.was_player.unwrap_or(false);

    let response = ui
        .horizontal(|ui| {
            if player_corpse {
                // ФР-11: ⚠ — труп игрока, лут вешает статус «Мародёр».
                ui.colored_label(ORANGE, "⚠");
            }
            ui.label(entity.name.as_str());
            if let Some(hp) = &entity.hp {
                let frac = (hp.cur as f32 / hp.max.max(1) as f32).clamp(0.0, 1.0);
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(110.0)
                        .text(format!("{}/{}", hp.cur, hp.max)),
                );
            }
            for tag in entity.tags.iter().flatten() {
                ui.colored_label(ORANGE, tag);
            }
        })
        .response;

    // Левый клик по строке — выбор цели (ФР-05).
    if response.clicked() && matches!(entity.kind, EntityKind::Monster | EntityKind::Player) {
        ctx.store.select_target(entity.id.clone());
    }

    response.context_menu(|ui| {
        if ui.button(t(lang, Key::Examine)).clicked() {
            send_examine(ctx, &entity.id);
            ui.close();
        }
        // «Атаковать» скрыт в безопасной зоне (ФР-03); сервер — истина правил.
        let attackable = !safe && matches!(entity.kind, EntityKind::Monster | EntityKind::Player);
        if attackable && ui.button(t(lang, Key::Attack)).clicked() {
            send_attack(ctx, &entity.id);
            ui.close();
        }
        // M8: социальные действия по игроку (ФР-14/15).
        if matches!(entity.kind, EntityKind::Player) {
            if ui.button(t(lang, Key::PartyInviteBtn)).clicked() {
                let cmd = ld2_protocol::cmd::PartyInvite {
                    player_id: entity.id.clone(),
                };
                match ctx.transport.send(&cmd) {
                    Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Party),
                    Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                }
                ui.close();
            }
            if ui.button(t(lang, Key::TradeBtn)).clicked() {
                let cmd = ld2_protocol::cmd::TradeOpen {
                    player_id: entity.id.clone(),
                };
                match ctx.transport.send(&cmd) {
                    Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Trade),
                    Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                }
                ui.close();
            }
        }
        if player_corpse {
            ui.separator();
            ui.weak(t(lang, Key::CorpsePlayerHint));
        }
    });
}

// ─── M8: группа, сделка (ФР-14, ФР-15) ─────────────────────────────────────

/// Виджет группы в левой колонке (ФР-14): состав, лидер, HP, операции.
fn party_widget(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let collapsed = ctx.forms.party_collapsed;
    let party = ctx.store.party.clone();
    let my_id = ctx
        .store
        .snapshot
        .as_ref()
        .map(|s| s.char.id.clone())
        .unwrap_or_default();
    ui.group(|ui| {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!collapsed, t(lang, Key::PartyLabel))
                .clicked()
            {
                ctx.forms.party_collapsed = !collapsed;
            }
            if collapsed {
                return;
            }
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match &party {
                    None => {
                        if ui.small_button(t(lang, Key::PartyCreateBtn)).clicked() {
                            let cmd = ld2_protocol::cmd::PartyCreate {};
                            match ctx.transport.send(&cmd) {
                                Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Party),
                                Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                            }
                        }
                    }
                    Some(_) => {
                        if ui.small_button(t(lang, Key::PartyLeaveBtn)).clicked() {
                            let cmd = ld2_protocol::cmd::PartyLeave {};
                            match ctx.transport.send(&cmd) {
                                Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Party),
                                Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                            }
                        }
                    }
                },
            );
        });
        if collapsed {
            return;
        }
        let Some(party) = party else {
            ui.weak(t(lang, Key::PartyEmpty));
            return;
        };
        let i_am_leader = party.leader == my_id;
        for member in &party.members {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if member.id == party.leader {
                            ui.colored_label(GREEN, "★");
                        }
                        ui.label(&member.name);
                        if member.id == my_id {
                            ui.weak("(вы)");
                        }
                    });
                    if let Some(hp) = &member.hp {
                        let frac = (hp.cur as f32 / hp.max.max(1) as f32).clamp(0.0, 1.0);
                        ui.add(egui::ProgressBar::new(frac).desired_height(8.0));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Исключает только лидер и не себя (ФР-14).
                    if i_am_leader
                        && member.id != my_id
                        && ui.small_button(t(lang, Key::PartyKickBtn)).clicked()
                    {
                        let cmd = ld2_protocol::cmd::PartyKick {
                            player_id: member.id.clone(),
                        };
                        match ctx.transport.send(&cmd) {
                            Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Party),
                            Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                        }
                    }
                });
            });
        }
    });
}

/// Модалка приглашения в группу (ФР-14): принять/отклонить, §7 invite_id.
fn party_invite_modal(
    ctx: &mut UiCtx<'_>,
    ui: &mut egui::Ui,
    invite: &crate::store::PartyInviteView,
) {
    let lang = ctx.lang;
    egui::Window::new(t(lang, Key::PartyLabel))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
        .show(ui.ctx(), |ui| {
            ui.label(tf(
                lang,
                Key::PartyInviteFrom,
                &[("{name}", invite.from_name.as_str())],
            ));
            ui.horizontal(|ui| {
                if ui.button(t(lang, Key::PartyAcceptBtn)).clicked() {
                    let cmd = ld2_protocol::cmd::PartyAccept {
                        invite_id: invite.invite_id.clone(),
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Party),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                    ctx.store.party_invite = None;
                }
                if ui.button(t(lang, Key::PartyDeclineBtn)).clicked() {
                    let cmd = ld2_protocol::cmd::PartyDecline {
                        invite_id: invite.invite_id.clone(),
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Party),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                    ctx.store.party_invite = None;
                }
            });
        });
}

/// Окно сделки (ФР-15): две колонки я/партнёр, золото, фиксация, отмена.
/// Версия/зеркало — с сервера (клиент не пересчитывает).
fn trade_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, trade: &ld2_protocol::evt::TradeState) {
    let lang = ctx.lang;
    let my_id = ctx
        .store
        .snapshot
        .as_ref()
        .map(|s| s.char.id.clone())
        .unwrap_or_default();
    let inventory = ctx
        .store
        .snapshot
        .as_ref()
        .map(|s| s.char.inventory.clone())
        .unwrap_or_default();
    let offer_id = trade.offer_id.clone();
    let version = trade.version;
    egui::Window::new(t(lang, Key::TradeLabel))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            // Оценка стоимости + предупреждение (ФР-15 анти-абуз).
            if let Some(value) = trade.value_hint {
                ui.weak(tf(lang, Key::TradeValue, &[("{v}", &value.to_string())]));
            }
            ui.weak(t(lang, Key::TradeWarn));
            ui.separator();

            ui.columns(2, |cols| {
                // Моя колонка: предложения + добавление + золото.
                let me = &mut cols[0];
                me.strong(t(lang, Key::TradeYou));
                for item in &trade.you {
                    me.label(format!("• {} ×{}", item.name, item.qty.unwrap_or(1)));
                }
                me.weak(format!("{}: {}", t(lang, Key::TradeGold), trade.you_gold));
                if trade.you_locked {
                    me.weak(format!("✓ {}", t(lang, Key::TradeAccept)));
                }

                // Их колонка (зеркало с сервера).
                let them = &mut cols[1];
                them.strong(t(lang, Key::TradeThem));
                for item in &trade.them {
                    them.label(format!("• {} ×{}", item.name, item.qty.unwrap_or(1)));
                }
                them.weak(format!("{}: {}", t(lang, Key::TradeGold), trade.them_gold));
                if trade.them_locked {
                    them.weak(format!("✓ {}", t(lang, Key::TradeAccept)));
                }
            });

            ui.separator();
            // Добавление предмета из инвентаря (только не предложенные).
            let offered: Vec<String> = trade.you.iter().map(|i| i.uid.clone()).collect();
            let available: Vec<_> = inventory
                .iter()
                .filter(|i| !offered.contains(&i.uid))
                .collect();
            if !available.is_empty() {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("trade_item")
                        .selected_text(t(lang, Key::TradeAddItem))
                        .show_ui(ui, |ui| {
                            for item in &available {
                                if ui
                                    .selectable_label(
                                        ctx.forms.trade_item == item.uid,
                                        item.name.clone(),
                                    )
                                    .clicked()
                                {
                                    ctx.forms.trade_item = item.uid.clone();
                                }
                            }
                        });
                });
            }
            if !ctx.forms.trade_item.is_empty() && !offered.contains(&ctx.forms.trade_item) {
                let uid = ctx.forms.trade_item.clone();
                if ui
                    .small_button(format!("＋ {}", t(lang, Key::TradeAddItem)))
                    .clicked()
                {
                    let cmd = ld2_protocol::cmd::TradeAddItem {
                        offer_id: offer_id.clone(),
                        item_uid: uid.clone(),
                        qty: None,
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Trade),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                    ctx.forms.trade_item.clear();
                }
            }

            // Золото.
            ui.horizontal(|ui| {
                ui.label(t(lang, Key::TradeGold));
                ui.add(
                    egui::TextEdit::singleline(&mut ctx.forms.trade_gold)
                        .desired_width(90.0)
                        .hint_text("0"),
                );
                if ui.small_button(t(lang, Key::TradeSetGold)).clicked() {
                    let gold: i64 = ctx.forms.trade_gold.trim().parse().unwrap_or(0);
                    let cmd = ld2_protocol::cmd::TradeSetGold {
                        offer_id: offer_id.clone(),
                        gold,
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Trade),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                // Фиксация своей стороны (сверка версии — на сервере, §9).
                if ui.button(t(lang, Key::TradePropose)).clicked() {
                    let cmd = ld2_protocol::cmd::TradePropose {
                        offer_id: offer_id.clone(),
                        version,
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Trade),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                }
                if ui.button(t(lang, Key::TradeAccept)).clicked() {
                    let cmd = ld2_protocol::cmd::TradeAccept {
                        offer_id: offer_id.clone(),
                        version,
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Trade),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                }
                if ui.button(t(lang, Key::TradeCancel)).clicked() {
                    let cmd = ld2_protocol::cmd::TradeCancel {
                        offer_id: offer_id.clone(),
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Trade),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                }
            });
            let _ = &my_id;
        });
}

/// Модалка приглашения в гильдию (ФР-16): принять = `guild.join` (§7).
fn guild_invite_modal(
    ctx: &mut UiCtx<'_>,
    ui: &mut egui::Ui,
    invite: &crate::store::GuildInviteView,
) {
    let lang = ctx.lang;
    egui::Window::new(t(lang, Key::GuildTab))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
        .show(ui.ctx(), |ui| {
            ui.label(tf(
                lang,
                Key::PartyInviteFrom,
                &[("{name}", invite.guild_name.as_str())],
            ));
            ui.horizontal(|ui| {
                if ui.button(t(lang, Key::GuildJoinBtn)).clicked() {
                    let cmd = ld2_protocol::cmd::GuildJoin {
                        guild_id: Some(invite.guild_id.clone()),
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_sheet_sent(crate::store::Act::Guild),
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                    ctx.store.guild_invite = None;
                }
                if ui.button(t(lang, Key::PartyDeclineBtn)).clicked() {
                    ctx.store.guild_invite = None;
                }
            });
        });
}

fn examine_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, detail: serde_json::Value) {
    let lang = ctx.lang;
    let mut close = false;
    egui::Window::new(t(lang, Key::EntityDetailTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            let name = detail["entity"]["name"].as_str().unwrap_or("?");
            let kind = detail["entity"]["kind"].as_str().unwrap_or("?");
            ui.heading(name);
            ui.weak(kind);
            if let Some(hp) = detail["entity"]["hp"].as_object() {
                ui.weak(format!("HP: {}/{}", hp["cur"], hp["max"]));
            }
            if detail["was_player"] == true {
                ui.colored_label(ORANGE, t(lang, Key::CorpsePlayerHint));
            }
            if let Some(loot) = detail["loot"].as_array() {
                let looted = detail["looted"] == true;
                ui.add_space(6.0);
                ui.strong(t(lang, Key::LootLabel));
                for item in loot {
                    let uid = item["uid"].as_str().unwrap_or_default().to_string();
                    let name = item["name"].as_str().unwrap_or("?").to_string();
                    let kind = item["type"].as_str().unwrap_or("?");
                    ui.horizontal(|ui| {
                        ui.label(format!("• {name} ({kind})"));
                        if looted {
                            return;
                        }
                        if ui.small_button(t(lang, Key::TakeLoot)).clicked() {
                            if detail["was_player"] == true {
                                // ФР-11: лут трупа ИГРОКА — подтверждение о статусе.
                                ctx.store.set_loot_confirm(uid.clone(), name.clone());
                            } else {
                                let cmd = ItemUse {
                                    item_uid: uid.clone(),
                                    target_id: None,
                                };
                                match ctx.transport.send(&cmd) {
                                    Ok(_) => {
                                        ctx.store.on_world_sent(Act::Drop);
                                        ctx.store.entity_detail = None;
                                    }
                                    Err(e) => {
                                        ctx.store.world_error = Some(format!("нет связи: {e}"))
                                    }
                                }
                            }
                        }
                    });
                }
            }
            if let Some(err) = &ctx.store.world_error {
                ui.colored_label(RED, err);
            }
            if let Some(err) = &ctx.store.sheet_error {
                ui.colored_label(RED, err);
            }
            ui.add_space(8.0);
            if ui.button(t(lang, Key::Exit)).clicked() {
                close = true;
            }
        });
    if close {
        ctx.store.entity_detail = None;
    }
}

// ─── навигация и команды ──────────────────────────────────────────────────

fn alt_nav(ctx: &mut UiCtx<'_>, ui: &egui::Ui, loc: &LocationSnapshot) {
    let dir = ui.input(|i| {
        if !i.modifiers.alt {
            return None;
        }
        for (key, direction) in [
            (egui::Key::ArrowUp, "north"),
            (egui::Key::ArrowDown, "south"),
            (egui::Key::ArrowLeft, "west"),
            (egui::Key::ArrowRight, "east"),
        ] {
            if i.key_pressed(key) {
                return Some(direction.to_string());
            }
        }
        None
    });
    let Some(dir) = dir else {
        // Tab — цикл целей по монстрам (ФР-05).
        if ui.input(|i| i.key_pressed(egui::Key::Tab) && !i.modifiers.alt && !i.modifiers.command) {
            ctx.store.cycle_target();
        }
        return;
    };
    if let Some(exit) = loc
        .exits
        .iter()
        .find(|e| e.dir.as_deref() == Some(dir.as_str()))
    {
        send_move(ctx, &exit.id);
    }
}

fn send_move(ctx: &mut UiCtx<'_>, exit_id: &str) {
    let cmd = LocationMove {
        exit_id: exit_id.to_string(),
    };
    match ctx.transport.send(&cmd) {
        Ok(_) => ctx.store.on_world_sent(Act::Move),
        Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
    }
}

fn send_examine(ctx: &mut UiCtx<'_>, entity_id: &str) {
    let cmd = EntityInfo {
        entity_id: entity_id.to_string(),
    };
    match ctx.transport.send(&cmd) {
        Ok(_) => ctx.store.on_world_sent(Act::Examine),
        Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
    }
}

fn send_attack(ctx: &mut UiCtx<'_>, entity_id: &str) {
    let cmd = CombatAttack {
        target_id: entity_id.to_string(),
    };
    match ctx.transport.send(&cmd) {
        Ok(_) => ctx.store.on_world_sent(Act::Attack),
        Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
    }
}

// ─── панель действий (ФР-05/ФР-09): руны посоха + свитки/зелья ────────────

fn hotbar(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>, snap: &StateSnapshot) {
    let lang = ctx.lang;
    // Стоимость каста в мане — из конфига сервера (§5, M5.0).
    let rune_cost = ctx
        .store
        .session
        .as_ref()
        .map(|s| s.magic.mana.rune_cost)
        .unwrap_or(0);

    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong(t(lang, Key::HotbarLabel));

            // Руны экипированного посоха: бейдж зарядов ×N (ФР-09).
            if let Some(staff_uid) = snap.char.equip.get("staff").cloned().flatten() {
                if let Some(staff) = snap.char.inventory.iter().find(|i| i.uid == staff_uid) {
                    for socket in staff.sockets.iter().flatten() {
                        let Some(rune) = &socket.rune else { continue };
                        let charges_ok = rune.charges > 0;
                        let mp_ok = rune_cost <= 0 || snap.char.mp.cur >= rune_cost;
                        let enabled = charges_ok && mp_ok;
                        let label = format!("{} ×{}", rune.name, rune.charges);
                        let resp = ui.add_enabled(enabled, egui::Button::new(label));
                        if enabled && resp.clicked() {
                            let cmd = MagicRuneCast {
                                staff_uid: staff_uid.clone(),
                                socket_index: socket.index,
                                target_id: None,
                            };
                            match ctx.transport.send(&cmd) {
                                Ok(_) => ctx.store.on_world_sent(Act::Cast),
                                Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
                            }
                        }
                        // Тултип: заряды и стоимость в мане (ФР-09).
                        resp.on_hover_ui(|ui| {
                            ui.strong(rune.name.as_str());
                            ui.weak(format!("{}/{}", rune.charges, rune.max));
                            if rune_cost > 0 {
                                ui.label(format!(
                                    "{}: {} маны",
                                    t(lang, Key::CostLabel),
                                    rune_cost
                                ));
                            }
                            if !charges_ok {
                                ui.colored_label(RED, t(lang, Key::NoCharge));
                            }
                            if !mp_ok {
                                ui.colored_label(RED, t(lang, Key::NotEnoughMp));
                            }
                        });
                    }
                }
            }

            // Свитки и зелья из инвентаря (свиток — без маны, D4).
            let consumables: Vec<_> = snap
                .char
                .inventory
                .iter()
                .filter(|i| matches!(i.item_type.as_str(), "scroll" | "potion"))
                .cloned()
                .collect();
            for item in consumables {
                let label = format!("{} ×{}", item.name, item.qty.unwrap_or(1));
                if ui
                    .button(label)
                    .on_hover_text(item.item_type.as_str())
                    .clicked()
                {
                    let cmd = ItemUse {
                        item_uid: item.uid.clone(),
                        target_id: None,
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => ctx.store.on_world_sent(Act::Cast),
                        Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
                    }
                }
            }

            // Активка «Мощный удар» (ФР-05, skills.md §4): только кулдаун (D4).
            let now = local_epoch() + ctx.store.time_offset;
            let cd_left = ctx
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
            let resp = ui.add_enabled(ready, egui::Button::new(label));
            if ready && resp.clicked() {
                let cmd = SkillUse {
                    skill_id: "power_strike".into(),
                    target_id: ctx.store.target.clone(),
                };
                match ctx.transport.send(&cmd) {
                    Ok(_) => ctx.store.on_world_sent(Act::Cast),
                    Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            resp.on_hover_ui(|ui| {
                ui.weak(t(lang, Key::TabHint));
                if !ready {
                    ui.colored_label(RED, format!("{cd_left}с"));
                }
            });
        });
    });
}

// ─── модалка смерти (ФР-06) ────────────────────────────────────────────────

fn death_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, death: CharDeath) {
    let lang = ctx.lang;
    egui::Window::new(t(lang, Key::DeathTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.colored_label(RED, t(lang, Key::DeathTitle));
            ui.add_space(6.0);
            ui.weak(t(lang, Key::DeathLost));
            for item in &death.lost_items {
                ui.label(format!(
                    "• {} ({})",
                    item.name,
                    item.rarity.as_deref().unwrap_or("-")
                ));
            }
            ui.add_space(10.0);
            // Закрыть можно только подтверждением — крестика и Esc нет.
            if ui
                .add_sized([180.0, 34.0], egui::Button::new(t(lang, Key::DeathConfirm)))
                .clicked()
            {
                ctx.store.confirm_death();
            }
        });
}

// ─── подтверждение: лут трупа игрока (ФР-11) ───────────────────────────────

fn loot_confirm_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, uid: &str, name: &str) {
    let lang = ctx.lang;
    let mut cancel = false;
    let mut take = false;
    egui::Window::new(t(lang, Key::TakeLoot))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.colored_label(RED, t(lang, Key::LootPlayerWarn));
            ui.weak(name);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t(lang, Key::Exit)).clicked() {
                    cancel = true;
                }
                if ui.button(t(lang, Key::TakeLoot)).clicked() {
                    take = true;
                }
            });
        });

    if take {
        ctx.store.clear_loot_confirm();
        let cmd = ItemUse {
            item_uid: uid.to_string(),
            target_id: None,
        };
        match ctx.transport.send(&cmd) {
            Ok(_) => {
                ctx.store.on_world_sent(Act::Drop);
                ctx.store.entity_detail = None;
            }
            Err(e) => ctx.store.world_error = Some(format!("нет связи: {e}")),
        }
    } else if cancel {
        ctx.store.clear_loot_confirm();
    }
}
