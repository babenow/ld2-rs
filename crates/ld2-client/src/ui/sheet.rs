//! Правая панель — лист персонажа (ФР-04 навыки/вложение пула, ФР-07
//! инвентарь/экипировка/drag&drop/требования). Состояние — `Forms.sheet`.

use eframe::egui;
use ld2_protocol::cmd::{
    CharXpInvest, CraftLearn, CraftRecipes, CraftStart, ItemDrop, ItemEquip, ItemUnequip, ItemUse,
    QuestAbandon, QuestAccept, QuestTurnIn, SocketExtract, SocketInsert, UpgradePreview,
    UpgradeStart,
};
use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::QuestPhase;
use ld2_protocol::types::{CharacterState, ItemSnapshot};

use super::{DragFrom, SheetTab};
use crate::l10n::{t, Key};
use crate::store::{Act, UpgradeView};
use crate::ui::UiCtx;

const RED: egui::Color32 = egui::Color32::from_rgb(0xE0, 0x6C, 0x75);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x98, 0xC3, 0x79);
const GRAY: egui::Color32 = egui::Color32::from_rgb(0xAB, 0xB2, 0xBF);
const TILE: egui::Vec2 = egui::vec2(132.0, 46.0);
const SLOT: egui::Vec2 = egui::vec2(270.0, 30.0);

fn rarity_color(rarity: &str) -> egui::Color32 {
    match rarity {
        "common" => egui::Color32::from_rgb(0xAB, 0xB2, 0xBF),
        "uncommon" => egui::Color32::from_rgb(0x98, 0xC3, 0x79),
        "rare" => egui::Color32::from_rgb(0x61, 0xAF, 0xEF),
        "epic" => egui::Color32::from_rgb(0xC6, 0x78, 0xDD),
        "legendary" => egui::Color32::from_rgb(0xE5, 0xC0, 0x7B),
        _ => egui::Color32::WHITE,
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

pub fn show(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let tab = ctx.forms.sheet.tab;

    tracker_block(ui, ctx);

    ui.horizontal(|ui| {
        for (candidate, key) in [
            (SheetTab::Skills, Key::SheetSkills),
            (SheetTab::Attrs, Key::SheetAttrs),
            (SheetTab::Inventory, Key::SheetInventory),
            (SheetTab::Statuses, Key::StatusesTab),
            (SheetTab::Quests, Key::QuestsTab),
            (SheetTab::Professions, Key::ProfessionsTab),
        ] {
            let selected = tab == candidate;
            if ui.selectable_label(selected, t(lang, key)).clicked() {
                ctx.forms.sheet.tab = candidate;
            }
        }
    });
    ui.separator();

    if let Some(err) = &ctx.store.sheet_error {
        ui.colored_label(RED, err);
        ui.add_space(4.0);
    }

    // Вкладка «Статусы»: журнал нарушений подгружается один раз (ФР-11).
    if tab == SheetTab::Statuses && !ctx.store.crime_log_loaded && ctx.store.snapshot.is_some() {
        ctx.store.crime_log_loaded = true;
        let cmd = ld2_protocol::cmd::CrimeLog { limit: Some(50) };
        send(ctx, &cmd, Act::CrimeLog);
    }
    // Вкладка «Профессии»: рецепты загружаются один раз (ФР-08).
    if tab == SheetTab::Professions && !ctx.store.recipes_loaded && ctx.store.snapshot.is_some() {
        ctx.store.recipes_loaded = true;
        let cmd = CraftRecipes { profession: None };
        send(ctx, &cmd, Act::Craft);
    }

    match tab {
        SheetTab::Skills => skills(ui, ctx),
        SheetTab::Attrs => attrs(ui, ctx),
        SheetTab::Inventory => inventory(ui, ctx),
        SheetTab::Statuses => statuses(ui, ctx),
        SheetTab::Quests => quests(ui, ctx),
        SheetTab::Professions => professions(ui, ctx),
    }

    // Модалка «Описание предмета» (ФР-07).
    if let Some(detail) = ctx.store.item_detail.clone() {
        item_modal(ctx, ui, detail);
    }
    // Модалки M5 (ФР-09/ФР-10).
    if let Some(view) = ctx.store.upgrade.clone() {
        upgrade_modal(ctx, ui, &view);
    }
    if let Some(staff_uid) = ctx.store.socketing.clone() {
        socketing_modal(ctx, ui, &staff_uid);
    }
}

// ─── вкладка «Навыки» (ФР-04) ──────────────────────────────────────────────

fn skills(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.weak(t(lang, Key::Loading));
        return;
    };

    ui.strong(format!(
        "{}: {}",
        t(lang, Key::XpPoolLabel),
        snap.char.xp_pool
    ));
    ui.separator();

    for skill in &snap.char.skills {
        let name = skill.name.clone().unwrap_or_else(|| skill.id.clone());
        ui.horizontal(|ui| {
            ui.label(name);
            ui.weak(format!("·{}", skill.rank));
        });
        let frac = if skill.xp_to_next > 0 {
            (skill.xp as f32 / skill.xp_to_next as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ui.add(egui::ProgressBar::new(frac).text(format!("{}/{}", skill.xp, skill.xp_to_next)));

        // Вложение очков опыта из пула (D3/D4, ФР-04).
        if snap.char.xp_pool > 0 {
            let pool = snap.char.xp_pool;
            ui.horizontal(|ui| {
                for (label, points) in [("+1", 1_i64), ("+5", 5_i64)] {
                    if ui.small_button(label).clicked() {
                        invest(ctx, &skill.id, points.min(pool));
                    }
                }
                if ui.small_button(t(lang, Key::MaxLabel)).clicked() {
                    invest(ctx, &skill.id, pool);
                }
            });
        }
        ui.add_space(6.0);
    }
}

fn invest(ctx: &mut UiCtx<'_>, skill_id: &str, points: i64) {
    let cmd = CharXpInvest {
        skill_id: skill_id.to_string(),
        points,
    };
    send(ctx, &cmd, Act::Invest);
}

fn attrs(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.weak(t(lang, Key::Loading));
        return;
    };
    egui::Grid::new("attrs_grid")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for (key, value) in &snap.char.attrs {
                ui.weak(key.as_str());
                ui.strong(value.to_string());
                ui.end_row();
            }
        });
}

// ─── вкладка «Инвентарь» (ФР-07) ──────────────────────────────────────────

fn inventory(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.weak(t(lang, Key::Loading));
        return;
    };
    let equip_slots: Vec<String> = ctx
        .store
        .session
        .as_ref()
        .map(|s| s.inventory.equip_slots.clone())
        .unwrap_or_default();

    let pointer = ui.input(|i| i.pointer.hover_pos());
    let released = ui.input(|i| i.pointer.any_released());

    // ── слоты экипировки (зоны сброса) ──
    ui.strong(t(lang, Key::EquipLabel));
    let mut slot_rects: Vec<(String, egui::Rect)> = Vec::new();
    for slot in &equip_slots {
        let uid = snap.char.equip.get(slot).cloned().flatten();
        let item = uid
            .as_ref()
            .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == *u));
        let width = ui.available_width().min(SLOT.x);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(width, SLOT.y), egui::Sense::click_and_drag());
        slot_rects.push((slot.clone(), rect));

        let dragging_over =
            ctx.forms.sheet.drag.is_some() && pointer.is_some_and(|p| rect.contains(p));
        let painter = ui.painter_at(rect);
        let bg = if dragging_over {
            egui::Color32::from_rgb(0x3A, 0x44, 0x3C)
        } else {
            egui::Color32::from_gray(34)
        };
        painter.rect_filled(rect, 4.0, bg);
        painter.text(
            rect.min + egui::vec2(6.0, 7.0),
            egui::Align2::LEFT_TOP,
            slot.as_str(),
            egui::FontId::proportional(11.0),
            GRAY,
        );

        if let Some(item) = item {
            let name_pos = rect.min + egui::vec2(76.0, 5.0);
            painter.text(
                name_pos,
                egui::Align2::LEFT_TOP,
                item.name.as_str(),
                egui::FontId::proportional(13.0),
                rarity_color(&item.rarity),
            );
            painter.text(
                rect.min + egui::vec2(76.0, 21.0),
                egui::Align2::LEFT_TOP,
                badges(item),
                egui::FontId::proportional(11.0),
                GRAY,
            );
            if resp.drag_started() {
                ctx.forms.start_drag(DragFrom::Slot(item.uid.clone()));
            }
            resp.context_menu(|ui| {
                let slot = slot.clone();
                if ui.button(t(lang, Key::MenuUnequip)).clicked() {
                    let cmd = ItemUnequip { slot: slot.clone() };
                    send(ctx, &cmd, Act::Unequip);
                    ui.close();
                }
                if ui.button(t(lang, Key::MenuDesc)).clicked() {
                    ctx.store.item_detail = Some(serde_json::to_value(item).unwrap_or_default());
                    ui.close();
                }
            });
            resp.on_hover_ui(|ui| tooltip(ui, item, &snap.char));
        } else {
            painter.text(
                rect.min + egui::vec2(76.0, 12.0),
                egui::Align2::LEFT_TOP,
                t(lang, Key::EmptySlot),
                egui::FontId::proportional(12.0),
                GRAY,
            );
        }
    }

    ui.add_space(8.0);

    // ── сетка инвентаря ──
    ui.strong(t(lang, Key::SheetInventory));
    let inv_frame = egui::Frame::group(ui.style()).show(ui, |ui| {
        if snap.char.inventory.is_empty() {
            ui.weak("…");
        }
        ui.horizontal_wrapped(|ui| {
            for item in &snap.char.inventory {
                inventory_tile(ui, ctx, item, &snap.char);
            }
        });
    });
    let inv_rect = inv_frame.response.rect;

    // ── завершение перетаскивания ──
    process_drop(ctx, ui, &snap, &slot_rects, inv_rect, pointer, released);
}

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

fn inventory_tile(
    ui: &mut egui::Ui,
    ctx: &mut UiCtx<'_>,
    item: &ItemSnapshot,
    ch: &CharacterState,
) {
    let lang = ctx.lang;
    let (rect, resp) = ui.allocate_exact_size(TILE, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(36));
    painter.text(
        rect.min + egui::vec2(6.0, 5.0),
        egui::Align2::LEFT_TOP,
        item.name.as_str(),
        egui::FontId::proportional(13.0),
        rarity_color(&item.rarity),
    );
    painter.text(
        rect.min + egui::vec2(6.0, 25.0),
        egui::Align2::LEFT_TOP,
        badges(item),
        egui::FontId::proportional(11.0),
        GRAY,
    );

    if resp.drag_started() {
        ctx.forms.start_drag(DragFrom::Inventory(item.uid.clone()));
    }

    resp.context_menu(|ui| {
        if ui.button(t(lang, Key::MenuUse)).clicked() {
            let cmd = ItemUse {
                item_uid: item.uid.clone(),
                target_id: None,
            };
            send(ctx, &cmd, Act::Drop); // результат — событиями (item/removed)
            ui.close();
        }
        if let Some(slot) = slot_for(&item.item_type) {
            let equipped = ch.equip.values().flatten().any(|u| u == &item.uid);
            if !equipped && ui.button(t(lang, Key::MenuEquip)).clicked() {
                let cmd = ItemEquip {
                    item_uid: item.uid.clone(),
                    slot: slot.to_string(),
                };
                send(ctx, &cmd, Act::Equip);
                ui.close();
            }
        }
        // Улучшение (ФР-10): до максимума из конфига сервера.
        let max_level = ctx
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let upgradable = matches!(item.item_type.as_str(), "weapon" | "armor" | "staff");
        if upgradable && item.upgrade < max_level && ui.button(t(lang, Key::UpgradeBtn)).clicked() {
            ctx.store.open_upgrade(item.uid.clone(), item.name.clone());
            let cmd = UpgradePreview {
                item_uid: item.uid.clone(),
            };
            send(ctx, &cmd, Act::Preview);
            ui.close();
        }
        // Инкрустация (ФР-09): только посох с сокетами.
        if item.item_type == "staff"
            && item.sockets.is_some()
            && ui.button(t(lang, Key::BtnSocketing)).clicked()
        {
            ctx.store.open_socketing(item.uid.clone());
            ui.close();
        }
        if ui.button(t(lang, Key::MenuDesc)).clicked() {
            ctx.store.item_detail = Some(serde_json::to_value(item).unwrap_or_default());
            ui.close();
        }
        if ui.button(t(lang, Key::MenuDrop)).clicked() {
            let cmd = ItemDrop {
                item_uid: item.uid.clone(),
                qty: None,
            };
            send(ctx, &cmd, Act::Drop);
            ui.close();
        }
    });
    // on_hover_ui забирает Response — поэтому последним.
    resp.on_hover_ui(|ui| tooltip(ui, item, ch));
}

fn tooltip(ui: &mut egui::Ui, item: &ItemSnapshot, ch: &CharacterState) {
    let lang = crate::l10n::Lang::Ru; // тултип — серверные данные; язык не важен
    ui.strong(item.name.as_str());
    ui.weak(format!("{} · +{}", item.rarity, item.upgrade));
    for (key, value) in &item.stats {
        ui.label(format!("{key}: {value}"));
    }
    if let Some(sockets) = &item.sockets {
        for socket in sockets {
            match &socket.rune {
                Some(rune) => {
                    ui.label(format!("◈ {}: {}/{}", rune.name, rune.charges, rune.max));
                }
                None => {
                    ui.label("○ —");
                }
            }
        }
    }
    if let Some(req) = &item.requires {
        if !req.attr.is_empty() {
            ui.separator();
            ui.weak(t(lang, Key::ReqLabel));
            for (attr, need) in &req.attr {
                let have = ch.attrs.get(attr).copied().unwrap_or(0);
                if have >= *need {
                    ui.label(format!("{attr}: {have}/{need}"));
                } else {
                    // ФР-07: недоступное требование — красным.
                    ui.colored_label(RED, format!("{attr}: {have}/{need}"));
                }
            }
        }
        if let Some(profession) = &req.profession {
            let known = ch.professions.iter().any(|p| &p.id == profession);
            let text = format!("{}: {profession}", t(lang, Key::ReqLabel));
            if known {
                ui.label(text);
            } else {
                ui.colored_label(RED, text);
            }
        }
    }
}

fn process_drop(
    ctx: &mut UiCtx<'_>,
    ui: &egui::Ui,
    snap: &StateSnapshot,
    slot_rects: &[(String, egui::Rect)],
    inv_rect: egui::Rect,
    pointer: Option<egui::Pos2>,
    released: bool,
) {
    let Some(drag) = ctx.forms.sheet.drag.clone() else {
        return;
    };

    // Призрак перетаскивания + подсветка зоны сброса.
    if let Some(pos) = pointer {
        if let Some(item) = find_item(snap, &drag) {
            ui.painter().text(
                pos + egui::vec2(12.0, 0.0),
                egui::Align2::LEFT_TOP,
                item.name.as_str(),
                egui::FontId::proportional(13.0),
                egui::Color32::WHITE,
            );
        }
        if let Some((_, rect)) = slot_rects.iter().find(|(_, r)| r.contains(pos)) {
            ui.painter()
                .rect_filled(*rect, 4.0, GREEN.gamma_multiply(0.35));
        }
        if matches!(drag, DragFrom::Slot(_)) && inv_rect.contains(pos) {
            ui.painter()
                .rect_filled(inv_rect, 4.0, GREEN.gamma_multiply(0.20));
        }
    }

    let Some(pos) = pointer.filter(|_| released) else {
        return;
    };
    match drag {
        DragFrom::Inventory(uid) => {
            if let Some((slot, _)) = slot_rects.iter().find(|(_, r)| r.contains(pos)) {
                let cmd = ItemEquip {
                    item_uid: uid,
                    slot: slot.clone(),
                };
                send(ctx, &cmd, Act::Equip);
            }
        }
        DragFrom::Slot(source_uid) => {
            if inv_rect.contains(pos) {
                // Перетащили из слота в инвентарь → снять (ФР-07).
                let slot = snap
                    .char
                    .equip
                    .iter()
                    .find(|(_, v)| v.as_ref() == Some(&source_uid))
                    .map(|(k, _)| k.clone());
                if let Some(slot) = slot {
                    let cmd = ItemUnequip { slot };
                    send(ctx, &cmd, Act::Unequip);
                }
            }
        }
    }
    ctx.forms.sheet.drag = None;
}

fn find_item<'a>(snap: &'a StateSnapshot, drag: &DragFrom) -> Option<&'a ItemSnapshot> {
    let uid = match drag {
        DragFrom::Inventory(uid) | DragFrom::Slot(uid) => uid,
    };
    snap.char.inventory.iter().find(|i| i.uid == *uid)
}

fn send<C: ld2_protocol::Command>(ctx: &mut UiCtx<'_>, cmd: &C, act: Act) {
    match ctx.transport.send(cmd) {
        Ok(_) => ctx.store.on_sheet_sent(act),
        Err(e) => ctx.store.sheet_error = Some(format!("нет связи: {e}")),
    }
}

// ─── вкладка «Статусы» (ФР-11, M6) ─────────────────────────────────────────

fn statuses(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.weak(t(lang, Key::Loading));
        return;
    };
    // Отсчёт по серверным срокам: now = локальное + offset (§1.1).
    let now = crate::store::local_epoch() + ctx.store.time_offset;
    let active = crate::store::filter_active_statuses(&snap.char.crime.statuses, now);

    if active.is_empty() {
        ui.weak(t(lang, Key::NoStatuses));
    }
    for status in &active {
        let title = super::crime_kind_title(ctx.store, &status.kind);
        let color = super::severity_color(status.severity);
        ui.horizontal(|ui| {
            ui.colored_label(color, title.as_str());
            ui.weak(super::format_duration(lang, status.expires_at - now));
        });
    }

    ui.separator();
    ui.strong(t(lang, Key::CrimeJournal));
    if ui.button(t(lang, Key::ReloadJournal)).clicked() {
        let cmd = ld2_protocol::cmd::CrimeLog { limit: Some(50) };
        send(ctx, &cmd, Act::CrimeLog);
    }
    if ctx.store.crime_log.is_empty() {
        ui.weak(t(lang, Key::JournalEmpty));
    }
    for entry in ctx.store.crime_log.iter().rev().take(60) {
        let title = super::crime_kind_title(ctx.store, &entry.kind);
        ui.weak(format!(
            "{title}: +{} · {} {}",
            super::format_duration(lang, entry.delta_s),
            t(lang, Key::TotalLabel),
            super::format_duration(lang, entry.total_s)
        ));
    }
}

// ─── трекер квестов (ФР-12) ─────────────────────────────────────────────────

fn tracker_block(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    if ctx.store.quest_tracker.is_empty() {
        return;
    }
    let Some(snap) = ctx.store.snapshot.clone() else {
        return;
    };
    ui.group(|ui| {
        ui.weak(t(lang, Key::TrackerLabel));
        for id in &ctx.store.quest_tracker {
            if let Some(q) = snap.char.quests.iter().find(|q| &q.id == id) {
                ui.horizontal(|ui| {
                    ui.label(q.title.clone().unwrap_or_else(|| q.id.clone()));
                    ui.weak(goals_summary(q));
                });
            }
        }
    });
    ui.separator();
}

/// Сводка целей: «Волков убито 4/5» или текст цели.
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

// ─── вкладка «Квесты» (ФР-12) ──────────────────────────────────────────────

fn quests(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.weak(t(lang, Key::Loading));
        return;
    };
    if snap.char.quests.is_empty() {
        ui.weak(t(lang, Key::NoQuests));
        return;
    }

    for quest in &snap.char.quests {
        let tracked = ctx.store.quest_tracker.contains(&quest.id);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let (state_key, color) = match quest.state {
                    QuestPhase::Available => (
                        Key::StateAvailable,
                        egui::Color32::from_rgb(0x98, 0xC3, 0x79),
                    ),
                    QuestPhase::Active => {
                        (Key::StateActive, egui::Color32::from_rgb(0xAB, 0xB2, 0xBF))
                    }
                    QuestPhase::TurnIn => {
                        (Key::StateTurnIn, egui::Color32::from_rgb(0xD1, 0x9A, 0x66))
                    }
                    QuestPhase::Done => (Key::StateDone, egui::Color32::from_rgb(0x61, 0xAF, 0xEF)),
                };
                ui.colored_label(color, t(lang, state_key));
                ui.strong(quest.title.clone().unwrap_or_else(|| quest.id.clone()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Трекер (ФР-12): максимум 3, переключение.
                    let label = if tracked {
                        t(lang, Key::TrackedBtn)
                    } else {
                        t(lang, Key::TrackBtn)
                    };
                    if ui.selectable_label(tracked, label).clicked() {
                        ctx.store.toggle_quest_tracker(&quest.id);
                    }
                    match quest.state {
                        QuestPhase::Available => {
                            if ui.small_button(t(lang, Key::BtnAccept)).clicked() {
                                let cmd = QuestAccept {
                                    quest_id: quest.id.clone(),
                                };
                                send(ctx, &cmd, Act::Quest);
                            }
                        }
                        QuestPhase::TurnIn => {
                            if ui.small_button(t(lang, Key::BtnTurnIn)).clicked() {
                                let cmd = QuestTurnIn {
                                    quest_id: quest.id.clone(),
                                };
                                send(ctx, &cmd, Act::Quest);
                            }
                        }
                        QuestPhase::Active => {
                            if ui.small_button(t(lang, Key::BtnAbandon)).clicked() {
                                let cmd = QuestAbandon {
                                    quest_id: quest.id.clone(),
                                };
                                send(ctx, &cmd, Act::Quest);
                            }
                        }
                        QuestPhase::Done => {}
                    }
                });
            });
            if !quest.goals.is_empty() {
                ui.weak(format!("• {}", goals_summary(quest)));
            }
            if let Some(reward) = &quest.reward_preview {
                if let Some(xp) = reward.xp {
                    ui.weak(format!("{}: {xp} XP", t(lang, Key::RewardLabel)));
                }
            }
        });
        ui.add_space(4.0);
    }
    if let Some(err) = &ctx.store.sheet_error {
        ui.colored_label(RED, err);
    }
}

// ─── вкладка «Профессии» (ФР-08) ───────────────────────────────────────────

/// Демо-каталог профессий для обучения (skills.md §5; сервер валидирует:
/// `E_PROFESSION_LIMIT`/`E_INTERNAL` — ошибки видны в sheet_error).
const LEARN_CATALOG: [(&str, Key); 7] = [
    ("blacksmith", Key::ProfBlacksmith),
    ("alchemist", Key::ProfAlchemist),
    ("woodcarver", Key::ProfWoodcarver),
    ("tailor", Key::ProfTailor),
    ("cook", Key::ProfCook),
    ("herbalist", Key::ProfHerbalist),
    ("hunter", Key::ProfHunter),
];

/// Название профессии: l10n-ключ по id, фолбэк — сам id.
fn prof_label(lang: crate::l10n::Lang, id: &str) -> String {
    use crate::l10n::Key as K;
    let key = match id {
        "blacksmith" => Some(K::ProfBlacksmith),
        "alchemist" => Some(K::ProfAlchemist),
        "scribe" => Some(K::ProfScribe),
        "woodcarver" => Some(K::ProfWoodcarver),
        "tailor" => Some(K::ProfTailor),
        "cook" => Some(K::ProfCook),
        "herbalist" => Some(K::ProfHerbalist),
        "hunter" => Some(K::ProfHunter),
        _ => None,
    };
    key.map(|k| t(lang, k).to_string())
        .unwrap_or_else(|| id.to_string())
}

fn professions(ui: &mut egui::Ui, ctx: &mut UiCtx<'_>) {
    let lang = ctx.lang;
    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.weak(t(lang, Key::Loading));
        return;
    };

    // Изученные профессии: ранги и XP (ФР-08).
    for prof in &snap.char.professions {
        ui.horizontal(|ui| {
            ui.label(prof_label(lang, &prof.id));
            ui.weak(t(lang, Key::RankFmt).replace("{r}", &prof.rank.to_string()));
            let frac = if prof.xp_to_next > 0 {
                (prof.xp as f32 / prof.xp_to_next as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.add(egui::ProgressBar::new(frac).text(format!("{}/{}", prof.xp, prof.xp_to_next)));
        });
    }

    // Обучение новым (демо-каталог).
    let available: Vec<(&'static str, Key)> = LEARN_CATALOG
        .iter()
        .copied()
        .filter(|(id, _)| !snap.char.professions.iter().any(|p| p.id == *id))
        .collect();
    if !available.is_empty() {
        ui.add_space(4.0);
        egui::ComboBox::from_label(t(lang, Key::LearnLabel)).show_ui(ui, |ui| {
            for (id, key) in &available {
                if ui.selectable_label(false, t(lang, *key)).clicked() {
                    let cmd = CraftLearn {
                        profession_id: id.to_string(),
                    };
                    send(ctx, &cmd, Act::Learn);
                }
            }
        });
    }

    if let Some(err) = &ctx.store.sheet_error {
        ui.colored_label(RED, err);
        ui.add_space(4.0);
    }

    ui.separator();
    ui.strong(t(lang, Key::RecipesLabel));
    if ctx.store.recipes.is_empty() {
        ui.weak(t(lang, Key::NoRecipes));
    }

    // Рецепты: требования, ингредиенты (красные при нехватке — ФР-08), крафт.
    let recipes = ctx.store.recipes.clone();
    for recipe in &recipes {
        let recipe_id = recipe["id"].as_str().unwrap_or_default().to_string();
        let name = recipe["name"].as_str().unwrap_or("?");
        let profession = recipe["profession"].as_str().unwrap_or("");
        let rank = recipe["rank"].as_i64().unwrap_or(0);
        let station = recipe["station_label"].as_str().unwrap_or("");

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

        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(name);
                ui.weak(format!(
                    "· {} · {}",
                    prof_label(lang, profession),
                    t(lang, Key::RankFmt).replace("{r}", &rank.to_string())
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let pending = ctx.store.pending == Some(Act::Craft);
                    if ui
                        .add_enabled(
                            ingredients_ok && !pending,
                            egui::Button::new(t(lang, Key::BtnCraft)),
                        )
                        .clicked()
                    {
                        let cmd = CraftStart {
                            recipe_id: recipe_id.clone(),
                            qty: 1,
                        };
                        send(ctx, &cmd, Act::Craft);
                    }
                });
            });
            ui.weak(station);
            for (line, ok) in ingredient_lines {
                if ok {
                    ui.weak(format!("• {line}"));
                } else {
                    ui.colored_label(
                        RED,
                        format!("• {}: {}", t(lang, Key::MissingIngredients), line),
                    );
                }
            }
        });
        ui.add_space(4.0);
    }
}

// ─── модалка описания предмета (ФР-07) ─────────────────────────────────────

fn item_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, detail: serde_json::Value) {
    let lang = ctx.lang;
    let mut close = false;
    egui::Window::new(t(lang, Key::ItemDescTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            let name = detail["name"].as_str().unwrap_or("?");
            let rarity = detail["rarity"].as_str().unwrap_or("common");
            ui.colored_label(rarity_color(rarity), name);
            ui.weak(format!(
                "{} · +{}",
                detail["type"].as_str().unwrap_or("?"),
                detail["upgrade"].as_i64().unwrap_or(0)
            ));
            if let Some(stats) = detail["stats"].as_object() {
                ui.add_space(4.0);
                for (key, value) in stats {
                    ui.label(format!("{key}: {value}"));
                }
            }
            if let Some(sockets) = detail["sockets"].as_array() {
                ui.add_space(4.0);
                for socket in sockets {
                    if let Some(rune) = socket["rune"].as_object() {
                        ui.label(format!(
                            "◈ {}: {}/{}",
                            rune["name"], rune["charges"], rune["max"]
                        ));
                    } else {
                        ui.label("○ —".to_string());
                    }
                }
            }
            if let Some(value) = detail["gold_value"].as_i64() {
                ui.weak(format!("{value} зол."));
            }
            ui.add_space(8.0);
            if ui.button(t(lang, Key::Exit)).clicked() {
                close = true;
            }
        });
    if close {
        ctx.store.item_detail = None;
    }
}

// ─── модалка улучшения (ФР-10) ──────────────────────────────────────────────

fn upgrade_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, view: &UpgradeView) {
    let lang = ctx.lang;
    let mut close = false;
    let pending_upgrade = ctx.store.pending == Some(Act::Upgrade);

    egui::Window::new(t(lang, Key::UpgradeTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.heading(view.item_name.as_str());

            let Some(preview) = &view.preview else {
                ui.weak(t(lang, Key::Loading));
                if ui.button(t(lang, Key::Exit)).clicked() {
                    close = true;
                }
                return;
            };

            // «сейчас +N → после +M» (дельта от сервера, §7).
            let current = ctx
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
            ui.label(format!("+{current} → {delta}"));

            if let Some(chance) = preview["chance"].as_f64() {
                ui.weak(format!(
                    "{}: {:.0}%",
                    t(lang, Key::ChanceLabel),
                    chance * 100.0
                ));
            }
            if let Some(gold) = preview["cost"]["gold"].as_i64() {
                ui.weak(format!("{}: {} зол.", t(lang, Key::CostLabel), gold));
            }
            if let Some(materials) = preview["cost"]["materials"].as_object() {
                for (name, qty) in materials {
                    ui.weak(format!("  • {name}: {qty}"));
                }
            }
            if let Some(err) = &ctx.store.sheet_error {
                ui.colored_label(RED, err);
            }
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button(t(lang, Key::Exit)).clicked() {
                    close = true;
                }
                let preview_id = preview["preview_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let can_start = !preview_id.is_empty() && !pending_upgrade;
                if ui
                    .add_enabled(can_start, egui::Button::new(t(lang, Key::UpgradeBtn)))
                    .clicked()
                {
                    let cmd = UpgradeStart {
                        item_uid: view.item_uid.clone(),
                        preview_id,
                    };
                    send(ctx, &cmd, Act::Upgrade);
                }
                if pending_upgrade {
                    ui.weak(t(lang, Key::Loading));
                }
            });
        });

    if close {
        ctx.store.close_upgrade();
    }
}

// ─── модалка инкрустации (ФР-09) ────────────────────────────────────────────

fn socketing_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, staff_uid: &str) {
    let lang = ctx.lang;
    let mut close = false;

    let Some(snap) = ctx.store.snapshot.clone() else {
        return;
    };
    let Some(staff) = snap
        .char
        .inventory
        .iter()
        .find(|i| i.uid == staff_uid)
        .cloned()
    else {
        ctx.store.close_socketing();
        return;
    };
    let runes: Vec<ItemSnapshot> = snap
        .char
        .inventory
        .iter()
        .filter(|i| i.item_type == "rune")
        .cloned()
        .collect();

    egui::Window::new(t(lang, Key::SocketingTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.heading(staff.name.as_str());

            for socket in staff.sockets.iter().flatten() {
                ui.horizontal(|ui| {
                    ui.weak(format!("◈ {}", socket.index));
                    match &socket.rune {
                        Some(rune) => {
                            ui.label(format!("{} ({}/{})", rune.name, rune.charges, rune.max));
                            if ui.small_button(t(lang, Key::ExtractRune)).clicked() {
                                let cmd = SocketExtract {
                                    staff_uid: staff.uid.clone(),
                                    socket_index: socket.index,
                                };
                                send(ctx, &cmd, Act::Socket);
                            }
                        }
                        None => {
                            if runes.is_empty() {
                                ui.weak(t(lang, Key::NoRunes));
                            }
                            for rune in &runes {
                                let label = format!("{}: {}", t(lang, Key::InsertRune), rune.name);
                                if ui.small_button(label).clicked() {
                                    let cmd = SocketInsert {
                                        staff_uid: staff.uid.clone(),
                                        socket_index: socket.index,
                                        rune_uid: rune.uid.clone(),
                                    };
                                    send(ctx, &cmd, Act::Socket);
                                }
                            }
                        }
                    }
                });
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
        ctx.store.close_socketing();
    }
}
