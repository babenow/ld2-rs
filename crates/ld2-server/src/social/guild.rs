//! Приглашения в гильдию (M12.7d): эфемерны, сама гильдия — в БД.

use super::SocialState;

/// Приглашение в гильдию (§11 `invite` события `evt/guild/updated`).
#[derive(Debug, Clone)]
pub struct GuildInvite {
    pub to: String,
    pub from: String,
    pub from_name: String,
    pub guild_id: String,
    pub guild_name: String,
}

impl SocialState {
    /// Выдать приглашение в гильдию (перезапись прошлого адресата).
    pub fn guild_invite(
        &mut self,
        to: &str,
        from: &str,
        from_name: &str,
        guild_id: &str,
        guild_name: &str,
    ) -> String {
        self.guild_invites.retain(|_, i| i.to != to);
        self.guild_invite_seq += 1;
        let id = format!("ginv_{}", self.guild_invite_seq);
        self.guild_invites.insert(
            id.clone(),
            GuildInvite {
                to: to.to_string(),
                from: from.to_string(),
                from_name: from_name.to_string(),
                guild_id: guild_id.to_string(),
                guild_name: guild_name.to_string(),
            },
        );
        id
    }

    /// Забрать приглашение адресата (проверка владельца внутри).
    pub fn take_guild_invite(&mut self, char_id: &str, invite_id: &str) -> Option<GuildInvite> {
        let invite = self.guild_invites.get(invite_id)?;
        if invite.to != char_id {
            return None;
        }
        self.guild_invites.remove(invite_id)
    }

    /// Активные приглашения адресата: `(invite_id, guild_id)`.
    pub fn guild_invites_of(&self, char_id: &str) -> Vec<(String, String)> {
        self.guild_invites
            .iter()
            .filter(|(_, i)| i.to == char_id)
            .map(|(id, i)| (id.clone(), i.guild_id.clone()))
            .collect()
    }

    /// Payload приглашения для `evt/guild/updated` (§11).
    pub fn guild_invite_payload(&self, invite_id: &str) -> Option<serde_json::Value> {
        let i = self.guild_invites.get(invite_id)?;
        Some(serde_json::json!({
            "guild_id": i.guild_id,
            "guild_name": i.guild_name,
            "from": i.from,
            "from_name": i.from_name,
        }))
    }

    /// Отвязка при дисконнекте: приглашения снимаются.
    pub fn drop_guild_invites_for(&mut self, char_id: &str) {
        self.guild_invites.retain(|_, i| i.to != char_id);
    }
}
