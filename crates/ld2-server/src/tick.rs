//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).

use std::time::Duration;

use ld2_protocol::evt::{
    CharStats, CombatEnd, CombatHit, CombatKind, CorpseCreated, EntityDespawn, EntityHp,
};
use ld2_protocol::types::Hp;
use serde_json::json;
use tracing::warn;

use crate::config::{CombatSection, SkillsSection};
use crate::state::SharedState;
use crate::world::World;

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
        world.seed_mobs(&rows);
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
            craft_finish_step(&state).await;
        }
    }
    tracing::info!("тик-луп остановлен");
}

/// Один тик: план атак (под локом, без await) → БД-работа (урон персонажам,
/// смерти, XP) → рассылка адресатам.
async fn tick_once(state: &SharedState) {
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

/// Рассылка адресатам (§2.4: порядок внутри канала сохраняется).
fn send_all(state: &SharedState, events: &[(String, String)]) {
    let world = state.world.lock().expect("мир");
    for (addr, text) in events {
        if !text.is_empty() {
            world.send_to_char(addr, text.clone());
        }
    }
}

/// Запланированный урон моба персонажу.
#[derive(Debug)]
struct MobHit {
    char_id: String,
    amount: i64,
}

/// Запланированный XP убийства. `skill` — `None`: навык берётся из
/// экипированного оружия при применении; `Some` — школа заклинания (M12.5b).
#[derive(Debug)]
pub(crate) struct KillXp {
    pub(crate) char_id: String,
    pub(crate) amount: i64,
    pub(crate) skill: Option<String>,
    /// id убитой сущности — прогресс квестов типа `kill` (ФР-12).
    pub(crate) target: Option<String>,
}

#[derive(Default)]
struct Plan {
    /// (адресат — char_id, JSON конверта).
    events: Vec<(String, String)>,
    mob_hits: Vec<MobHit>,
    xp: Vec<KillXp>,
}

fn plan_combat(
    world: &mut World,
    now_ms: i64,
    cfg: &CombatSection,
    skills_cfg: &SkillsSection,
) -> Plan {
    let mut plan = Plan::default();
    let attack_interval_ms = ((cfg.attack_interval_s * 1000.0).round() as i64).max(1);

    // ── атаки персонажей ────────────────────────────────────────────────
    for (char_id, target, loc, next_at) in world.combat_list() {
        if now_ms < next_at {
            continue;
        }
        world.set_next_attack(&char_id, now_ms + attack_interval_ms);

        let target_alive_in_loc = world.mob_location(&target).is_some_and(|l| l == loc);
        if !target_alive_in_loc {
            world.end_combat(&char_id);
            push_event(
                &mut plan,
                &char_id,
                "evt/combat/end",
                &CombatEnd {
                    reason: "цель недоступна".into(),
                },
            );
            continue;
        }
        let Some(hp) = world.damage_mob(&target, cfg.base_damage) else {
            world.end_combat(&char_id);
            push_event(
                &mut plan,
                &char_id,
                "evt/combat/end",
                &CombatEnd {
                    reason: "цель недоступна".into(),
                },
            );
            continue;
        };
        let mob_name = world
            .mob(&target)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let mob_max = world.mob_max_hp(&target);
        let source_name = world.char_name(&char_id).unwrap_or_default();

        push_event(
            &mut plan,
            &char_id,
            "evt/combat/hit",
            &CombatHit {
                source: char_id.clone(),
                source_name,
                target: target.clone(),
                target_name: mob_name.clone(),
                kind: CombatKind::Melee,
                amount: cfg.base_damage,
                crit: false,
                miss: false,
                text: format!("Вы бьёте {mob_name}: {}", cfg.base_damage),
                extra: Default::default(),
            },
        );
        push_event(
            &mut plan,
            &char_id,
            "evt/entity/hp",
            &EntityHp {
                entity_id: target.clone(),
                hp: Hp {
                    cur: hp,
                    max: mob_max,
                },
            },
        );

        if hp == 0 {
            // Общий путь смерти моба (атака и заклинания, M12.5b).
            if let Some(kill) =
                mob_kill_effects(world, &mut plan.events, &target, &loc, &char_id, skills_cfg)
            {
                plan.xp.push(kill);
            }
        }
    }

    // ── атаки агрессивных мобов (по локациям с персонажами) ────────────
    let aggro_targets: Vec<(String, String)> = world
        .locations_with_players()
        .into_iter()
        .flat_map(|loc| {
            world
                .aggro_mobs_in(&loc)
                .into_iter()
                .map(|mob| (mob, loc.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    for (mob_id, loc) in aggro_targets {
        if world.mob_next_attack(&mob_id).unwrap_or(0) > now_ms {
            continue;
        }
        world.set_mob_next_attack(&mob_id, now_ms + attack_interval_ms);
        let Some(victim) = world.chars_in_location(&loc).into_iter().next() else {
            continue;
        };
        let amount = world.mob_damage(&mob_id);
        if amount <= 0 {
            continue;
        }
        let mob_name = world
            .mob(&mob_id)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let victim_name = world.char_name(&victim).unwrap_or_default();
        push_event(
            &mut plan,
            &victim,
            "evt/combat/hit",
            &CombatHit {
                source: mob_id.clone(),
                source_name: mob_name.clone(),
                target: victim.clone(),
                target_name: victim_name,
                kind: CombatKind::Melee,
                amount,
                crit: false,
                miss: false,
                text: format!("{mob_name} бьёт вас: {amount}"),
                extra: Default::default(),
            },
        );
        plan.mob_hits.push(MobHit {
            char_id: victim,
            amount,
        });
    }

    plan
}

/// События смерти моба (деспан/труп всем в локации + бой окончен) и XP
/// убийства — общий путь для атаки и заклинаний (M12.5b). События — в
/// `plan`, XP — возвращается.
pub(crate) fn mob_kill_effects(
    world: &mut World,
    plan: &mut Vec<(String, String)>,
    target: &str,
    loc: &str,
    attacker: &str,
    skills_cfg: &SkillsSection,
) -> Option<KillXp> {
    let difficulty = world.mob(target).map(|m| m.difficulty).unwrap_or(1);
    let amount = skills_cfg.kill_xp_base + difficulty * skills_cfg.kill_xp_per_difficulty;
    let corpse_id = world.kill_mob(target);
    for addr in world.chars_in_location(loc) {
        push_text(
            plan,
            &addr,
            "evt/entity/despawn",
            &EntityDespawn {
                entity_id: target.to_string(),
                location_id: loc.to_string(),
            },
        );
        if let Some(corpse_id) = &corpse_id {
            if let Some(corpse) = world.corpse(corpse_id) {
                let loot_count = corpse.loot.iter().filter(|l| l.is_some()).count() as u32;
                push_text(
                    plan,
                    &addr,
                    "evt/corpse/created",
                    &CorpseCreated {
                        entity: corpse.entity(),
                        loot_count,
                    },
                );
            }
        }
    }
    world.end_combat(attacker);
    push_text(
        plan,
        attacker,
        "evt/combat/end",
        &CombatEnd {
            reason: "цель уничтожена".into(),
        },
    );
    Some(KillXp {
        char_id: attacker.to_string(),
        amount,
        skill: None,
        target: Some(target.to_string()),
    })
}

fn push_event<T: serde::Serialize>(plan: &mut Plan, addr: &str, ty: &str, payload: &T) {
    let text = event_text(ty, payload);
    plan.events.push((addr.to_string(), text));
}

fn push_text<T: serde::Serialize>(
    plan: &mut Vec<(String, String)>,
    addr: &str,
    ty: &str,
    payload: &T,
) {
    let text = event_text(ty, payload);
    plan.push((addr.to_string(), text));
}

fn event_text<T: serde::Serialize>(ty: &str, payload: &T) -> String {
    crate::session::event_text(ty, None, serde_json::to_value(payload).unwrap_or_default())
}

/// Урон моба персонажу (БД, D27): урон → `evt/char/stats`; 0 hp → общий
/// поток смерти ФР-06 + синхронизация позиции в мире.
async fn apply_mob_hit(state: &SharedState, hit: &MobHit) -> Vec<(String, String)> {
    let pool = &state.pool;
    let row = match crate::db::characters::get(pool, &hit.char_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Vec::new(),
        Err(e) => {
            warn!("тик (персонаж {}): {e}", hit.char_id);
            return Vec::new();
        }
    };
    let new_hp = (i64::from(row.hp_cur) - hit.amount).max(0);
    if new_hp == 0 {
        // Смерть: позиция после респауна — источник истины для мира.
        let events = crate::death::kill_character(state, &hit.char_id).await;
        if let Ok(Some(row)) = crate::db::characters::get(pool, &hit.char_id).await {
            let mut world = state.world.lock().expect("мир");
            world.set_char_location(&hit.char_id, &row.location_id);
            world.end_combat(&hit.char_id);
        }
        return events;
    }
    if let Err(e) = crate::db::characters::set_hp(pool, &hit.char_id, new_hp as i32).await {
        warn!("тик (hp {}): {e}", hit.char_id);
        return Vec::new();
    }
    let mut patch = serde_json::Map::new();
    patch.insert("hp".into(), json!({ "cur": new_hp, "max": row.hp_max }));
    let text = crate::session::event_text(
        "evt/char/stats",
        None,
        serde_json::to_value(CharStats { patch }).unwrap_or_default(),
    );
    vec![(hit.char_id.clone(), text)]
}

/// Применить XP убийства: навык оружия персонажа (экипировка → `item_defs.skill`,
/// иначе `unarmed`) либо заданная школа (заклинания); ранг — с ограничением
/// гейтов (systems::apply_kill_xp). `Some(text)` — событие `evt/char/xp`.
pub(crate) async fn grant_kill_xp(state: &SharedState, work: &KillXp) -> Option<String> {
    let pool = &state.pool;
    let skill_id = match &work.skill {
        Some(skill) => skill.clone(),
        None => {
            // Навык: экипированное оружие с `skill` (skills.md §2.3).
            sqlx::query_scalar(
                "SELECT COALESCE( \
                    (SELECT d.skill FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND i.equipped_slot IS NOT NULL \
                       AND d.skill IS NOT NULL \
                     ORDER BY i.created_at LIMIT 1), \
                    'unarmed')",
            )
            .bind(&work.char_id)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| {
                warn!("тик (навык оружия {}): {e}", work.char_id);
                "unarmed".to_string()
            })
        }
    };

    let (xp, rank, max_attr) = match (
        crate::db::skills::learned_one(pool, &work.char_id, &skill_id).await,
        crate::db::characters::get(pool, &work.char_id).await,
    ) {
        (Ok(learned), Ok(Some(row))) => {
            let attrs: std::collections::BTreeMap<String, i64> =
                serde_json::from_value(row.attrs).unwrap_or_default();
            let max_attr = attrs.values().max().copied().unwrap_or(0);
            let (xp, rank) = learned.unwrap_or((0, 0));
            (xp, rank, max_attr)
        }
        (Err(e), _) | (_, Err(e)) => {
            warn!("тик (XP {}): {e}", work.char_id);
            return None;
        }
        _ => return None,
    };

    let (new_xp, new_rank) =
        crate::systems::apply_kill_xp(xp, rank, work.amount, &state.config.skills, max_attr);
    if new_xp == xp && new_rank == rank {
        return None; // без изменений — не пишем
    }
    if let Err(e) =
        crate::db::skills::grant_tx(pool, &work.char_id, &skill_id, new_xp, new_rank).await
    {
        warn!("тик (XP-вклад {}): {e}", work.char_id);
        return None;
    }
    // evt/char/xp — навыку; pool_left не трогаем (вклад идёт напрямую).
    let need = crate::systems::xp_to_next(new_rank.max(0) as u32, &state.config.skills);
    Some(event_text(
        "evt/char/xp",
        &ld2_protocol::evt::CharXp {
            skill_id,
            amount: work.amount,
            skill_xp: new_xp,
            rank: Some(new_rank.max(0) as u32),
            xp_to_next: Some(need),
            pool_left: None,
        },
    ))
}

/// Мана-реген раз в секунту (skills.md §2.4, конфиг `[gameplay.magic.mana]`):
/// вне боя — полный бар за `regen_out_s`, в бою — `regen_in_pct` %/с.
/// Дробная мана округляется вверх — v1-допущение (отклонение M12.5b).
async fn regen_step(state: &SharedState) {
    let mana = &state.config.gameplay.magic.mana;
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
        let max = i64::from(row.mp_max);
        let cur = i64::from(row.mp_cur);
        if cur >= max {
            continue; // полная мана — молчим (без шума каждый тик)
        }
        let delta = if in_combat {
            (f64::from(row.mp_max) * mana.regen_in_pct_s / 100.0)
                .ceil()
                .max(1.0) as i64
        } else {
            (f64::from(row.mp_max) / (mana.regen_out_s.max(1) as f64))
                .ceil()
                .max(1.0) as i64
        };
        let new = (cur + delta).min(max);
        if new <= cur {
            continue;
        }
        if let Err(e) = crate::db::characters::set_mp(&state.pool, char_id, new as i32).await {
            warn!("реген (mp {}): {e}", char_id);
            continue;
        }
        let mut patch = serde_json::Map::new();
        patch.insert("mp".into(), json!({ "cur": new, "max": max }));
        let text = crate::session::event_text(
            "evt/char/stats",
            None,
            serde_json::to_value(CharStats { patch }).unwrap_or_default(),
        );
        let world = state.world.lock().expect("мир");
        world.send_to_char(char_id, text);
    }
}

/// Истечение статусов преступления раз в секунду (ФР-11, §10): снятие
/// просроченных → `evt/crime/removed` адресату (подстраховка клиентского
/// fallback по `expires_at`).
async fn crime_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let online: Vec<String> = {
        let world = state.world.lock().expect("мир");
        world.online_chars()
    };
    for char_id in online {
        let expired = match crate::db::crime::expire(&state.pool, &char_id, now).await {
            Ok(kinds) => kinds,
            Err(e) => {
                warn!("crime (истечение {char_id}): {e}");
                continue;
            }
        };
        for kind in expired {
            let text = crate::session::event_text(
                "evt/crime/removed",
                None,
                serde_json::json!({ "kind": kind }),
            );
            if text.is_empty() {
                continue;
            }
            let world = state.world.lock().expect("мир");
            world.send_to_char(&char_id, text);
        }
    }
}

/// Готовые крафт-джобы раз в секунду (M12.6): выдача предметов + XP
/// профессии → `evt/craft/result`, `evt/item/added`, `evt/craft/profession`.
/// Джоб забирается атомарно (`take_job`) — рестарт не дублирует выдачу (D27).
async fn craft_finish_step(state: &SharedState) {
    let now = crate::time::now_epoch() as f64;
    let jobs = match crate::db::craft::ready_jobs(&state.pool, now).await {
        Ok(jobs) => jobs,
        Err(e) => {
            warn!("крафт (джобы): {e}");
            return;
        }
    };
    for (job_id, char_id, recipe_id, total_qty, xp) in jobs {
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
                "evt/craft/result",
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
async fn recipe_output_type(state: &SharedState, recipe_id: &str) -> String {
    crate::db::craft::recipe(&state.pool, recipe_id)
        .await
        .ok()
        .flatten()
        .map(|r| r.output_type)
        .unwrap_or_else(|| recipe_id.to_string())
}

/// XP в профессию рецепта с повышением ранга (skills.md §5.1) → событие
/// `evt/craft/profession` (или `None`).
async fn profession_xp_step(
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
    let (mut xp, mut rank) = learned
        .iter()
        .find(|(id, _, _)| *id == recipe.profession)
        .map(|(_, x, r)| (*x, *r))
        .unwrap_or((0, 1));
    xp += gain;
    let cfg = &state.config.gameplay.professions;
    // Рост ранга, пока хватает порога и не достигнут потолок.
    loop {
        if rank as u32 >= cfg.rank_max {
            break;
        }
        let need = ((cfg.xp_to_next_base as f64) * cfg.xp_growth.powi(rank)).round() as i64;
        if xp < need {
            break;
        }
        xp -= need;
        rank += 1;
    }
    if let Err(e) =
        crate::db::professions::grant_xp(&state.pool, char_id, &recipe.profession, xp, rank).await
    {
        warn!("крафт (XP профессии {char_id}): {e}");
        return None;
    }
    let rank_u32 = rank.max(1) as u32;
    let xp_to_next = if rank_u32 >= cfg.rank_max {
        0
    } else {
        ((cfg.xp_to_next_base as f64) * cfg.xp_growth.powi(rank)).round() as i64
    };
    let snapshot = ld2_protocol::types::ProfessionSnapshot {
        id: recipe.profession.clone(),
        rank: rank_u32,
        xp,
        xp_to_next,
        extra: Default::default(),
    };
    let text = crate::session::event_text(
        "evt/craft/profession",
        None,
        serde_json::to_value(ld2_protocol::evt::CraftProfession {
            profession: snapshot,
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
            "evt/quest/updated"
        } else {
            "evt/quest/progress"
        };
        let text = crate::session::event_text(ty, None, serde_json::json!({ "quest": state_obj }));
        if !text.is_empty() {
            events.push((char_id.to_string(), text));
        }
    }
    events
}
