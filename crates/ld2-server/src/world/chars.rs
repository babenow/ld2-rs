//! Боевое состояние персонажа и руны (M17/M18/F58d).

use super::*;
use crate::consts::{EFFECT_BLEED, EFFECT_BURN, EFFECT_ROOT, EFFECT_STUN, MS_PER_SECOND};

const MIN_BURN_PERIOD_MS: i64 = 500;

/// Бой персонажа (сессионный: при дисконнекте/смене локации прерывается).
#[derive(Debug, Clone)]
pub struct CombatState {
    pub target_id: String,
    pub location_id: String,
    /// Следующая атака (epoch, мс).
    pub next_attack_at: i64,
}

/// M18/F58d (ADR-012 §5): руна в **экипированном** оружии — параметры
/// авто-приёма для `plan_combat` (синхронный доступ без БД; сборка —
/// `session::refresh_char_runes` на входе/экипировке/инкрустации/касте).
#[derive(Debug, Clone)]
pub struct SocketedRune {
    /// uid предмета-носителя (запись charges в БД из плана).
    pub staff_uid: String,
    pub socket_index: u32,
    pub def_id: String,
    pub spell: String,
    /// Шанс срабатывания при ударе, 0..1 (контент `stats.chance`).
    pub chance: f64,
    /// Урон при срабатывании (контент `stats.damage`).
    pub damage: i64,
    /// Затрата зарядов (деф 1, `stats.cost`).
    pub cost: i64,
    /// Остаток зарядов (кэш синхронизируется с БД при изменениях).
    pub charges: i64,
    /// Бафф персонажа при срабатывании (контент `stats.buff`).
    pub buff: Option<RuneBuff>,
    /// Эффект на цель-моба при срабатывании (контент `stats.effect`):
    /// `stun`/`root`/`bleed`/`burn` (ADR-012 §5, M18/F58d + burn).
    pub effect: Option<RuneEffect>,
}

/// M18/F58d: эффект руны на моба (контент `stats.effect`).
#[derive(Debug, Clone)]
pub struct RuneEffect {
    /// `stun` | `root` | `bleed` | `burn`.
    pub kind: String,
    /// Для `bleed` — урон/сек; для `burn` — урон за тик; для stun/root
    /// не используется.
    pub value: i64,
    pub duration_s: i64,
    /// Для `burn` — период тика, с (дефолт 1); остальным не используется.
    pub period_s: i64,
}

/// M18/F58d: параметры баффа руны (контент `stats.buff`).
#[derive(Debug, Clone)]
pub struct RuneBuff {
    /// Ключ баффа — def_id руны (стаки разных рун не смешиваются).
    pub kind: String,
    /// Отображаемое название (сервер — истина, §8).
    pub title: String,
    /// Параметр (`accuracy`/`crit_chance`/`armor`/…).
    pub param: String,
    pub value: i64,
    pub pct: bool,
    pub duration_s: i64,
    pub max_stacks: i32,
}

/// M37: кровотечение персонажа от приёма моба («Укус» белого волка):
/// урон в секунду, срок и следующая секунда.
#[derive(Debug, Clone, Copy)]
pub struct CharBleed {
    pub dps: i64,
    pub until_ms: i64,
    pub next_at: i64,
}

/// M32: снимок боевых параметров персонажа для синхронного тика боя
/// (без похода в БД на каждый удар). Источник — `stats::derive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharCombat {
    /// Урон удара: `damage_min..=damage_max` (уже с оружием/навыком).
    pub damage_min: i64,
    pub damage_max: i64,
    /// Точность (1..99) — шанс попасть по мобу.
    pub accuracy: i64,
    /// Уклонение (0..60) — шанс моба промахнуться.
    pub dodge: i64,
    /// Парирование (0..60) — шанс вдвое снизить урон.
    pub parry: i64,
    /// Броня — плоское снижение входящего урона.
    pub armor: i64,
}

impl World {
    /// M32: обновить кэш боевых параметров персонажа.
    pub fn set_char_combat(&mut self, char_id: &str, combat: CharCombat) {
        self.char_combat.insert(char_id.to_string(), combat);
    }

    /// M32: боевые параметры персонажа (до входа в мир — нет).
    pub fn char_combat(&self, char_id: &str) -> Option<CharCombat> {
        self.char_combat.get(char_id).copied()
    }

    /// M24: включить/выключить неуязвимость персонажа («режим бога»).
    pub fn set_god(&mut self, char_id: &str, enabled: bool) {
        if enabled {
            self.god_chars.insert(char_id.to_string());
        } else {
            self.god_chars.remove(char_id);
        }
    }
    /// M24: есть ли у персонажа неуязвимость (тик, боевые расчёты).
    pub fn is_god(&self, char_id: &str) -> bool {
        self.god_chars.contains(char_id)
    }
    /// M25: наложить эффект контроля (не короче уже действующего);
    /// true — применён.
    pub fn set_char_effect(&mut self, char_id: &str, kind: &str, until_ms: i64) -> bool {
        let entry = self.char_effects.entry(char_id.to_string()).or_default();
        let fresh = entry.get(kind).copied().unwrap_or(0) < until_ms;
        if fresh {
            entry.insert(kind.to_string(), until_ms);
        }
        fresh
    }
    /// M25: срок активного эффекта (просроченный не активен).
    pub fn char_effect_until(&self, char_id: &str, kind: &str, now_ms: i64) -> Option<i64> {
        let until = self.char_effects.get(char_id)?.get(kind).copied()?;
        (until > now_ms).then_some(until)
    }
    /// M25: активен ли эффект контроля сейчас.
    pub fn char_effect_active(&self, char_id: &str, kind: &str, now_ms: i64) -> bool {
        self.char_effect_until(char_id, kind, now_ms).is_some()
    }
    /// M37: наложить кровотечение (сильнее/дольше — обновляет).
    pub fn set_char_bleed(&mut self, char_id: &str, dps: i64, until_ms: i64, now_ms: i64) {
        let dps = dps.max(1);
        let entry = self
            .char_bleed
            .entry(char_id.to_string())
            .or_insert(CharBleed {
                dps,
                until_ms,
                next_at: now_ms + MS_PER_SECOND,
            });
        if entry.until_ms <= now_ms {
            *entry = CharBleed {
                dps,
                until_ms,
                next_at: now_ms + MS_PER_SECOND,
            };
        } else {
            entry.dps = entry.dps.max(dps);
            entry.until_ms = entry.until_ms.max(until_ms);
        }
    }
    /// M37: персонажи, у которых пора тикнуть кровотечение.
    pub fn bleeding_chars(&self, now_ms: i64) -> Vec<(String, i64)> {
        self.char_bleed
            .iter()
            .filter(|(_, b)| b.until_ms > now_ms && b.next_at <= now_ms)
            .map(|(id, b)| (id.clone(), b.dps))
            .collect()
    }
    /// M37: сдвинуть тик кровотечения (истёкшее — снять).
    pub fn advance_char_bleed(&mut self, char_id: &str, now_ms: i64) {
        let expired = self
            .char_bleed
            .get(char_id)
            .is_none_or(|b| b.until_ms <= now_ms);
        if expired {
            self.char_bleed.remove(char_id);
        } else if let Some(b) = self.char_bleed.get_mut(char_id) {
            b.next_at = now_ms + MS_PER_SECOND;
        }
    }
    /// M37: снять кровотечение (смерть/выход).
    pub fn clear_char_bleed(&mut self, char_id: &str) {
        self.char_bleed.remove(char_id);
    }
    /// M17/F30f: чем бьёт персонаж — творительный из экипированного
    /// оружия; без данных (до входа) — кулаки.
    pub fn char_hits_as(&self, char_id: &str) -> &'static str {
        self.char_hits.get(char_id).copied().unwrap_or("руками")
    }
    /// M17/F30f: обновить «чем бьёт» (вызывается на входе в мир и при
    /// экипировке/снятии оружия).
    pub fn set_char_hits_as(&mut self, char_id: &str, hits_as: &'static str) {
        self.char_hits.insert(char_id.to_string(), hits_as);
    }
    /// M17/F30g: период атаки из `cooldown_s` оружия; `None` — снять
    /// значение (нет оружия → конфиг).
    pub fn set_char_attack_ms(&mut self, char_id: &str, ms: Option<i64>) {
        match ms {
            Some(ms) => {
                self.char_attack.insert(char_id.to_string(), ms);
            }
            None => {
                self.char_attack.remove(char_id);
            }
        }
    }
    /// M17/F30g: текущий период атаки, мс (`None` — из конфига).
    pub fn char_attack_ms(&self, char_id: &str) -> Option<i64> {
        self.char_attack.get(char_id).copied()
    }
    /// M18/F58d: руны экипированного оружия (пусто — не экипировано
    /// или руны без `chance` не участвуют в авто-приёме).
    pub fn char_runes(&self, char_id: &str) -> &[SocketedRune] {
        self.char_runes
            .get(char_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    /// M18/F58d: заменить кэш рун персонажа (сборка — `refresh_char_runes`).
    pub fn set_char_runes(&mut self, char_id: &str, runes: Vec<SocketedRune>) {
        if runes.is_empty() {
            self.char_runes.remove(char_id);
        } else {
            self.char_runes.insert(char_id.to_string(), runes);
        }
    }
    /// M18/F58d: расход зарядов в кэше (план авто-приёма; БД — в
    /// `tick_once` по `Plan::rune_work`).
    pub fn spend_char_rune(&mut self, char_id: &str, socket_index: u32, charges: i64) {
        if let Some(runes) = self.char_runes.get_mut(char_id) {
            if let Some(rune) = runes.iter_mut().find(|r| r.socket_index == socket_index) {
                rune.charges = charges;
            }
        }
    }
    /// M18/F58d (ADR-012 §5): наложить эффект руны на моба —
    /// `stun`/`root`/`bleed` (контент `stats.effect`). `root` — поле
    /// под будущее перемещение мобов; `bleed` тикает в `plan_combat`.
    pub fn apply_rune_effect(
        &mut self,
        mob_id: &str,
        effect: &RuneEffect,
        src_char_id: &str,
        now_ms: i64,
    ) {
        let kind = effect.kind.as_str();
        let value = effect.value;
        let duration_s = effect.duration_s;
        let period_s = effect.period_s;
        let Some(mob) = self.mobs.get_mut(mob_id) else {
            return;
        };
        let until = now_ms + duration_s.max(0) * MS_PER_SECOND;
        match kind {
            EFFECT_STUN => mob.stun_until = until,
            EFFECT_ROOT => mob.root_until = until,
            EFFECT_BLEED => {
                // Продление активного кровотечения НЕ сбрасывает таймер
                // тика (иначе частые удары не дают DoT дотикнуть).
                if mob.bleed_dps == 0 || mob.bleed_until <= now_ms {
                    mob.bleed_dps = value.max(1);
                    mob.bleed_next_at = now_ms + MS_PER_SECOND; // первый тик через секунду
                    mob.bleed_src = Some(src_char_id.to_string());
                }
                mob.bleed_until = until;
            }
            EFFECT_BURN => {
                // «Горение» — как кровотечение, но с периодом из контента
                // (продление не сбрасывает таймер тика).
                if mob.burn_dps == 0 || mob.burn_until <= now_ms {
                    mob.burn_dps = value.max(1);
                    mob.burn_period = (period_s.max(1) * MS_PER_SECOND).max(MIN_BURN_PERIOD_MS);
                    mob.burn_next_at = now_ms + mob.burn_period;
                    mob.burn_src = Some(src_char_id.to_string());
                }
                mob.burn_until = until;
            }
            _ => {}
        }
    }
    /// Регистрация соединения персонажа (рассылка событий тика §2.4).
    /// R10: персонаж, уже привязанный к другой сессии (peer), — отказ:
    /// две WS-сессии не должны действовать одним персонажем (отключение
    /// старой раньше отвязывало мир у новой). Повторный bind той же
    /// сессии — ок (перевыбор персонажа).
    pub fn bind_char(&mut self, char_id: &str, name: &str, tx: OutSender, owner: &str) -> bool {
        if self.char_owner.get(char_id).is_some_and(|cur| cur != owner) {
            return false;
        }
        self.conns.insert(char_id.to_string(), tx);
        self.char_names
            .insert(char_id.to_string(), name.to_string());
        self.char_owner
            .insert(char_id.to_string(), owner.to_string());
        true
    }
    /// M34: персонажи, привязанные к сессии-владельцу (вытеснение
    /// аккаунта вторым входом).
    pub fn char_ids_owned_by(&self, owner: &str) -> Vec<String> {
        self.char_owner
            .iter()
            .filter(|(_, o)| o.as_str() == owner)
            .map(|(id, _)| id.clone())
            .collect()
    }
    /// R10: отвязка — только своего биндинга (умершая старая сессия
    /// позже новой не отвязывает мир у новой). `true` — отвязано
    /// (тогда соединение чистит и социум).
    pub fn unbind_char(&mut self, char_id: &str, owner: &str) -> bool {
        if self.char_owner.get(char_id).map(String::as_str) != Some(owner) {
            return false;
        }
        self.conns.remove(char_id);
        self.char_names.remove(char_id);
        self.char_loc.remove(char_id);
        self.combat.remove(char_id);
        self.ability_cd.retain(|(c, _), _| c != char_id);
        self.char_bleed.remove(char_id);
        self.char_effects.remove(char_id);
        self.gather_jobs.remove(char_id);
        self.char_owner.remove(char_id);
        true
    }
    /// Позиция персонажа в мире (мишени мобов, бой по локации).
    pub fn set_char_location(&mut self, char_id: &str, location_id: &str) {
        self.char_loc
            .insert(char_id.to_string(), location_id.to_string());
    }
    /// Обратный поиск: имя персонажа → char_id (whisper, ФР-13).
    pub fn char_id_by_name(&self, name: &str) -> Option<String> {
        self.char_names
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(id, _)| id.clone())
    }
    /// Локация персонажа (проверка сделки «в одной локации», ФР-15).
    pub fn char_location(&self, char_id: &str) -> Option<String> {
        self.char_loc.get(char_id).cloned()
    }
    pub fn char_name(&self, char_id: &str) -> Option<String> {
        self.char_names.get(char_id).cloned()
    }
    /// Локации, где есть персонажи (шаг агрессии мобов).
    pub fn locations_with_players(&self) -> Vec<String> {
        let mut locs: Vec<String> = self.char_loc.values().cloned().collect();
        locs.sort();
        locs.dedup();
        locs
    }
    /// Персонажи, привязанные к миру (онлайн — реген маны, M12.5b).
    pub fn online_chars(&self) -> Vec<String> {
        let mut chars: Vec<String> = self.char_names.keys().cloned().collect();
        chars.sort();
        chars
    }
    pub fn chars_in_location(&self, location_id: &str) -> Vec<String> {
        let mut chars: Vec<String> = self
            .char_loc
            .iter()
            .filter(|(_, loc)| loc.as_str() == location_id)
            .map(|(id, _)| id.clone())
            .collect();
        chars.sort();
        chars
    }
    /// Кулдаун активной способности: `Some(остаток_с)` (M14, §4).
    pub fn ability_cooldown_left(&self, char_id: &str, skill: &str, now_s: i64) -> Option<i64> {
        let until = self
            .ability_cd
            .get(&(char_id.to_string(), skill.to_string()))?;
        Some(*until - now_s).filter(|left| *left > 0)
    }
    /// Записать кулдаун активной способности (until = now + cooldown_s).
    pub fn set_ability_cooldown(&mut self, char_id: &str, skill: &str, until_s: i64) {
        self.ability_cd
            .insert((char_id.to_string(), skill.to_string()), until_s);
    }
    /// Привязан ли персонаж к миру (есть канал соединения) — онлайн.
    pub fn is_bound(&self, char_id: &str) -> bool {
        self.conns.contains_key(char_id)
    }
    /// Отправить конверт соединению персонажа (best-effort).
    pub fn send_to_char(&self, char_id: &str, text: String) {
        if let Some(tx) = self.conns.get(char_id) {
            // R15: bounded-канал — полный буфер = медленный клиент;
            // рассылка снимается (память ограничена), команды через
            // sink работают; resync восстановит состояние.
            let _ = tx.try_send(text);
        }
    }
}
