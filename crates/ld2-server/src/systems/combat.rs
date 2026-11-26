//! Бой и ресурсы: ярость, шаг ресурса, интервал атаки мобов, преступность (M17/M25).

/// Вклад нового преступления в общий срок (§10).
///
/// `T = 0` (первое преступление) → полный срок `t` — так согласован пример
/// §10 (murderer=21600 первым); иначе частичное сложение
/// `k·t·(1 − T/T_MAX)`. Возвращает итоговый общий срок
/// `T_new = min(T_MAX, T + вклад)` и **дельту** — сколько секунд
/// добавляется новому статусу (он живёт `delta` секунд).
pub fn crime_blend(t_cur: i64, t_new: i64, t_max: i64, k: f64) -> (i64, i64) {
    let t_cur = t_cur.max(0);
    let t_max = t_max.max(1).max(t_cur);
    let delta = if t_cur == 0 {
        // Первое преступление — без «скидки».
        t_new.max(0)
    } else {
        let ratio = (t_cur as f64 / t_max as f64).clamp(0.0, 1.0);
        (k * t_new.max(0) as f64 * (1.0 - ratio)).round().max(0.0) as i64
    };
    let total = (t_cur + delta).min(t_max);
    (total, (total - t_cur).max(0))
}

// ─── M32: боевые броски (точность, уклон, парирование, броня, магрезист) ────

/// Шанс попадания, %: `точность − уклонение`, кламп 5..95 — полный
/// промах или полная неуязвимость невозможны.
pub fn hit_chance(accuracy: i64, dodge: i64) -> i64 {
    (accuracy - dodge).clamp(5, 95)
}

/// Попал ли удар: бросок `0..100` меньше шанса попадания.
pub fn hit_rolls(accuracy: i64, dodge: i64, roll: f64) -> bool {
    roll < hit_chance(accuracy, dodge) as f64
}

/// Урон в диапазоне `[min, max]` по броску `[0,1)` (min == max —
/// детерминированно, чем пользуются тесты без оружия).
pub fn roll_damage(min: i64, max: i64, roll01: f64) -> i64 {
    let min = min.max(0);
    let max = max.max(min);
    if max == min {
        return min;
    }
    let span = (max - min + 1) as f64;
    let offset = (roll01.clamp(0.0, 0.999_999) * span).floor() as i64;
    (min + offset).clamp(min, max)
}

/// Урон способности по броскам (M32): `None` — промах. `mult` —
/// множитель способности (`damage_mult`), `hit_roll`/`parry_roll` —
/// броски 0..100, `dmg_roll` — 0..1.
#[allow(clippy::too_many_arguments)]
pub fn ability_damage(
    accuracy: i64,
    dodge: i64,
    damage_min: i64,
    damage_max: i64,
    armor: i64,
    parry: i64,
    mult: f64,
    hit_roll: f64,
    dmg_roll: f64,
    parry_roll: f64,
) -> Option<i64> {
    if !hit_rolls(accuracy, dodge, hit_roll) {
        return None;
    }
    let roll = roll_damage(damage_min, damage_max, dmg_roll);
    let mut damage = ((roll as f64) * mult).round().max(1.0) as i64;
    if parry_roll < parry.clamp(0, 100) as f64 {
        damage = after_parry(damage);
    }
    Some(after_armor(damage, armor))
}

/// Парирование — половина урона с округлением вверх, минимум 1.
pub fn after_parry(damage: i64) -> i64 {
    (damage.max(1) + 1) / 2
}

/// Броня — плоское снижение урона, минимум 1 (удар всегда ранит).
pub fn after_armor(damage: i64, armor: i64) -> i64 {
    (damage.max(1) - armor.max(0)).max(1)
}

/// Магическое сопротивление, % — снижение урона, минимум 1.
pub fn after_resist(damage: i64, resist: i64) -> i64 {
    let r = resist.clamp(0, 90) as f64;
    (((damage.max(1) as f64) * (1.0 - r / 100.0)).round() as i64).max(1)
}

// ─── M17/F30e: ресурсы по классам (WoW-модель) ─────────────────────────────

/// Максимум ресурса по профилю: воин — ярость, лучник — энергия
/// (из `[gameplay.resource]`), остальные (мана) — колонка персонажа.
pub fn resource_max(profile: &str, res: &crate::config::ResourceSection, mp_max: i64) -> i64 {
    match profile {
        "warrior" => i64::from(res.rage_max),
        "archer" => i64::from(res.energy_max),
        _ => mp_max,
    }
}

/// Название ресурса в серверных сообщениях (родительный падеж):
/// «не хватает ярости/энергии/маны».
pub fn resource_name(profile: &str) -> &'static str {
    match profile {
        "warrior" => "ярости",
        "archer" => "энергии",
        _ => "маны",
    }
}

/// M25: интервал атаки моба — конфиг × множитель фазы босса, но не
/// ниже настройки сервера `min_attack_interval_s` («минимальная
/// скорость атаки»); сверху прибавляется `attack_speed` моба.
pub fn mob_attack_interval(base_ms: i64, phase_mult: f64, min_ms: i64) -> i64 {
    ((base_ms as f64 * phase_mult).round() as i64).max(min_ms)
}

/// Прирост ярости за урон (воин): доля `rage_gain_pct`, округление
/// вверх; не-воин или нулевой урон → 0.
pub fn rage_gain(res: &crate::config::ResourceSection, profile: &str, damage: i64) -> i64 {
    if profile != "warrior" || damage <= 0 {
        return 0;
    }
    (damage as f64 * res.rage_gain_pct).ceil() as i64
}

/// M17/F30e: шаг ресурса онлайн-персонажа раз в секунду по профилю:
/// **воин** — ярость падает вне боя (в бою не трогаем), **лучник** —
/// энергия восстанавливается всегда (в бою и вне), **мана** — прежняя
/// модель §2.4 (вне боя полный бар за `regen_out_s`, в бою %/с),
/// умноженная на духовность M30: `delta × (1 + spir×spirit_regen_pct/100)`
/// (character-params.md §3.2; ярость/энергия от духовности не зависят).
/// `None` — изменений нет (в том числе при полном ресурсе).
pub fn resource_step(
    profile: &str,
    cur: i64,
    max: i64,
    in_combat: bool,
    spir: i64,
    cfg: &crate::config::Config,
) -> Option<i64> {
    let cur = cur.min(max);
    let new = match profile {
        "warrior" => {
            if in_combat {
                return None;
            }
            (cur as f64 - cfg.gameplay.resource.rage_decay_s)
                .max(0.0)
                .round() as i64
        }
        "archer" => {
            if cur >= max {
                return None;
            }
            let step = cfg.gameplay.resource.energy_regen_s.round() as i64;
            (cur + step).min(max)
        }
        _ => {
            if cur >= max {
                return None;
            }
            let mana = &cfg.gameplay.magic.mana;
            let base = if in_combat {
                (max as f64 * mana.regen_in_pct_s / 100.0).ceil().max(1.0) as i64
            } else {
                (max as f64 / mana.regen_out_s.max(1) as f64)
                    .ceil()
                    .max(1.0) as i64
            };
            // M30: духовность ускоряет реген маны (в бое и вне).
            let spirit_mult = 1.0 + spir.max(0) as f64 * mana.spirit_regen_pct / 100.0;
            let delta = ((base as f64) * spirit_mult).ceil() as i64;
            (cur + delta).min(max)
        }
    };
    (new != cur).then_some(new)
}

/// M30: шаг регенерации HP раз в секунду (character-params.md §3.1):
/// вне боя полный бар за `health.regen_out_s`, в бою `regen_in_pct_s` %
/// максимума в секунду; шаг ≥ 1. Лечат только `0 < cur < max` —
/// **мёртвые не регенерируют** (кламп к максимуму делает `heal_hp`).
/// `None` — изменений нет.
pub fn health_step(
    cur: i64,
    max: i64,
    in_combat: bool,
    cfg: &crate::config::Config,
) -> Option<i64> {
    if cur <= 0 || cur >= max {
        return None;
    }
    let health = &cfg.gameplay.health;
    let delta = if in_combat {
        (max as f64 * health.regen_in_pct_s / 100.0).ceil().max(1.0) as i64
    } else {
        (max as f64 / health.regen_out_s.max(1) as f64)
            .ceil()
            .max(1.0) as i64
    };
    let new = (cur + delta).min(max);
    (new != cur).then_some(new)
}
