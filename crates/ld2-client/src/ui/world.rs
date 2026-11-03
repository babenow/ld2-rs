//! Игровой экран M3 (ФР-03, ФР-17): панель локации, навигация (мышь +
//! Alt+стрелки), списки сущностей с HP/тегами/⚠, контекстные меню, осмотр,
//! колонка истории и журнал мира. Игровые панели по ФР ТЗ §7.1 — с M6.

use eframe::egui;
use ld2_protocol::cmd::{CombatAttack, EntityInfo, LocationMove};
use ld2_protocol::types::{Entity, EntityKind, LocationSnapshot};

use crate::l10n::{t, Key};
use crate::store::Act;
use crate::ui::{log_panel, ScreenEffects, UiCtx};

const RED: egui::Color32 = egui::Color32::from_rgb(0xE0, 0x6C, 0x75);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xD1, 0x9A, 0x66);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x98, 0xC3, 0x79);

pub fn game(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui) -> ScreenEffects {
    let mut effects = vec![];
    let lang = ctx.lang;

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
    ui.columns(2, |cols| {
        let left = &mut cols[0];
        left.set_max_width(210.0);
        resources(left, ctx, &snap, &mut effects);
        left.add_space(10.0);
        history(left, ctx);

        let right = &mut cols[1];
        location_panel(right, ctx, &snap.location);
    });

    ui.add_space(4.0);
    log_panel::show(ui, lang, &ctx.store.log);

    if let Some(detail) = ctx.store.entity_detail.clone() {
        examine_modal(ctx, ui, detail);
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
    ui.horizontal(|ui| {
        ui.heading(snap.char.name.as_str());
        for status in &snap.char.crime.statuses {
            ui.colored_label(ORANGE, &status.kind);
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
        if player_corpse {
            ui.separator();
            ui.weak(t(lang, Key::CorpsePlayerHint));
        }
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
                ui.add_space(6.0);
                ui.strong(t(lang, Key::LootLabel));
                for item in loot {
                    ui.label(format!(
                        "• {} ({})",
                        item["name"].as_str().unwrap_or("?"),
                        item["type"].as_str().unwrap_or("?")
                    ));
                }
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
    let Some(dir) = dir else { return };
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
