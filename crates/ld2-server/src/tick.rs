//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3). Адресация событий — через реестр
//! соединений мира (§2.4: порядок внутри канала сохраняется).

use std::time::Duration;

use ld2_protocol::evt::{
    CharStats, CombatEnd, CombatHit, CombatKind, CorpseCreated, EntityDespawn, EntityHp,
};
use ld2_protocol::types::Hp;
use serde_json::json;
use tracing::warn;

use crate::config::CombatSection;
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
    loop {
        interval.tick().await;
        tick_once(&state).await;
    }
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
    // 3) XP убийств — в БД и событием (D27).
    for work in &plan.xp {
        grant_kill_xp(state, work).await;
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

/// Запланированный XP убийства (навык определяется при применении — из БД).
#[derive(Debug)]
struct KillXp {
    char_id: String,
    amount: i64,
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
    skills_cfg: &crate::config::SkillsSection,
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
        let difficulty = world.mob(&target).map(|m| m.difficulty).unwrap_or(1);
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
            // Убийство: despawn всем в локации + труп с лутом + бой окончен.
            let corpse_id = world.kill_mob(&target);
            for addr in world.chars_in_location(&loc) {
                push_event(
                    &mut plan,
                    &addr,
                    "evt/entity/despawn",
                    &EntityDespawn {
                        entity_id: target.clone(),
                        location_id: loc.clone(),
                    },
                );
                if let Some(corpse_id) = &corpse_id {
                    if let Some(corpse) = world.corpse(corpse_id) {
                        let loot_count = corpse.loot.iter().filter(|l| l.is_some()).count() as u32;
                        push_event(
                            &mut plan,
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
            world.end_combat(&char_id);
            push_event(
                &mut plan,
                &char_id,
                "evt/combat/end",
                &CombatEnd {
                    reason: "цель уничтожена".into(),
                },
            );
            // skills.md §2.3: xp = base + difficulty × per; навык оружия
            // определяется при применении (grant_kill_xp, из БД).
            let amount = kill_xp_for(skills_cfg, difficulty);
            plan.xp.push(KillXp { char_id, amount });
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

/// XP за убийство: kill_xp_base + difficulty × kill_xp_per_difficulty.
fn kill_xp_for(cfg: &crate::config::SkillsSection, difficulty: i64) -> i64 {
    cfg.kill_xp_base + difficulty * cfg.kill_xp_per_difficulty
}

fn push_event<T: serde::Serialize>(plan: &mut Plan, addr: &str, ty: &str, payload: &T) {
    let text =
        crate::session::event_text(ty, None, serde_json::to_value(payload).unwrap_or_default());
    plan.events.push((addr.to_string(), text));
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
/// иначе `unarmed`), ранг — с ограничением гейтов (systems::apply_kill_xp).
async fn grant_kill_xp(state: &SharedState, work: &KillXp) {
    let pool = &state.pool;
    // Навык: экипированное оружие с `skill` (skills.md §2.3).
    let skill_id: String = sqlx::query_scalar(
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
    });

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
            return;
        }
        _ => return,
    };

    let (new_xp, new_rank) =
        crate::systems::apply_kill_xp(xp, rank, work.amount, &state.config.skills, max_attr);
    if new_xp == xp && new_rank == rank {
        return; // без изменений — не пишем
    }
    if let Err(e) =
        crate::db::skills::grant_tx(pool, &work.char_id, &skill_id, new_xp, new_rank).await
    {
        warn!("тик (XP-вклад {}): {e}", work.char_id);
        return;
    }
    // evt/char/xp — навыку; pool_left не трогаем (вклад идёт напрямую).
    let need = crate::systems::xp_to_next(new_rank.max(0) as u32, &state.config.skills);
    let text = crate::session::event_text(
        "evt/char/xp",
        None,
        serde_json::to_value(ld2_protocol::evt::CharXp {
            skill_id,
            amount: work.amount,
            skill_xp: new_xp,
            rank: Some(new_rank.max(0) as u32),
            xp_to_next: Some(need),
            pool_left: None,
        })
        .unwrap_or_default(),
    );
    let world = state.world.lock().expect("мир");
    world.send_to_char(&work.char_id, text);
}
