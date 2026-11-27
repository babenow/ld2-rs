//! RF1a: гейты сессии — авторизация, роли, режимы персонажа, лимиты.
//!
//! Всё, что проверяется **до** выполнения команды: `require_auth`,
//! `require_char_id`, `require_admin`, модерация чата, блокировки
//! контроля (`E_BUSY`), rate-limit и кулдауны предметов. Методы
//! `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::envelope::{Ack, Envelope};
use ld2_protocol::evt::CombatCooldown;
use ld2_protocol::ErrorCode;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{out_ack, out_event, Out, Session};
use crate::db;

/// M24: команды, блокируемые активной разделкой (RF5a: один источник правды).
pub(super) const HARVEST_BLOCKED: &[&str] = &[
    names::CMD_LOCATION_MOVE,
    names::CMD_COMBAT_ATTACK,
    names::CMD_COMBAT_STOP,
    names::CMD_COMBAT_FLEE,
    names::CMD_SKILL_USE,
    names::CMD_MAGIC_RUNE_CAST,
    names::CMD_MAGIC_SCROLL_CAST,
    names::CMD_RESOURCE_GATHER,
    names::CMD_CRAFT_START,
    names::CMD_QUEST_ACCEPT,
    names::CMD_QUEST_TURN_IN,
    names::CMD_QUEST_ABANDON,
    names::CMD_TRADE_OPEN,
    names::CMD_TRADE_ADD_ITEM,
    names::CMD_TRADE_SET_GOLD,
    names::CMD_TRADE_PROPOSE,
    names::CMD_TRADE_ACCEPT,
    names::CMD_TRADE_CANCEL,
    names::CMD_PARTY_CREATE,
    names::CMD_PARTY_INVITE,
    names::CMD_PARTY_ACCEPT,
    names::CMD_PARTY_DECLINE,
    names::CMD_PARTY_LEAVE,
    names::CMD_PARTY_KICK,
    names::CMD_GUILD_CREATE,
    names::CMD_GUILD_JOIN,
    names::CMD_GUILD_LEAVE,
    names::CMD_GUILD_INVITE,
    names::CMD_GUILD_KICK,
    names::CMD_GUILD_RANK_SET,
    names::CMD_SHOP_BUY,
    names::CMD_UPGRADE_PREVIEW,
    names::CMD_UPGRADE_START,
    names::CMD_SOCKET_INSERT,
    names::CMD_SOCKET_EXTRACT,
];

/// M25: команды предметов и трупа — в стан блокируются сверх разделки.
pub(super) const ITEM_ACTIONS: &[&str] = &[
    names::CMD_ITEM_USE,
    names::CMD_ITEM_DROP,
    names::CMD_ITEM_EQUIP,
    names::CMD_ITEM_UNEQUIP,
    names::CMD_ITEM_MOVE,
    names::CMD_CORPSE_HARVEST,
];

/// M25: стан блокирует `HARVEST_BLOCKED ∪ ITEM_ACTIONS` (RF5a: без дублей списка).
pub(super) fn stun_blocked(msg_type: &str) -> bool {
    HARVEST_BLOCKED.contains(&msg_type) || ITEM_ACTIONS.contains(&msg_type)
}

impl Session {
    /// Текущий персонаж для команд мира (§3: без него «не в мире»).
    pub(super) fn require_char_id(&self, seq: u32) -> Result<String, Out> {
        self.selected_char_id
            .clone()
            .ok_or_else(|| Self::err(seq, ErrorCode::NotInLocation, "персонаж не выбран"))
    }

    /// M24: активная разделка персонажа — блок действий (`E_BUSY`).
    /// M25: активные дебаффы контроля → (код, сообщение) по типу
    /// команды; блокируемые списки — внутри (const в fn).
    pub(super) fn char_effect_gate(&self, msg_type: &str) -> Option<(ErrorCode, String)> {
        const SILENCE_BLOCKED: &[&str] =
            &[names::CMD_MAGIC_RUNE_CAST, names::CMD_MAGIC_SCROLL_CAST];
        const BLIND_BLOCKED: &[&str] = &[names::CMD_COMBAT_ATTACK, names::CMD_SKILL_USE];
        let now = crate::time::now_ms();
        let char_id = self.selected_char_id.as_deref()?;
        let (stunned, silenced, blinded) = {
            let world = self.state.world.lock().expect("мир");
            (
                world.char_effect_active(char_id, "stun", now),
                world.char_effect_active(char_id, "silence", now),
                world.char_effect_active(char_id, "blind", now),
            )
        };
        if stunned && stun_blocked(msg_type) {
            return Some((ErrorCode::Stunned, "оглушён".to_string()));
        }
        if silenced && SILENCE_BLOCKED.contains(&msg_type) {
            return Some((ErrorCode::Stunned, "немота — нельзя колдовать".to_string()));
        }
        if blinded && BLIND_BLOCKED.contains(&msg_type) {
            return Some((
                ErrorCode::Stunned,
                "ослепление — нельзя атаковать".to_string(),
            ));
        }
        None
    }

    pub(super) fn char_is_harvesting(&self) -> bool {
        let Some(char_id) = self.selected_char_id.as_ref() else {
            return false;
        };
        let world = self.state.world.lock().expect("мир");
        world.is_harvesting(char_id)
    }

    /// M24: гейт админ-команд — только роль `admin` (§9 E_PERMISSION;
    /// moderator проходит лишь модерацию чата, M14/F19).
    pub(super) fn require_admin(&self, seq: u32) -> Option<Out> {
        if self.role == "admin" {
            None
        } else {
            Some(Self::err(
                seq,
                ErrorCode::Permission,
                "требуется роль admin",
            ))
        }
    }

    // ── помощники ────────────────────────────────────────────────────────────

    /// Окно rate-limit auth-команд по IP (D30); `Some(retry_after_s)` — отклон.
    pub(super) fn rate_limit(&self) -> Option<u64> {
        self.state.rate.check(self.peer.ip())
    }

    /// R15: per-session окно на мутирующие команды (§9: `E_RATE_LIMITED`
    /// + `retry_after_s`). Окно 60 с; лимит — `[gameplay]
    /// cmd_rate_per_min` (0 — выключен).
    pub(super) fn cmd_rate_gate(&mut self, seq: u32) -> Option<Out> {
        let limit = self.state.config.gameplay.cmd_rate_per_min;
        if limit == 0 {
            return None;
        }
        let now = crate::time::now_epoch();
        if now.saturating_sub(self.cmd_rate_window.0) >= 60 {
            self.cmd_rate_window = (now, 0);
        }
        self.cmd_rate_window.1 += 1;
        if self.cmd_rate_window.1 > limit {
            let retry = (60 - now.saturating_sub(self.cmd_rate_window.0)).max(1) as u64;
            return Some(Self::ack_rate_limited(seq, retry));
        }
        None
    }

    /// M14/F19: может ли аккаунт писать в system-чат и удалять сообщения.
    pub(super) fn is_moderator(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "moderator")
    }

    pub(super) fn require_auth(&self, seq: u32) -> Option<Out> {
        if self.authenticated {
            None
        } else {
            Some(Self::err(
                seq,
                ErrorCode::AuthExpired,
                "нет активной сессии",
            ))
        }
    }

    pub(super) fn payload_of<T: DeserializeOwned>(env: &Envelope, seq: u32) -> Result<T, Out> {
        serde_json::from_value(env.payload.clone().unwrap_or(Value::Null)).map_err(|e| {
            Self::err(
                seq,
                ErrorCode::Internal,
                format!("некорректный payload: {e}"),
            )
        })
    }

    pub(super) fn err(seq: u32, code: ErrorCode, message: impl Into<String>) -> Out {
        out_ack(Ack::error(seq, None, &code, message))
    }

    pub(super) fn ack_payload<T: serde::Serialize>(seq: u32, payload: &T) -> Out {
        match Ack::ok_with_payload(seq, None, payload) {
            Ok(ack) => out_ack(ack),
            Err(e) => Self::err(seq, ErrorCode::Internal, format!("encode: {e}")),
        }
    }

    pub(super) fn ack_rate_limited(seq: u32, retry_after_s: u64) -> Out {
        let mut ack = Ack::error(
            seq,
            None,
            &ErrorCode::RateLimited,
            "слишком много попыток — повторите позже",
        );
        if let Some(err) = ack.error.as_mut() {
            err.retry_after_s = Some(retry_after_s);
        }
        out_ack(ack)
    }
    /// M17/F30g: гейт кулдауна предмета (cooldown_s, дефолт 1 с) —
    /// активный кулдаун → `Some(E_COOLDOWN + retry_after_s)`.
    pub(super) async fn item_cd_gate(
        &self,
        seq: u32,
        char_id: &str,
        def: &db::items::ItemDefRow,
    ) -> Option<Out> {
        let key = format!("item:{}", def.id);
        let left = {
            let world = self.state.world.lock().expect("мир");
            world.ability_cooldown_left(char_id, &key, crate::time::now_epoch())
        };
        let left = left?;
        let mut ack = Ack::error(seq, None, &ErrorCode::Cooldown, "предмет перезаряжается");
        if let Some(err) = ack.error.as_mut() {
            err.retry_after_s = Some(left.max(1) as u64);
        }
        Some(out_ack(ack))
    }

    /// M17/F30g: зафиксировать кулдаун предмета → событие клиенту
    /// (таймер + прогресс-бар кнопки).
    pub(super) fn item_cd_started(&self, char_id: &str, def: &db::items::ItemDefRow) -> Out {
        let cd = def.cooldown_s.max(0.001);
        let until = crate::time::now_epoch() + cd.ceil() as i64;
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_ability_cooldown(char_id, &format!("item:{}", def.id), until);
        }
        out_event(
            names::EVT_COMBAT_COOLDOWN,
            None,
            serde_json::to_value(CombatCooldown {
                skill_id: Some(format!("item:{}", def.id)),
                until_epoch: until,
                duration_ms: Some((cd * 1000.0).round() as i64),
            })
            .unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RF5a: стан покрывает разделку и сверх неё — ровно команды предметов.
    #[test]
    fn stun_covers_harvest_plus_item_actions() {
        for c in HARVEST_BLOCKED {
            assert!(
                stun_blocked(c),
                "{c} из HARVEST_BLOCKED должен блокироваться станом"
            );
        }
        for c in ITEM_ACTIONS {
            assert!(
                stun_blocked(c),
                "{c} из ITEM_ACTIONS должен блокироваться станом"
            );
            assert!(
                !HARVEST_BLOCKED.contains(c),
                "{c} не должен дублироваться в HARVEST_BLOCKED"
            );
        }
        let mut all: Vec<&str> = HARVEST_BLOCKED
            .iter()
            .chain(ITEM_ACTIONS.iter())
            .copied()
            .collect();
        all.sort_unstable();
        let n = all.len();
        all.dedup();
        assert_eq!(all.len(), n, "в объединении есть дубли");
        assert_eq!(n, 41, "стан блокирует 35 + 6 команд");
    }
}
