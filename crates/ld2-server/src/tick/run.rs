//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).
//!
//! Разложено по фазам: `run` (цикл и применение плана), `combat` (план
//! боя, смерти, награды), `jobs` (работы, DoT, баффы), `expire`
//! (истечение сущностей), `plan` (структуры плана и события).

use std::time::Duration;

use serde_json::json;
use tracing::warn;

use crate::state::SharedState;

use super::combat::{apply_mob_hit, apply_rage_dealt, grant_kill_xp, plan_combat};
use super::expire::{
    buff_expire_step, corpse_expire_step, crime_expire_step, ground_expire_step, respawn_step,
};
use super::jobs::{
    craft_finish_step, harvest_step, quest_kill_progress, regen_step, rune_recharge_step,
};
use super::plan::RuneWork;

/// Инициализация живого мира: мобы из БД (после content-сида) + запуск
/// тик-лупа (D31). Общий вход для main и тестового старта.
pub async fn init(state: &SharedState) -> Result<(), sqlx::Error> {
    // Запрос без лока мира (clippy: не держим MutexGuard через await).
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT id, entities FROM locations")
            .fetch_all(&state.pool)
            .await?;
    {
        let mut world = state.world.lock().expect("мир");
        world.seed_mobs(
            &rows,
            state
                .config
                .world
                .respawn_default
                .map(|r| (r.min_s, r.max_s)),
        );
    }
    tokio::spawn(run(state.clone()));
    Ok(())
}

/// Запустить тик-луп (вызывается из [`init`]).
pub async fn run(state: SharedState) {
    let tick_hz = state.config.world.tick_hz.max(1);
    let period = Duration::from_millis((1000 / tick_hz).max(1) as u64);
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stopped = state.shutdown_rx();
    let mut ticks: u64 = 0;
    // M18/F58c (ADR-012 §4): аккумулятор зарядов рун — по аналогии с
    // `resource.energy_regen_s`, сек из конфига; считаем в секундах.
    let mut recharge_acc: f64 = 0.0;
    loop {
        tokio::select! {
            _ = stopped.changed() => break, // request_shutdown (graceful)
            _ = interval.tick() => {}
        }
        tick_once(&state).await;
        ticks += 1;
        if ticks.is_multiple_of(u64::from(tick_hz)) {
            // Раз в секунду: мана-реген (§2.4), истечение статусов (§10)
            // и готовность крафт-джобов (M12.6).
            regen_step(&state).await;
            crime_expire_step(&state).await;
            buff_expire_step(&state).await;
            craft_finish_step(&state).await;
            // M20: TTL — выброшенные предметы и трупы исчезают.
            ground_expire_step(&state).await;
            corpse_expire_step(&state).await;
            // M20: респавны мобов и статического лута по сроку.
            respawn_step(&state).await;
            // Снятие шкур: готовые разделки (сервер-джобы5 с).
            harvest_step(&state).await;
            // M18/F58c: заряды рун (режим `time`; `item` — без тикера).
            recharge_acc += 1.0;
            let interval_s = state.config.gameplay.magic.rune_recharge_s;
            if state.config.gameplay.magic.rune_recharge == "time" {
                let mut steps: u32 = 0;
                while recharge_acc >= interval_s {
                    recharge_acc -= interval_s;
                    steps += 1;
                }
                if steps > 0 {
                    rune_recharge_step(&state, steps).await;
                }
            } else {
                recharge_acc = 0.0;
            }
        }
    }
    tracing::info!("тик-луп остановлен");
}

/// Один тик: план атак (под локом, без await) → БД-работа (урон персонажам,
/// смерти, XP) → рассылка адресатам.
pub(super) async fn tick_once(state: &SharedState) {
    let now_ms = crate::time::now_ms();
    let mut plan = {
        let mut world = state.world.lock().expect("мир");
        plan_combat(
            &mut world,
            now_ms,
            &state.config.gameplay.combat,
            &state.config.skills,
        )
    };

    // 1) события атак (hit/despawn/corpse/end) — в первую очередь.
    send_all(state, &plan.events);
    // 2) урон персонажам от мобов (D27; смерть — общий поток ФР-06).
    for hit in &plan.mob_hits {
        let death_events = apply_mob_hit(state, hit).await;
        send_all(state, &death_events);
    }
    // M17/F30e: ярость воина за нанесённый урон — патч mp адресату.
    for (char_id, amount) in &plan.char_hits {
        if let Some((addr, text)) = apply_rage_dealt(state, char_id, *amount).await {
            let world = state.world.lock().expect("мир");
            world.send_to_char(&addr, text);
        }
    }
    // M18/F58d: авто-приёмы рун — расход зарядов в БД (CAS) +
    // наложение баффов + патчи клиенту.
    apply_rune_work(state, &plan.rune_work).await;
    // 3) XP убийств — в БД и событием (D27); квесты-«убийства» (ФР-12).
    for work in &plan.xp {
        if let Some(text) = grant_kill_xp(state, work).await {
            let world = state.world.lock().expect("мир");
            world.send_to_char(&work.char_id, text);
        }
        if let Some(target) = &work.target {
            let events = quest_kill_progress(state, &work.char_id, target).await;
            let world = state.world.lock().expect("мир");
            for (addr, text) in events {
                world.send_to_char(&addr, text);
            }
        }
    }
    plan.events.clear();
    let _ = &mut plan;
}

/// M18/F58d (ADR-012 §5): применить сработавшие авто-приёмы —
/// расход зарядов в БД CAS-записью (параллельный каст/тикер выигрывают
/// → кэш пересобирается), наложение баффов (`db::buffs::apply` со
/// стаками) и рассылка: патч сокетов владельцу + полная замена
/// `evt/char/buffs` после наложений.
pub(super) async fn apply_rune_work(state: &SharedState, work: &[RuneWork]) {
    if work.is_empty() {
        return;
    }
    let now = crate::time::now_epoch();
    // Персонажи, чей список баффов изменился → одна полная замена на каждого.
    let mut buffs_dirty: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for w in work {
        // ── расход зарядов в БД ────────────────────────────────────────
        let row = match crate::db::items::find(&state.pool, &w.staff_uid, &w.char_id).await {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(e) => {
                warn!("авто-приём (поиск {}): {e}", w.staff_uid);
                continue;
            }
        };
        let mut sockets: Vec<serde_json::Value> =
            serde_json::from_value(row.sockets.clone()).unwrap_or_default();
        let Some(slot) = sockets.get_mut(w.socket_index as usize) else {
            continue;
        };
        if let Some(r) = slot.get_mut("rune") {
            r["charges"] = json!(w.new_charges);
        }
        let new_sockets = serde_json::Value::Array(sockets);
        match crate::db::items::set_sockets_if_unchanged(
            &state.pool,
            &w.staff_uid,
            &row.sockets,
            new_sockets.clone(),
        )
        .await
        {
            Ok(true) => {
                // Патч сокетов владельцу (wire-виде — accepts из def).
                let mut updated = row.clone();
                updated.sockets = new_sockets;
                if let Ok(Some(def)) = crate::db::items::def(&state.pool, &updated.def_id).await {
                    let snapshot = crate::session::item_snapshot(&updated, &def);
                    if let Some(sockets_wire) = snapshot.sockets {
                        let mut patch = serde_json::Map::new();
                        patch.insert(
                            "sockets".into(),
                            serde_json::to_value(&sockets_wire).unwrap_or_default(),
                        );
                        let text = crate::session::event_text(
                            "evt/item/state",
                            None,
                            serde_json::to_value(ld2_protocol::evt::ItemState {
                                uid: w.staff_uid.clone(),
                                patch,
                            })
                            .unwrap_or_default(),
                        );
                        if !text.is_empty() {
                            let world = state.world.lock().expect("мир");
                            world.send_to_char(&w.char_id, text);
                        }
                    }
                }
            }
            Ok(false) => {
                // Гонка с кастом/тикером: кэш мог разойтись — пересборка.
                crate::session::refresh_char_runes(state, &w.char_id).await;
            }
            Err(e) => warn!("авто-приём (запись {}): {e}", w.staff_uid),
        }

        // ── наложение баффа ────────────────────────────────────────────
        if let Some(buff) = &w.buff {
            let res = crate::db::buffs::apply(
                &state.pool,
                &w.char_id,
                &buff.kind,
                &buff.title,
                &buff.param,
                buff.value,
                buff.pct,
                buff.duration_s,
                buff.max_stacks,
                now,
            )
            .await;
            match res {
                Ok(()) => {
                    buffs_dirty.insert(w.char_id.clone());
                }
                Err(e) => warn!("авто-приём (бафф {}): {e}", buff.kind),
            }
        }
    }

    // Полная замена списка баффов — по одному на персонажа.
    for char_id in buffs_dirty {
        let rows = match crate::db::buffs::active(&state.pool, &char_id, now).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("баффы (список {char_id}): {e}");
                continue;
            }
        };
        let buffs: Vec<ld2_protocol::types::BuffState> = rows
            .iter()
            .map(
                |(kind, title, stacks, expires_at, ..)| ld2_protocol::types::BuffState {
                    kind: kind.clone(),
                    title: title.clone(),
                    stacks: (*stacks).max(0) as u32,
                    expires_at: *expires_at,
                    extra: Default::default(),
                },
            )
            .collect();
        let text = crate::session::event_text(
            "evt/char/buffs",
            None,
            serde_json::to_value(ld2_protocol::evt::CharBuffs { buffs }).unwrap_or_default(),
        );
        if text.is_empty() {
            continue;
        }
        let world = state.world.lock().expect("мир");
        world.send_to_char(&char_id, text);
    }
}

/// Рассылка адресатам (§2.4: порядок внутри канала сохраняется).
pub(super) fn send_all(state: &SharedState, events: &[(String, String)]) {
    let world = state.world.lock().expect("мир");
    for (addr, text) in events {
        if !text.is_empty() {
            world.send_to_char(addr, text.clone());
        }
    }
}
