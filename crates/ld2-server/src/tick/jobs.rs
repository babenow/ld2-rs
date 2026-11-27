//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).
//!
//! Разложено по фазам: `run` (цикл и применение плана), `combat` (план
//! боя, смерти, награды), `jobs` (работы, DoT, баффы), `expire`
//! (истечение сущностей), `plan` (структуры плана и события).

use ld2_protocol::names;

use ld2_protocol::evt::CharStats;
use serde_json::json;
use tracing::warn;

use crate::state::SharedState;

/// M18/F58c (ADR-012 §4): восстановление зарядов рун — по аналогии с
/// `regen_step`/`resource.energy_regen_s`: только онлайн-персонажи,
/// все их предметы с сокетами; `steps` — сколько зарядов начислить
/// (шаг аккумулятора ≥ 1 с — валидация конфига). Начисляются только
/// руны с `charges < max`, до потолка; изменения — CAS-записью (не
/// затираем параллельный каст/вставку) и патчем `evt/item/state`
/// владельцу (сокеты в wire-виде — с `accepts`, §6).
pub(super) async fn rune_recharge_step(state: &SharedState, steps: u32) {
    let online: Vec<String> = {
        let world = state.world.lock().expect("мир");
        world.online_chars()
    };
    if online.is_empty() {
        return;
    }
    let rows = match crate::db::items::with_sockets(&state.pool, &online).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!("заряды рун (поиск сокетов): {e}");
            return;
        }
    };
    for (char_id, mut row) in rows {
        let mut sockets: Vec<serde_json::Value> =
            serde_json::from_value(row.sockets.clone()).unwrap_or_default();
        let mut changed = false;
        for slot in &mut sockets {
            let Some(rune) = slot.get_mut("rune") else {
                continue;
            };
            if rune.is_null() {
                continue;
            }
            let charges = rune.get("charges").and_then(|v| v.as_i64()).unwrap_or(0);
            let max = rune.get("max").and_then(|v| v.as_i64()).unwrap_or(0);
            if max > charges {
                rune["charges"] = json!((charges + i64::from(steps)).min(max));
                changed = true;
            }
        }
        if !changed {
            continue;
        }
        let new_sockets = serde_json::Value::Array(sockets);
        // CAS: параллельный каст/вставка выигрывают — тогда пропускаем
        // (следующий интервал догонит по свежим данным).
        match crate::db::items::set_sockets_if_unchanged(
            &state.pool,
            &row.uid,
            &row.sockets,
            new_sockets.clone(),
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                warn!("заряды рун (запись {}): {e}", row.uid);
                continue;
            }
        }
        row.sockets = new_sockets;
        // Патч владельцу: сокеты в wire-виде (accepts добирается из def).
        let Some(def) = (match crate::db::items::def(&state.pool, &row.def_id).await {
            Ok(def) => def,
            Err(e) => {
                warn!("заряды рун (определение {}): {e}", row.def_id);
                continue;
            }
        }) else {
            continue;
        };
        let snapshot = crate::session::item_snapshot(&row, &def);
        let Some(sockets_wire) = snapshot.sockets else {
            continue;
        };
        let mut patch = serde_json::Map::new();
        patch.insert(
            "sockets".into(),
            serde_json::to_value(&sockets_wire).unwrap_or_default(),
        );
        let text = crate::session::event_text(
            names::EVT_ITEM_STATE,
            None,
            serde_json::to_value(ld2_protocol::evt::ItemState {
                uid: row.uid.clone(),
                patch,
            })
            .unwrap_or_default(),
        );
        if !text.is_empty() {
            let world = state.world.lock().expect("мир");
            world.send_to_char(&char_id, text);
        }
    }
}

/// Ресурс-шаг раз в секунду (M17/F30e; мана — skills.md §2.4):
/// мана — прежняя модель, **ярость** воина падает вне боя (в бою не
/// трогаем), **энергия** лучника растёт всегда; дробь — вверх (M12.5b).
pub(super) async fn regen_step(state: &SharedState) {
    let online: Vec<String> = {
        let world = state.world.lock().expect("мир");
        world.online_chars()
    };
    if online.is_empty() {
        return;
    }
    for char_id in &online {
        let in_combat = {
            let world = state.world.lock().expect("мир");
            world.in_combat(char_id)
        };
        let row = match crate::db::characters::get(&state.pool, char_id).await {
            Ok(Some(row)) => row,
            _ => continue,
        };
        // M17/F30e: ресурс по профилю — шаг из systems (None — молчим).
        let profile = row.profile.as_str();
        let max = crate::systems::resource_max(
            profile,
            &state.config.gameplay.resource,
            i64::from(row.mp_max),
        );
        // M30: духовность ускоряет реген маны (character-params.md §3.2).
        let attrs: std::collections::BTreeMap<String, i64> =
            serde_json::from_value(row.attrs.clone()).unwrap_or_default();
        let spir = attrs.get("spir").copied().unwrap_or(0);
        let mp_new = crate::systems::resource_step(
            profile,
            i64::from(row.mp_cur),
            max,
            in_combat,
            spir,
            &state.config,
        );
        // M30: регенерация здоровья (character-params.md §3.1) —
        // мёртвые не регенерируют, молчим без изменений.
        let hp_max = i64::from(row.hp_max);
        let hp_new =
            crate::systems::health_step(i64::from(row.hp_cur), hp_max, in_combat, &state.config);
        if mp_new.is_none() && hp_new.is_none() {
            continue; // без изменений — без шума каждый тик
        }
        let mut patch = serde_json::Map::new();
        if let Some(new) = mp_new {
            if let Err(e) = crate::db::characters::set_mp(&state.pool, char_id, new as i32).await {
                warn!("реген (mp {}): {e}", char_id);
                continue;
            }
            patch.insert("mp".into(), json!({ "cur": new, "max": max }));
        }
        if let Some(new) = hp_new {
            if let Err(e) =
                crate::db::characters::heal_hp(&state.pool, char_id, new - i64::from(row.hp_cur))
                    .await
            {
                warn!("реген (hp {}): {e}", char_id);
                continue;
            }
            patch.insert("hp".into(), json!({ "cur": new, "max": hp_max }));
        }
        let text = crate::session::event_text(
            names::EVT_CHAR_STATS,
            None,
            serde_json::to_value(CharStats { patch }).unwrap_or_default(),
        );
        let world = state.world.lock().expect("мир");
        world.send_to_char(char_id, text);
    }
}

/// Снятие шкур: готовые разделки → лут в труп (шкуры — в общий
/// список с качеством по рангу и критом, мясо/потроха — в отдельный
/// «Внутри»), XP только за снятие шкуры (формула/множители — M22,
/// docs/professions.md §3), итог персонажу `evt/harvest/result` и
/// обновление трупа (`evt/corpse/created` — апсерт у клиента).
pub(super) async fn harvest_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let due = {
        let mut world = state.world.lock().expect("мир");
        world.due_harvests(now)
    };
    for job in due {
        // Профессия персонажа: уровень/ранг (качество, крит, гейт
        // уровней) — нет строки (сбросили посреди действия) → без XP.
        let learned = crate::db::professions::learned(&state.pool, &job.char_id)
            .await
            .ok()
            .and_then(|rows| rows.into_iter().find(|(id, _, _, _)| id == "skinning"));
        // LearnedRow = (id, xp, rank, level) — порядок важен: ранг
        // идёт первым, уровень вторым (фикс: качество шкуры бралось
        // с уровня → «Великолепная» вместо обычной).
        let (rank, level) = learned
            .as_ref()
            .map(|(_, _, r, l)| ((*r).max(1) as u32, (*l).max(0) as u32))
            .unwrap_or((1, 0));
        // Труп: требуемый уровень — из флагов контента.
        let required = {
            let world = state.world.lock().expect("мир");
            world
                .corpse(&job.corpse_id)
                .and_then(|c| {
                    c.harvest
                        .as_ref()
                        .and_then(|h| h.get("level"))
                        .and_then(|v| v.as_i64())
                })
                .unwrap_or(1)
                .max(1) as u32
        };
        let finished = {
            let mut world = state.world.lock().expect("мир");
            world.finish_harvest(&job.corpse_id, &job.action, level, rank)
        };
        // Итог персонажу — всегда (ok=false: труп исчез/уже разделано).
        let ok = finished.is_some();
        let result = ld2_protocol::evt::HarvestResult {
            entity_id: job.corpse_id.clone(),
            name: job.name.clone(),
            action: job.action.clone(),
            ok,
            ruined: false,
        };
        let text = crate::session::event_text(names::EVT_HARVEST_RESULT, None, {
            serde_json::to_value(result).unwrap_or_default()
        });
        if !text.is_empty() {
            let world = state.world.lock().expect("мир");
            world.send_to_char(&job.char_id, text);
        }
        let Some((corpse, _skin, _inner)) = finished else {
            continue;
        };
        // XP — только за снятие шкуры (освежевание не даёт).
        if job.action == "skin" {
            if let Some((_, xp_old, rank_row, _)) = learned {
                let gain = crate::systems::harvest_xp_gain(required, level);
                if gain > 0 {
                    // M33/M34: премиум и другие множители профессии.
                    let gain = crate::progress::profession_xp(state, &job.char_id, gain).await;
                    let cfg = &state.config.gameplay.professions;
                    let (xp_new, level_new) =
                        crate::systems::profession_level_up(cfg, xp_old, level, gain);
                    match crate::db::professions::grant_progress(
                        &state.pool,
                        &job.char_id,
                        "skinning",
                        xp_new,
                        rank_row,
                        level_new as i32,
                    )
                    .await
                    {
                        Ok(()) => {
                            let snapshot = crate::systems::profession_snapshot(
                                cfg, "skinning", xp_new, rank_row, level_new,
                            );
                            let text = crate::session::event_text(
                                names::EVT_CRAFT_PROFESSION,
                                None,
                                serde_json::to_value(ld2_protocol::evt::CraftProfession {
                                    profession: snapshot,
                                    name: crate::db::professions::profession_name(
                                        &state.pool,
                                        "skinning",
                                    )
                                    .await
                                    .ok()
                                    .flatten(),
                                    gain: Some(gain),
                                })
                                .unwrap_or_default(),
                            );
                            if !text.is_empty() {
                                let world = state.world.lock().expect("мир");
                                world.send_to_char(&job.char_id, text);
                            }
                        }
                        Err(e) => warn!("снятие шкур (XP skinning): {e}"),
                    }
                }
            }
        }
        // Обновление трупа всем в локации (лут/флаги «сделано»).
        let loot_count = corpse.loot.iter().filter(|s| s.is_some()).count()
            + corpse.inner_loot.iter().filter(|s| s.is_some()).count();
        let updated = ld2_protocol::evt::CorpseCreated {
            entity: corpse.entity(),
            loot_count: loot_count as u32,
        };
        let text = crate::session::event_text(names::EVT_CORPSE_CREATED, None, {
            serde_json::to_value(updated).unwrap_or_default()
        });
        if !text.is_empty() {
            let world = state.world.lock().expect("мир");
            for addr in world.chars_in_location(&job_corpse_location(&world, &job.corpse_id)) {
                world.send_to_char(&addr, text.clone());
            }
        }
    }
}

/// Локация трупа для рассылки (нет трупа — рассылать некуда).
pub(super) fn job_corpse_location(world: &crate::world::World, corpse_id: &str) -> String {
    world
        .corpse(corpse_id)
        .map(|c| c.location_id.clone())
        .unwrap_or_default()
}

/// Готовые крафт-джобы раз в секунду (M12.6): выдача предметов + XP
/// профессии → `evt/craft/result`, `evt/item/added`, `evt/craft/profession`.
/// Джоб забирается атомарно (`take_job`) — рестарт не дублирует выдачу (D27).
pub(super) async fn craft_finish_step(state: &SharedState) {
    let now = crate::time::now_epoch() as f64;
    let jobs = match crate::db::craft::ready_jobs(&state.pool, now).await {
        Ok(jobs) => jobs,
        Err(e) => {
            warn!("крафт (джобы): {e}");
            return;
        }
    };
    for (job_id, char_id, recipe_id, total_qty, xp) in jobs {
        // Джоб НЕ забираем, пока персонаж офлайн: событие ушло бы в
        // непривязанный канал и результат потерялся бы (D27 — рестарт не
        // теряет: материалы списаны, выдача ждёт входа игрока).
        let online = {
            let world = state.world.lock().expect("мир");
            world.is_bound(&char_id)
        };
        if !online {
            continue;
        }
        if !crate::db::craft::take_job(&state.pool, job_id)
            .await
            .unwrap_or(false)
        {
            continue; // забран параллельным шагом
        }
        let mut events: Vec<(String, String)> = Vec::new();
        // Выдача предметов (output_type × total_qty).
        let output_type = recipe_output_type(state, &recipe_id).await;
        let made =
            match crate::db::items::insert_by_type(&state.pool, &char_id, &output_type, total_qty)
                .await
            {
                Ok(Some(row)) => vec![serde_json::json!({ "uid": row.uid, "qty": total_qty })],
                Ok(None) => {
                    warn!("крафт: нет предмета типа {output_type} (рецепт {recipe_id})");
                    Vec::new()
                }
                Err(e) => {
                    warn!("крафт (выдача {char_id}): {e}");
                    Vec::new()
                }
            };
        let result = ld2_protocol::evt::CraftResult {
            recipe_id: recipe_id.clone(),
            ok: true,
            made: made
                .iter()
                .filter_map(|m| {
                    Some(ld2_protocol::evt::MadeItem {
                        uid: m["uid"].as_str()?.to_string(),
                        qty: m["qty"].as_i64()?,
                    })
                })
                .collect(),
            consumed: Vec::new(),
            extra: Default::default(),
        };
        events.push((
            char_id.clone(),
            crate::session::event_text(
                names::EVT_CRAFT_RESULT,
                None,
                serde_json::to_value(&result).unwrap_or_default(),
            ),
        ));
        // XP профессии (skills.md §5.1: 40 + 25×ранг — уже в xp рецепта).
        if xp > 0 {
            if let Some(text) = profession_xp_step(state, &char_id, &recipe_id, xp).await {
                events.push((char_id.clone(), text));
            }
        }
        let world = state.world.lock().expect("мир");
        for (addr, text) in events {
            if !text.is_empty() {
                world.send_to_char(&addr, text);
            }
        }
    }
}

/// `output_type` рецепта (для вставки предмета по типу).
pub(super) async fn recipe_output_type(state: &SharedState, recipe_id: &str) -> String {
    crate::db::craft::recipe(&state.pool, recipe_id)
        .await
        .ok()
        .flatten()
        .map(|r| r.output_type)
        .unwrap_or_else(|| recipe_id.to_string())
}

/// XP в профессию рецепта с повышением ранга (skills.md §5.1) → событие
/// `evt/craft/profession` (или `None`).
pub(super) async fn profession_xp_step(
    state: &SharedState,
    char_id: &str,
    recipe_id: &str,
    gain: i64,
) -> Option<String> {
    let recipe = crate::db::craft::recipe(&state.pool, recipe_id)
        .await
        .ok()
        .flatten()?;
    let learned = crate::db::professions::learned(&state.pool, char_id)
        .await
        .ok()?;
    let (xp, rank, level) = learned
        .iter()
        .find(|(id, _, _, _)| *id == recipe.profession)
        .map(|(_, x, r, l)| (*x, *r, *l))
        .unwrap_or((0, 1, 0));
    let cfg = &state.config.gameplay.professions;
    // M33/M34: премиум и другие множители опыта профессии.
    let gain = crate::progress::profession_xp(state, char_id, gain).await;
    // M21: опыт растит УРОВЕНЬ (остаток переносится, потолок —
    // max_level); ранг меняет только тренер/награда квеста.
    let (xp, level_new) = crate::systems::profession_level_up(cfg, xp, level.max(0) as u32, gain);
    if let Err(e) = crate::db::professions::grant_progress(
        &state.pool,
        char_id,
        &recipe.profession,
        xp,
        rank,
        level_new as i32,
    )
    .await
    {
        warn!("крафт (XP профессии {char_id}): {e}");
        return None;
    }
    let snapshot =
        crate::systems::profession_snapshot(cfg, &recipe.profession, xp, rank, level_new);
    let name = crate::db::professions::profession_name(&state.pool, &recipe.profession)
        .await
        .ok()
        .flatten();
    let text = crate::session::event_text(
        names::EVT_CRAFT_PROFESSION,
        None,
        serde_json::to_value(ld2_protocol::evt::CraftProfession {
            profession: snapshot,
            name,
            gain: Some(gain),
        })
        .unwrap_or_default(),
    );
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Прогресс квестов типа `kill <target>` для убийцы (ФР-12): цели
/// продвигаются, выполненные → `evt/quest/progress`/`evt/quest/updated`.
pub(crate) async fn quest_kill_progress(
    state: &SharedState,
    char_id: &str,
    target: &str,
) -> Vec<(String, String)> {
    let defs = match crate::db::quests::defs(&state.pool).await {
        Ok(defs) => defs,
        Err(e) => {
            warn!("квесты (defs): {e}");
            return Vec::new();
        }
    };
    let progress = match crate::db::quests::progress(&state.pool, char_id).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!("квесты (прогресс {char_id}): {e}");
            return Vec::new();
        }
    };
    let mut events = Vec::new();
    for def in &defs {
        let Some(row) = progress.iter().find(|(id, _, _)| id == &def.0) else {
            continue; // не взят
        };
        if row.1 != "active" {
            continue;
        }
        let (new_progress, all_done) = crate::db::quests::advance_kill(&def.3, &row.2, target);
        if new_progress == row.2 {
            continue; // цель не про эту сущность
        }
        let new_state = if all_done { "turn_in" } else { "active" };
        if let Err(e) =
            crate::db::quests::set_progress(&state.pool, char_id, &def.0, new_progress.clone())
                .await
        {
            warn!("квесты (запись {}): {e}", def.0);
            continue;
        }
        if all_done {
            if let Err(e) =
                crate::db::quests::set_state(&state.pool, char_id, &def.0, "turn_in").await
            {
                warn!("квесты (turn_in {}): {e}", def.0);
            }
        }
        let fresh = crate::db::quests::progress(&state.pool, char_id)
            .await
            .unwrap_or_default();
        let p = fresh.iter().find(|(id, _, _)| id == &def.0);
        let state_obj = crate::db::quests::build_state(def, p);
        let ty = if new_state == "turn_in" {
            names::EVT_QUEST_UPDATED
        } else {
            names::EVT_QUEST_PROGRESS
        };
        let text = crate::session::event_text(ty, None, serde_json::json!({ "quest": state_obj }));
        if !text.is_empty() {
            events.push((char_id.to_string(), text));
        }
    }
    events
}
