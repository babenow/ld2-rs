//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).
//!
//! Разложено по фазам: `run` (цикл и применение плана), `combat` (план
//! боя, смерти, награды), `jobs` (работы, DoT, баффы), `expire`
//! (истечение сущностей), `plan` (структуры плана и события).

use ld2_protocol::evt::{
    CharStats, CombatEnd, CombatHit, CombatKind, CorpseCreated, EntityDespawn, EntityHp,
};
use ld2_protocol::types::Hp;
use serde_json::json;
use tracing::warn;

use crate::config::{CombatSection, SkillsSection};
use crate::state::SharedState;
use crate::world::World;

use super::plan::{event_text, push_event, push_text, KillXp, MobHit, Plan, RuneWork};

/// M32: исход одного физического удара (без событий — их шлёт вызывающая
/// точка).
enum MeleeOutcome {
    Miss,
    Hit { damage: i64, parried: bool },
}

/// M32: бросок атаки персонажа по мобу: точность против уклонения,
/// затем урон листа, парирование моба (½) и плоская броня.
fn char_melee_outcome(
    world: &mut World,
    char_id: &str,
    target: &str,
    cfg: &CombatSection,
) -> MeleeOutcome {
    let combat = world
        .char_combat(char_id)
        .unwrap_or(crate::world::CharCombat {
            damage_min: cfg.base_damage,
            damage_max: cfg.base_damage,
            accuracy: 80,
            dodge: 0,
            parry: 0,
            armor: 0,
        });
    let (mob_armor, mob_dodge, mob_parry, _, _) = world.mob_defense(target);
    if !crate::systems::hit_rolls(combat.accuracy, mob_dodge, world.roll_pct()) {
        return MeleeOutcome::Miss;
    }
    let mut damage =
        crate::systems::roll_damage(combat.damage_min, combat.damage_max, world.roll01());
    let parried = world.roll_pct() < mob_parry as f64;
    if parried {
        damage = crate::systems::after_parry(damage);
    }
    MeleeOutcome::Hit {
        damage: crate::systems::after_armor(damage, mob_armor),
        parried,
    }
}

/// M32: бросок атаки моба по персонажу: точность моба против уклонения
/// игрока, его парирование (½) и броня.
fn mob_melee_outcome(
    world: &mut World,
    mob_id: &str,
    victim: &str,
    damage_mult: f64,
) -> MeleeOutcome {
    let (_, _, _, _, mob_accuracy) = world.mob_defense(mob_id);
    let combat = world
        .char_combat(victim)
        .unwrap_or(crate::world::CharCombat {
            damage_min: 0,
            damage_max: 0,
            accuracy: 80,
            dodge: 0,
            parry: 0,
            armor: 0,
        });
    if !crate::systems::hit_rolls(mob_accuracy, combat.dodge, world.roll_pct()) {
        return MeleeOutcome::Miss;
    }
    let mut amount = ((world.mob_damage(mob_id) as f64) * damage_mult).round() as i64;
    let parried = world.roll_pct() < combat.parry as f64;
    if parried {
        amount = crate::systems::after_parry(amount);
    }
    MeleeOutcome::Hit {
        damage: crate::systems::after_armor(amount, combat.armor),
        parried,
    }
}

pub(super) fn plan_combat(
    world: &mut World,
    now_ms: i64,
    cfg: &CombatSection,
    skills_cfg: &SkillsSection,
) -> Plan {
    let mut plan = Plan::default();
    let attack_interval_ms = ((cfg.attack_interval_s * 1000.0).round() as i64).max(1);

    plan_char_attacks(
        &mut plan,
        world,
        now_ms,
        cfg,
        skills_cfg,
        attack_interval_ms,
    );
    plan_bleed_ticks(&mut plan, world, now_ms, skills_cfg);
    plan_burn_ticks(&mut plan, world, now_ms, skills_cfg);
    plan_mob_attacks(&mut plan, world, now_ms, cfg, attack_interval_ms);

    plan
}
fn plan_char_attacks(
    plan: &mut Plan,
    world: &mut World,
    now_ms: i64,
    cfg: &CombatSection,
    skills_cfg: &SkillsSection,
    attack_interval_ms: i64,
) {
    for (char_id, target, loc, next_at) in world.combat_list() {
        if now_ms < next_at {
            continue;
        }
        char_attack(
            plan,
            world,
            now_ms,
            cfg,
            skills_cfg,
            attack_interval_ms,
            &char_id,
            &target,
            &loc,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn char_attack(
    plan: &mut Plan,
    world: &mut World,
    now_ms: i64,
    cfg: &CombatSection,
    skills_cfg: &SkillsSection,
    attack_interval_ms: i64,
    char_id: &str,
    target: &str,
    loc: &str,
) {
    let char_id = char_id.to_string();
    let target = target.to_string();
    let loc = loc.to_string();
    // M17/F30g: период атаки — cooldown_s оружия, иначе конфиг.
    let interval = world.char_attack_ms(&char_id).unwrap_or(attack_interval_ms);
    world.set_next_attack(&char_id, now_ms + interval);

    let target_alive_in_loc = world.mob_location(&target).is_some_and(|l| l == loc);
    if !target_alive_in_loc {
        world.end_combat(&char_id);
        push_event(
            plan,
            &char_id,
            "evt/combat/end",
            &CombatEnd {
                reason: "цель недоступна".into(),
            },
        );
        return;
    }
    let mob_name = world
        .mob(&target)
        .map(|m| m.name.clone())
        .unwrap_or_default();
    let source_name = world.char_name(&char_id).unwrap_or_default();
    // M32: бой — реальный урон листа (оружие/навык/сила), точность,
    // уклон/парирование/броня моба (раньше били константой конфига).
    let hits_as = world.char_hits_as(&char_id);
    let (damage, parried) = match char_melee_outcome(world, &char_id, &target, cfg) {
        MeleeOutcome::Miss => {
            let mk = |text: String| CombatHit {
                source: char_id.clone(),
                source_name: source_name.clone(),
                target: target.clone(),
                target_name: mob_name.clone(),
                kind: CombatKind::Melee,
                amount: 0,
                crit: false,
                miss: true,
                text,
                extra: Default::default(),
            };
            push_event(
                plan,
                &char_id,
                "evt/combat/hit",
                &mk(format!("Вы промахиваетесь по {mob_name}")),
            );
            for addr in world.chars_in_location(&loc) {
                if addr == char_id {
                    continue;
                }
                push_event(
                    plan,
                    &addr,
                    "evt/combat/hit",
                    &mk(format!("{source_name} промахивается по {mob_name}")),
                );
            }
            return;
        }
        MeleeOutcome::Hit { damage, parried } => (damage, parried),
    };
    let Some(hp) = world.damage_mob(&target, damage) else {
        world.end_combat(&char_id);
        push_event(
            plan,
            &char_id,
            "evt/combat/end",
            &CombatEnd {
                reason: "цель недоступна".into(),
            },
        );
        return;
    };
    // M17/F30e: нанесённый урон → ярость (применяется в tick_once).
    plan.char_hits.push((char_id.clone(), damage));
    let mob_max = world.mob_max_hp(&target);

    // M17/F30f: строка удара — «вы» атакующему, имя — наблюдателям;
    // «чем бьёт» — кэш экипированного оружия в мире.
    let mk_hit = |text: String| CombatHit {
        source: char_id.clone(),
        source_name: source_name.clone(),
        target: target.clone(),
        target_name: mob_name.clone(),
        kind: CombatKind::Melee,
        amount: damage,
        crit: false,
        miss: false,
        text,
        extra: Default::default(),
    };
    let parry_note = if parried {
        format!(" ({mob_name} парирует)")
    } else {
        String::new()
    };
    push_event(
        plan,
        &char_id,
        "evt/combat/hit",
        &mk_hit(format!(
            "{}{parry_note}",
            crate::combat_log::melee_hit_text(&source_name, &mob_name, hits_as, damage, true)
        )),
    );
    for addr in world.chars_in_location(&loc) {
        if addr == char_id {
            continue;
        }
        push_event(
            plan,
            &addr,
            "evt/combat/hit",
            &mk_hit(format!(
                "{}{parry_note}",
                crate::combat_log::melee_hit_text(&source_name, &mob_name, hits_as, damage, false)
            )),
        );
    }
    push_event(
        plan,
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

    let (mob_hp, killed_by_rune) = rune_procs(
        plan,
        world,
        &char_id,
        &target,
        &loc,
        now_ms,
        hp,
        mob_max,
        &source_name,
        &mob_name,
        skills_cfg,
    );
    if mob_hp == 0 && !killed_by_rune {
        // Общий путь смерти моба (атака и заклинания, M12.5b).
        if let Some(kill) =
            mob_kill_effects(world, &mut plan.events, &target, &loc, &char_id, skills_cfg)
        {
            plan.xp.push(kill);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rune_procs(
    plan: &mut Plan,
    world: &mut World,
    char_id: &str,
    target: &str,
    loc: &str,
    now_ms: i64,
    hp: i64,
    mob_max: i64,
    source_name: &str,
    mob_name: &str,
    skills_cfg: &SkillsSection,
) -> (i64, bool) {
    let char_id = char_id.to_string();
    let target = target.to_string();
    let loc = loc.to_string();
    let source_name = source_name.to_string();
    let mob_name = mob_name.to_string();
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
                world.apply_rune_effect(&target, effect, &char_id, now_ms);
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
                plan,
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
                    plan,
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
                plan,
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
                if let Some(kill) =
                    mob_kill_effects(world, &mut plan.events, &target, &loc, &char_id, skills_cfg)
                {
                    plan.xp.push(kill);
                }
                break;
            }
        }
    }

    (mob_hp, killed_by_rune)
}

fn plan_bleed_ticks(plan: &mut Plan, world: &mut World, now_ms: i64, skills_cfg: &SkillsSection) {
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
            push_event(plan, &addr, "evt/combat/hit", &dot_hit);
        }
        push_event(
            plan,
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
}

fn plan_burn_ticks(plan: &mut Plan, world: &mut World, now_ms: i64, skills_cfg: &SkillsSection) {
    for (mob_id, dps, src) in world.burning_mobs(now_ms) {
        let Some((loc, mob_name)) = world.mob_loc_name(&mob_id) else {
            continue;
        };
        world.advance_burn_tick(&mob_id, now_ms);
        let Some(hp) = world.damage_mob(&mob_id, dps) else {
            continue;
        };
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
            text: format!("{mob_name} горит: **{dps}**"),
            extra: Default::default(),
        };
        for addr in world.chars_in_location(&loc) {
            push_event(plan, &addr, "evt/combat/hit", &dot_hit);
        }
        push_event(
            plan,
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
            // Добил горение — XP источнику руны.
            if let Some(attacker) = src.as_deref() {
                if let Some(kill) =
                    mob_kill_effects(world, &mut plan.events, &mob_id, &loc, attacker, skills_cfg)
                {
                    plan.xp.push(kill);
                }
            }
        }
    }
}

fn plan_mob_attacks(
    plan: &mut Plan,
    world: &mut World,
    now_ms: i64,
    cfg: &CombatSection,
    attack_interval_ms: i64,
) {
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
        // M25: босс — вступление в бой и переход фазы по времени боя;
        // смена фазы → событие всем в локации (глиф в списке монстров).
        if let Some((_, true)) = world.boss_tick(&mob_id, now_ms) {
            if let Some(state) = world.boss_state(&mob_id) {
                for addr in world.chars_in_location(&loc) {
                    push_event(
                        plan,
                        &addr,
                        "evt/mob/phase",
                        &ld2_protocol::evt::MobPhase {
                            entity_id: mob_id.clone(),
                            phase: state.phase,
                            name: state.name.clone(),
                            glyph: state.glyph.clone(),
                        },
                    );
                }
            }
        }
        let (damage_mult, interval_mult) = world.boss_phase_mult(&mob_id, now_ms);
        // M25: интервал — (база конфига + замедление моба
        // `attack_speed`, мс) × множитель фазы/баффа, но не ниже
        // настройки сервера min_attack_interval_s («минимальная
        // скорость атаки») — крупные мобы бьют редко даже в поздних
        // фазах.
        let interval_ms = crate::systems::mob_attack_interval(
            attack_interval_ms + world.mob_attack_speed(&mob_id),
            interval_mult,
            (cfg.min_attack_interval_s * 1000.0).round() as i64,
        );
        world.set_mob_next_attack(&mob_id, now_ms + interval_ms);
        world.set_mob_attack_interval(&mob_id, interval_ms);
        if apply_boss_skill(plan, world, now_ms, &loc, &mob_id, damage_mult) {
            continue;
        }

        let Some(victim) = world.chars_in_location(&loc).into_iter().next() else {
            continue;
        };
        // M25: цель атаки — для строки «атакует вас/имя» в списке.
        world.set_mob_target(&mob_id, &victim);
        let mob_name = world
            .mob(&mob_id)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let victim_name = world.char_name(&victim).unwrap_or_default();
        // M32: точность моба против уклонения персонажа; парирование —
        // половина урона, броня персонажа — плоское снижение.
        let (amount, parried) = match mob_melee_outcome(world, &mob_id, &victim, damage_mult) {
            MeleeOutcome::Miss => {
                push_event(
                    plan,
                    &victim,
                    "evt/combat/hit",
                    &CombatHit {
                        source: mob_id.clone(),
                        source_name: mob_name.clone(),
                        target: victim.clone(),
                        target_name: victim_name,
                        kind: CombatKind::Melee,
                        amount: 0,
                        crit: false,
                        miss: true,
                        text: format!("{mob_name} промахивается по вам"),
                        extra: Default::default(),
                    },
                );
                // Бар атаки обновляем и при промахе — моб всё равно бил.
                for addr in world.chars_in_location(&loc) {
                    push_event(
                        plan,
                        &addr,
                        "evt/mob/attack",
                        &ld2_protocol::evt::MobAttack {
                            entity_id: mob_id.clone(),
                            next_attack_at: now_ms + interval_ms,
                            interval_ms,
                        },
                    );
                }
                continue;
            }
            MeleeOutcome::Hit { damage, parried } => (damage, parried),
        };
        if amount <= 0 {
            continue;
        }
        let parry_note = if parried {
            " (вы парируете)"
        } else {
            ""
        };
        push_event(
            plan,
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
                text: format!("{mob_name} бьёт вас: **{amount}**{parry_note}"),
                extra: Default::default(),
            },
        );
        // M25: бар атаки — клиент узнаёт срок следующей атаки.
        for addr in world.chars_in_location(&loc) {
            push_event(
                plan,
                &addr,
                "evt/mob/attack",
                &ld2_protocol::evt::MobAttack {
                    entity_id: mob_id.clone(),
                    next_attack_at: now_ms + interval_ms,
                    interval_ms,
                },
            );
        }
        plan.mob_hits.push(MobHit {
            char_id: victim,
            amount,
        });
    }
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
    // M29 (заказчик): гибель моба — строка «{имя} погиб» всем в локации
    // (имя — до kill_mob, тот удаляет моба из реестра).
    let mob_name = world
        .mob(target)
        .map(|m| m.name.clone())
        .unwrap_or_default();
    let died = crate::session::presence::died_line(&mob_name, "male");
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
                picked_by: None,
                picked_gender: None,
                text: Some(died.clone()),
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

/// Урон моба персонажу (БД, D27): урон → `evt/char/stats`; 0 hp → общий
/// M17/F30e: прирост ярости воина за нанесённый урон — БД + патч mp
/// адресату; не-воин/без изменений → `None`.
pub(super) async fn apply_rage_dealt(
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
pub(super) async fn apply_mob_hit(state: &SharedState, hit: &MobHit) -> Vec<(String, String)> {
    // M24: «режим бога» — урон не применяется вовсе: ни смерти, ни
    // потери лута, ни ярости за полученный урон.
    {
        let world = state.world.lock().expect("мир");
        if world.is_god(&hit.char_id) {
            return Vec::new();
        }
    }
    // M24: урон прерывает активную разделку — семантика M23:
    // skin → кожа испорчена (повтор невозможен, «Шкура испорчена»
    // в журнале), gut → просто отмена (повтор разрешён); шкура в
    // лут не попадает, опыта нет (finish_harvest не выполняется).
    let harvest_evt = {
        let mut world = state.world.lock().expect("мир");
        world.cancel_harvest(&hit.char_id).map(|job| {
            let ruined = job.action == "skin";
            if ruined {
                world.ruin_skin(&job.corpse_id);
            }
            event_text(
                "evt/harvest/result",
                &serde_json::to_value(ld2_protocol::evt::HarvestResult {
                    entity_id: job.corpse_id,
                    name: job.name,
                    action: job.action,
                    ok: false,
                    ruined,
                })
                .unwrap_or_default(),
            )
        })
    };
    let pool = &state.pool;
    let row = match crate::db::characters::get(pool, &hit.char_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Vec::new(),
        Err(e) => {
            warn!("тик (персонаж {}): {e}", hit.char_id);
            return Vec::new();
        }
    };
    // R14: атомарный относительный урон — GREATEST(0, hp_cur - урон)
    // с RETURNING; абсолютная запись (как и зелье) теряла урон/лечение
    // при параллельном применении.
    let (hp_cur, hp_max) =
        match crate::db::characters::damage_hp(pool, &hit.char_id, hit.amount).await {
            Ok(Some(pair)) => pair,
            Ok(None) => return Vec::new(), // персонаж исчез (смерть параллельно)
            Err(e) => {
                warn!("тик (hp {}): {e}", hit.char_id);
                return Vec::new();
            }
        };
    if hp_cur == 0 {
        // Смерть: позиция после респауна — источник истины для мира.
        let mut events = crate::death::kill_character(state, &hit.char_id).await;
        if let Ok(Some(row)) = crate::db::characters::get(pool, &hit.char_id).await {
            let mut world = state.world.lock().expect("мир");
            world.set_char_location(&hit.char_id, &row.location_id);
            world.end_combat(&hit.char_id);
        }
        // M24: прерванная уроном разделка — итог клиенту и при смерти.
        if let Some(text) = harvest_evt {
            events.push((hit.char_id.clone(), text));
        }
        return events;
    }
    let mut patch = serde_json::Map::new();
    patch.insert("hp".into(), json!({ "cur": hp_cur, "max": hp_max }));
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
    let mut out = vec![(hit.char_id.clone(), text)];
    // M24: итог прерванной разделки — следом за патчем характеристик.
    if let Some(text) = harvest_evt {
        out.push((hit.char_id.clone(), text));
    }
    out
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

    let (xp, rank, max_attr, total_xp, char_name, gender, attrs, mp_max) = match (
        crate::db::skills::learned_one(pool, &work.char_id, &skill_id).await,
        crate::db::characters::get(pool, &work.char_id).await,
    ) {
        (Ok(learned), Ok(Some(row))) => {
            let attrs: std::collections::BTreeMap<String, i64> =
                serde_json::from_value(row.attrs).unwrap_or_default();
            let max_attr = attrs.values().max().copied().unwrap_or(0);
            let (xp, rank) = learned.unwrap_or((0, 0));
            (
                xp,
                rank,
                max_attr,
                row.total_xp,
                row.name,
                row.gender,
                attrs,
                row.mp_max,
            )
        }
        (Err(e), _) | (_, Err(e)) => {
            warn!("тик (XP {}): {e}", work.char_id);
            return None;
        }
        _ => return None,
    };

    // M31: уровень после начисления — им же ограничен ранг навыка
    // (не более 2 за уровень), он же нужен для анонса level-up.
    let level_cfg = &state.config.gameplay.level;
    let (old_level, ..) = crate::systems::level_for_total(total_xp, level_cfg);
    let (level_after, ..) =
        crate::systems::level_for_total(total_xp + work.amount.max(0), level_cfg);

    // M15/F24: общий опыт растёт при каждом убийстве — даже когда
    // навык на гейте и его XP не изменился.
    if work.amount > 0 {
        if let Err(e) = crate::db::characters::add_total_xp(pool, &work.char_id, work.amount).await
        {
            warn!("тик (total_xp {}): {e}", work.char_id);
        }
        // M22: новый уровень персонажа — анонс в локации (своему
        // персонажу клиент пишет «Вы получили…» по char_id).
        if level_after > old_level {
            // M30: максимум HP растёт с уровнем (character-params.md
            // §2.1) — пересчёт кэша колонки + границ текущего значения.
            let hp_max = crate::stats::max_hp(&attrs, level_after, &state.config);
            if let Err(e) =
                crate::db::characters::set_max(pool, &work.char_id, hp_max, i64::from(mp_max)).await
            {
                warn!("set_max (level-up {}): {e}", work.char_id);
            }
            let payload = ld2_protocol::evt::CharLevelUp {
                char_id: work.char_id.clone(),
                name: char_name.clone(),
                gender: gender.clone(),
                level: level_after,
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

    // M31: потолок ранга боевых/защитных/магических навыков — не более
    // 2 за уровень персонажа; группа берётся из контента (skills.md §3).
    let group = match crate::db::skills::content_skill(pool, &skill_id).await {
        Ok(Some(row)) => row.skill_group,
        Ok(None) => String::new(),
        Err(e) => {
            warn!("тик (группа навыка {skill_id}): {e}");
            String::new()
        }
    };
    let rank_cap = crate::systems::skill_rank_cap(&state.config.skills, &group, level_after);
    let (new_xp, new_rank) = crate::systems::apply_kill_xp(
        xp,
        rank,
        work.amount,
        &state.config.skills,
        max_attr,
        rank_cap,
    );
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

#[allow(clippy::too_many_arguments)]
fn apply_boss_skill(
    plan: &mut Plan,
    world: &mut World,
    now_ms: i64,
    loc: &str,
    mob_id: &str,
    damage_mult: f64,
) -> bool {
    let mob_id = mob_id.to_string();
    let loc = loc.to_string();
    // M25: навыки босса — каст (подготовка: босс не атакует) либо
    // применение мгновенного/готового каста.
    let (skill_tick, casting) = world.boss_skill_tick(&mob_id, now_ms);
    let boss_name = world
        .mob(&mob_id)
        .map(|m| m.name.clone())
        .unwrap_or_default();
    match &skill_tick {
        crate::world::BossSkillTick::Started(skill) => {
            for addr in world.chars_in_location(&loc) {
                push_event(
                    plan,
                    &addr,
                    "evt/mob/cast",
                    &ld2_protocol::evt::MobCast {
                        entity_id: mob_id.clone(),
                        source_name: boss_name.clone(),
                        name: skill.name.clone(),
                        glyph: skill.glyph.clone(),
                        ends_at: now_ms + skill.cast_s.max(1) * 1000,
                    },
                );
            }
        }
        crate::world::BossSkillTick::Applied(skill) => {
            let skill_damage = ((skill.damage as f64) * damage_mult).round() as i64;
            for addr in world.chars_in_location(&loc) {
                push_event(
                    plan,
                    &addr,
                    "evt/mob/skill",
                    &ld2_protocol::evt::MobSkill {
                        entity_id: mob_id.clone(),
                        source_name: boss_name.clone(),
                        name: skill.name.clone(),
                        glyph: skill.glyph.clone(),
                    },
                );
                if skill_damage > 0 {
                    plan.mob_hits.push(MobHit {
                        char_id: addr,
                        amount: skill_damage,
                    });
                }
            }
            // M25: эффект приёма — контроль над игроками локации
            // (стан/немота/ослепление) + событие на отображение.
            if let Some(effect) = &skill.effect {
                for addr in world.chars_in_location(&loc) {
                    let until = now_ms + effect.duration_s.max(1) * 1000;
                    if world.set_char_effect(&addr, &effect.kind, until) {
                        push_event(
                            plan,
                            &addr,
                            "evt/char/afflict",
                            &ld2_protocol::evt::CharAfflict {
                                kind: effect.kind.clone(),
                                title: crate::world::effect_title(&effect.kind),
                                until_ms: until,
                            },
                        );
                    }
                }
            }
        }
        crate::world::BossSkillTick::None => {}
    }
    // Подготовка/применение навыка — без обычной атаки в такт.
    casting
        || matches!(
            skill_tick,
            crate::world::BossSkillTick::Started(_) | crate::world::BossSkillTick::Applied(_)
        )
}
