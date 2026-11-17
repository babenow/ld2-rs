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
async fn apply_rune_work(state: &SharedState, work: &[RuneWork]) {
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

/// M18/F58d: отложенная работа авто-приёма руны — применяется в
/// `tick_once` (у плана нет доступа к БД): расход зарядов + бафф.
struct RuneWork {
    char_id: String,
    /// uid оружия-носителя (запись charges).
    staff_uid: String,
    socket_index: u32,
    new_charges: i64,
    /// Наложить бафф персонажа (`stats.buff` руны).
    buff: Option<crate::world::RuneBuff>,
}

#[derive(Default)]
struct Plan {
    /// (адресат — char_id, JSON конверта).
    events: Vec<(String, String)>,
    mob_hits: Vec<MobHit>,
    xp: Vec<KillXp>,
    /// M17/F30e: нанесённый персонажами урон → прирост ярости (БД
    /// применяется в `tick_once`, у плана нет доступа к БД).
    char_hits: Vec<(String, i64)>,
    /// M18/F58d: сработавшие авто-приёмы рун (БД — в `tick_once`).
    rune_work: Vec<RuneWork>,
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
        // M17/F30g: период атаки — cooldown_s оружия, иначе конфиг.
        let interval = world.char_attack_ms(&char_id).unwrap_or(attack_interval_ms);
        world.set_next_attack(&char_id, now_ms + interval);

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
        // M17/F30e: нанесённый урон → ярость (применяется в tick_once).
        plan.char_hits.push((char_id.clone(), cfg.base_damage));
        let mob_name = world
            .mob(&target)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let mob_max = world.mob_max_hp(&target);
        let source_name = world.char_name(&char_id).unwrap_or_default();

        // M17/F30f: строка удара — «вы» атакующему, имя — наблюдателям;
        // «чем бьёт» — кэш экипированного оружия в мире.
        let hits_as = world.char_hits_as(&char_id);
        let mk_hit = |text: String| CombatHit {
            source: char_id.clone(),
            source_name: source_name.clone(),
            target: target.clone(),
            target_name: mob_name.clone(),
            kind: CombatKind::Melee,
            amount: cfg.base_damage,
            crit: false,
            miss: false,
            text,
            extra: Default::default(),
        };
        push_event(
            &mut plan,
            &char_id,
            "evt/combat/hit",
            &mk_hit(crate::combat_log::melee_hit_text(
                &source_name,
                &mob_name,
                hits_as,
                cfg.base_damage,
                true,
            )),
        );
        for addr in world.chars_in_location(&loc) {
            if addr == char_id {
                continue;
            }
            push_event(
                &mut plan,
                &addr,
                "evt/combat/hit",
                &mk_hit(crate::combat_log::melee_hit_text(
                    &source_name,
                    &mob_name,
                    hits_as,
                    cfg.base_damage,
                    false,
                )),
            );
        }
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

        // M18/F58d (ADR-012 §5): авто-приёмы рун — шанс при ударе
        // (кэш рун экипированного оружия); урон + отложенная работа
        // (расход зарядов, бафф) в `Plan::rune_work` — БД в `tick_once`.
        let mut mob_hp = hp;
        let mut killed_by_rune = false;
        if mob_hp > 0 {
            let procs = world.char_runes(&char_id).to_vec();
            for rune in procs {
                if rune.charges < rune.cost {
                    continue; // нет зарядов → не срабатывает (ADR-012 §5)
                }
                let mut buf = [0u8; 8];
                // [0,1): u64::MAX + 1.0 как знаменатель (иначе 1.0 ∈ диапазону).
                let roll = if getrandom::fill(&mut buf).is_ok() {
                    (u64::from_le_bytes(buf) as f64) / (u64::MAX as f64 + 1.0)
                } else {
                    1.0 // нет CSPRNG → не срабатывает
                };
                if !crate::systems::rune_proc_roll(rune.chance, roll) {
                    continue;
                }
                // Сработало: заряды — сразу в кэш, БД/бафф — в tick_once.
                let new_charges = rune.charges - rune.cost;
                world.spend_char_rune(&char_id, rune.socket_index, new_charges);
                plan.rune_work.push(RuneWork {
                    char_id: char_id.clone(),
                    staff_uid: rune.staff_uid.clone(),
                    socket_index: rune.socket_index,
                    new_charges,
                    buff: rune.buff.clone(),
                });
                // M18/F58d: эффект на цель (стен/кровотечение/корни) —
                // состояние мира, применяется сразу в плане.
                if let Some(effect) = &rune.effect {
                    world.apply_rune_effect(
                        &target,
                        &effect.kind,
                        effect.value,
                        effect.duration_s,
                        &char_id,
                        now_ms,
                    );
                }
                let Some(new_hp) = world.damage_mob(&target, rune.damage) else {
                    break; // цель исчезла
                };
                mob_hp = new_hp;
                // Строка рунного удара — как при касте (§8).
                let rune_hit = |text: String| CombatHit {
                    source: char_id.clone(),
                    source_name: source_name.clone(),
                    target: target.clone(),
                    target_name: mob_name.clone(),
                    kind: CombatKind::Rune,
                    amount: rune.damage,
                    crit: false,
                    miss: false,
                    text,
                    extra: Default::default(),
                };
                push_event(
                    &mut plan,
                    &char_id,
                    "evt/combat/hit",
                    &rune_hit(crate::combat_log::spell_hit_text(
                        &rune.spell,
                        &source_name,
                        &mob_name,
                        rune.damage,
                        true,
                    )),
                );
                for addr in world.chars_in_location(&loc) {
                    if addr == char_id {
                        continue;
                    }
                    push_event(
                        &mut plan,
                        &addr,
                        "evt/combat/hit",
                        &rune_hit(crate::combat_log::spell_hit_text(
                            &rune.spell,
                            &source_name,
                            &mob_name,
                            rune.damage,
                            false,
                        )),
                    );
                }
                push_event(
                    &mut plan,
                    &char_id,
                    "evt/entity/hp",
                    &EntityHp {
                        entity_id: target.clone(),
                        hp: Hp {
                            cur: mob_hp,
                            max: mob_max,
                        },
                    },
                );
                if mob_hp == 0 {
                    // Руна добила — общий путь смерти (M12.5b).
                    killed_by_rune = true;
                    if let Some(kill) = mob_kill_effects(
                        world,
                        &mut plan.events,
                        &target,
                        &loc,
                        &char_id,
                        skills_cfg,
                    ) {
                        plan.xp.push(kill);
                    }
                    break;
                }
            }
        }

        if mob_hp == 0 && !killed_by_rune {
            // Общий путь смерти моба (атака и заклинания, M12.5b).
            if let Some(kill) =
                mob_kill_effects(world, &mut plan.events, &target, &loc, &char_id, skills_cfg)
            {
                plan.xp.push(kill);
            }
        }
    }

    // ── M18/F58d: кровотечение (DoT рун) — тик раз в секунду ──────────
    for (mob_id, dps, _next_at, src) in world.bleeding_mobs(now_ms) {
        let Some((loc, mob_name)) = world.mob_loc_name(&mob_id) else {
            continue;
        };
        world.advance_bleed_tick(&mob_id, now_ms);
        let Some(hp) = world.damage_mob(&mob_id, dps) else {
            continue;
        };
        // Строка DoT — всем в локации.
        let dot_hit = CombatHit {
            source: src.clone().unwrap_or_default(),
            source_name: src
                .as_ref()
                .and_then(|id| world.char_name(id))
                .unwrap_or_default(),
            target: mob_id.clone(),
            target_name: mob_name.clone(),
            kind: CombatKind::Rune,
            amount: dps,
            crit: false,
            miss: false,
            text: format!("{mob_name} кровоточит: **{dps}**"),
            extra: Default::default(),
        };
        for addr in world.chars_in_location(&loc) {
            push_event(&mut plan, &addr, "evt/combat/hit", &dot_hit);
        }
        push_event(
            &mut plan,
            &src.clone().unwrap_or_default(),
            "evt/entity/hp",
            &EntityHp {
                entity_id: mob_id.clone(),
                hp: Hp {
                    cur: hp,
                    max: world.mob_max_hp(&mob_id),
                },
            },
        );
        if hp == 0 {
            // Добил DoT — XP и квесты источнику руны.
            if let Some(attacker) = src.as_deref() {
                if let Some(kill) =
                    mob_kill_effects(world, &mut plan.events, &mob_id, &loc, attacker, skills_cfg)
                {
                    plan.xp.push(kill);
                }
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
        // M18/F58d: стан руной — моб не атакует, пока не истёк.
        if world.mob_stunned(&mob_id, now_ms) {
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
                text: format!("{mob_name} бьёт вас: **{amount}**"),
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
                kind: Some(ld2_protocol::types::EntityKind::Monster),
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
/// M17/F30e: прирост ярости воина за нанесённый урон — БД + патч mp
/// адресату; не-воин/без изменений → `None`.
async fn apply_rage_dealt(
    state: &SharedState,
    char_id: &str,
    damage: i64,
) -> Option<(String, String)> {
    let row = crate::db::characters::get(&state.pool, char_id)
        .await
        .ok()
        .flatten()?;
    let res = &state.config.gameplay.resource;
    let profile = row.profile.as_str();
    let gain = crate::systems::rage_gain(res, profile, damage);
    if gain <= 0 {
        return None;
    }
    let max = crate::systems::resource_max(profile, res, i64::from(row.mp_max));
    let cur = i64::from(row.mp_cur).min(max);
    let new = (cur + gain).min(max);
    if new <= cur {
        return None;
    }
    if let Err(e) = crate::db::characters::set_mp(&state.pool, char_id, new as i32).await {
        warn!("ярость (нанесённый урон {char_id}): {e}");
        return None;
    }
    let mut patch = serde_json::Map::new();
    patch.insert("mp".into(), json!({ "cur": new, "max": max }));
    let text = crate::session::event_text(
        "evt/char/stats",
        None,
        serde_json::to_value(CharStats { patch }).unwrap_or_default(),
    );
    Some((char_id.to_string(), text))
}

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
    // M17/F30e: ярость воина за полученный урон — в тот же патч mp.
    if row.profile == "warrior" {
        let res = &state.config.gameplay.resource;
        let max = crate::systems::resource_max("warrior", res, i64::from(row.mp_max));
        let cur = i64::from(row.mp_cur).min(max);
        let new = (cur + crate::systems::rage_gain(res, "warrior", hit.amount)).min(max);
        if new > cur {
            match crate::db::characters::set_mp(pool, &hit.char_id, new as i32).await {
                Ok(()) => {
                    patch.insert("mp".into(), json!({ "cur": new, "max": max }));
                }
                Err(e) => warn!("ярость (полученный урон {}): {e}", hit.char_id),
            }
        }
    }
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

    let (xp, rank, max_attr, total_xp, char_name, gender) = match (
        crate::db::skills::learned_one(pool, &work.char_id, &skill_id).await,
        crate::db::characters::get(pool, &work.char_id).await,
    ) {
        (Ok(learned), Ok(Some(row))) => {
            let attrs: std::collections::BTreeMap<String, i64> =
                serde_json::from_value(row.attrs).unwrap_or_default();
            let max_attr = attrs.values().max().copied().unwrap_or(0);
            let (xp, rank) = learned.unwrap_or((0, 0));
            (xp, rank, max_attr, row.total_xp, row.name, row.gender)
        }
        (Err(e), _) | (_, Err(e)) => {
            warn!("тик (XP {}): {e}", work.char_id);
            return None;
        }
        _ => return None,
    };

    // M15/F24: общий опыт растёт при каждом убийстве — даже когда
    // навык на гейте и его XP не изменился.
    if work.amount > 0 {
        if let Err(e) = crate::db::characters::add_total_xp(pool, &work.char_id, work.amount).await
        {
            warn!("тик (total_xp {}): {e}", work.char_id);
        }
        // M22: новый уровень персонажа — анонс в локации (своему
        // персонажу клиент пишет «Вы получили…» по char_id).
        let level_cfg = &state.config.gameplay.level;
        let (old_level, ..) = crate::systems::level_for_total(total_xp, level_cfg);
        let (new_level, ..) = crate::systems::level_for_total(total_xp + work.amount, level_cfg);
        if new_level > old_level {
            let payload = ld2_protocol::evt::CharLevelUp {
                char_id: work.char_id.clone(),
                name: char_name.clone(),
                gender: gender.clone(),
                level: new_level,
            };
            let text = crate::session::event_text(
                "evt/char/level_up",
                None,
                serde_json::to_value(payload).unwrap_or_default(),
            );
            if !text.is_empty() {
                let world = state.world.lock().expect("мир");
                if let Some(loc) = world.char_location(&work.char_id) {
                    for addr in world.chars_in_location(&loc) {
                        world.send_to_char(&addr, text.clone());
                    }
                }
            }
        }
    }

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

/// M18/F58c (ADR-012 §4): восстановление зарядов рун — по аналогии с
/// `regen_step`/`resource.energy_regen_s`: только онлайн-персонажи,
/// все их предметы с сокетами; `steps` — сколько зарядов начислить
/// (шаг аккумулятора ≥ 1 с — валидация конфига). Начисляются только
/// руны с `charges < max`, до потолка; изменения — CAS-записью (не
/// затираем параллельный каст/вставку) и патчем `evt/item/state`
/// владельцу (сокеты в wire-виде — с `accepts`, §6).
async fn rune_recharge_step(state: &SharedState, steps: u32) {
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
            "evt/item/state",
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
async fn regen_step(state: &SharedState) {
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
        let Some(new) = crate::systems::resource_step(
            profile,
            i64::from(row.mp_cur),
            max,
            in_combat,
            &state.config,
        ) else {
            continue; // без изменений — без шума каждый тик
        };
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

/// M18/F58d (ADR-012 §6): снятие истёкших баффов рун раз в секунду —
/// M20: респавны по сроку (очередь `World`): моб возвращается из
/// шаблона сида со стартовым HP → `evt/entity/spawn`, его старый труп
/// уходит → `evt/entity/despawn` (kind=corpse); статический лут —
/// сброс `loot_taken` в БД (без события — клиент осматривает заново).
async fn respawn_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let due = {
        let mut world = state.world.lock().expect("мир");
        world.due_respawns(now)
    };
    for entry in due {
        match entry.kind {
            crate::world::RespawnKind::Mob { corpse_id } => {
                let (entity, corpse_id) = {
                    let mut world = state.world.lock().expect("мир");
                    let entity = world.respawn_mob(&entry.entity_id);
                    let corpse = corpse_id
                        .as_deref()
                        .and_then(|id| world.remove_corpse(id))
                        .map(|c| c.id);
                    (entity, corpse)
                };
                let Some(entity) = entity else {
                    continue; // нет шаблона (не должен случиться)
                };
                let world = state.world.lock().expect("мир");
                // Сначала старый труп (вместе с лутом), затем моб.
                if let Some(corpse_id) = corpse_id {
                    let text = event_text(
                        "evt/entity/despawn",
                        &ld2_protocol::evt::EntityDespawn {
                            entity_id: corpse_id,
                            location_id: entry.location_id.clone(),
                            kind: Some(ld2_protocol::types::EntityKind::Corpse),
                        },
                    );
                    for addr in world.chars_in_location(&entry.location_id) {
                        world.send_to_char(&addr, text.clone());
                    }
                }
                let text = event_text(
                    "evt/entity/spawn",
                    &ld2_protocol::evt::EntitySpawn {
                        entity,
                        location_id: entry.location_id.clone(),
                    },
                );
                for addr in world.chars_in_location(&entry.location_id) {
                    world.send_to_char(&addr, text.clone());
                }
            }
            crate::world::RespawnKind::Loot => {
                if let Err(e) =
                    crate::db::items::loot_taken_reset(&state.pool, &entry.entity_id).await
                {
                    warn!("респавн лута ({}): {e}", entry.entity_id);
                }
            }
            crate::world::RespawnKind::Resource => {
                let restored = {
                    let mut world = state.world.lock().expect("мир");
                    world.respawn_resource(&entry.entity_id)
                };
                let Some((node, was_hidden)) = restored else {
                    continue; // нет узла (не должен случиться)
                };
                let world = state.world.lock().expect("мир");
                if was_hidden {
                    // Трава исчезала → возвращается видимой.
                    let text = event_text(
                        "evt/entity/spawn",
                        &ld2_protocol::evt::EntitySpawn {
                            entity: node.entity(),
                            location_id: entry.location_id.clone(),
                        },
                    );
                    for addr in world.chars_in_location(&entry.location_id) {
                        world.send_to_char(&addr, text.clone());
                    }
                } else {
                    // Жилы/деревья остаются — только патч шкалы запаса.
                    let mut patch = serde_json::Map::new();
                    patch.insert(
                        "hp".into(),
                        serde_json::json!({"cur": node.current, "max": node.max}),
                    );
                    let text = event_text(
                        "evt/entity/move",
                        &ld2_protocol::evt::EntityMove {
                            entity_id: node.id.clone(),
                            patch,
                        },
                    );
                    for addr in world.chars_in_location(&entry.location_id) {
                        world.send_to_char(&addr, text.clone());
                    }
                }
            }
        }
    }
}

/// Снятие шкур: готовые разделки → лут в труп (шкуры — в общий
/// список с качеством по рангу и критом, мясо/потроха — в отдельный
/// «Внутри»), XP только за снятие шкуры (формула/множители — M22,
/// docs/professions.md §3), итог персонажу `evt/harvest/result` и
/// обновление трупа (`evt/corpse/created` — апсерт у клиента).
async fn harvest_step(state: &SharedState) {
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
        let (level, rank) = learned
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
        };
        let text = crate::session::event_text("evt/harvest/result", None, {
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
                                "evt/craft/profession",
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
        let text = crate::session::event_text("evt/corpse/created", None, {
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
fn job_corpse_location(world: &crate::world::World, corpse_id: &str) -> String {
    world
        .corpse(corpse_id)
        .map(|c| c.location_id.clone())
        .unwrap_or_default()
}

/// M20: выброшенные предметы истекают по `drop_ttl_s` → «{name} исчез»
/// всем в локации (`ttl <= 0` — выключено).
async fn ground_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let ttl = state.config.world.drop_ttl_s;
    if ttl <= 0 {
        return;
    }
    let expired: Vec<(String, ld2_protocol::evt::ItemExpired)> = {
        let mut world = state.world.lock().expect("мир");
        world
            .due_ground(now, ttl)
            .into_iter()
            .map(|g| {
                (
                    g.location_id.clone(),
                    ld2_protocol::evt::ItemExpired {
                        uid: g.id,
                        name: g.name,
                    },
                )
            })
            .collect()
    };
    for (loc, payload) in expired {
        let text = event_text("evt/item/expired", &payload);
        let world = state.world.lock().expect("мир");
        for addr in world.chars_in_location(&loc) {
            world.send_to_char(&addr, text.clone());
        }
    }
}

/// M20: трупы старше `corpse_ttl_s` исчезают (вместе с лутом) —
/// `evt/entity/despawn` (kind=corpse → «{name} исчезает»).
async fn corpse_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let ttl = state.config.world.corpse_ttl_s;
    if ttl <= 0 {
        return;
    }
    let expired: Vec<(String, String)> = {
        let mut world = state.world.lock().expect("мир");
        world
            .due_corpses(now, ttl)
            .into_iter()
            .map(|c| (c.location_id.clone(), c.id))
            .collect()
    };
    for (loc, corpse_id) in expired {
        let payload = ld2_protocol::evt::EntityDespawn {
            entity_id: corpse_id,
            location_id: loc.clone(),
            kind: Some(ld2_protocol::types::EntityKind::Corpse),
        };
        let text = event_text("evt/entity/despawn", &payload);
        let world = state.world.lock().expect("мир");
        for addr in world.chars_in_location(&loc) {
            world.send_to_char(&addr, text.clone());
        }
    }
}

/// клиенту шлётся **полная замена** списка `evt/char/buffs` (паттерн
/// `evt/crime/status`); только онлайн-персонажи (как `regen_step`).
async fn buff_expire_step(state: &SharedState) {
    let now = crate::time::now_epoch();
    let online: Vec<String> = {
        let world = state.world.lock().expect("мир");
        world.online_chars()
    };
    for char_id in online {
        let expired = match crate::db::buffs::expire(&state.pool, &char_id, now).await {
            Ok(kinds) => kinds,
            Err(e) => {
                warn!("баффы (истечение {char_id}): {e}");
                continue;
            }
        };
        if expired.is_empty() {
            continue;
        }
        // Полная замена: оставшиеся активные баффы.
        let rest = match crate::db::buffs::active(&state.pool, &char_id, now).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("баффы (список {char_id}): {e}");
                continue;
            }
        };
        let buffs: Vec<ld2_protocol::types::BuffState> = rest
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
    let (xp, rank, level) = learned
        .iter()
        .find(|(id, _, _, _)| *id == recipe.profession)
        .map(|(_, x, r, l)| (*x, *r, *l))
        .unwrap_or((0, 1, 0));
    let cfg = &state.config.gameplay.professions;
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
        "evt/craft/profession",
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
