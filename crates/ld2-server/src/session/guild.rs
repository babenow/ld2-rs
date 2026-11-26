//! RF1s: гильдии (M22) — состояние, витрина, создание, вступление, выход,
//! приглашение/кик, ранги и лента событий.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{
    GuildBrowse, GuildCreate, GuildInvite, GuildJoin, GuildKick, GuildRankSet,
};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::event_payload_text;
use super::{Out, Session};
use crate::db;

impl Session {
    /// `GuildState` из БД: участники + online по реестру мира.
    pub(super) async fn guild_state(
        &self,
        guild_id: &str,
    ) -> Option<ld2_protocol::types::GuildState> {
        let row = db::guilds::find(&self.state.pool, guild_id).await.ok()??;
        let members = db::guilds::members(&self.state.pool, guild_id)
            .await
            .unwrap_or_default();
        let world = self.state.world.lock().expect("мир");
        let members = members
            .into_iter()
            .map(|(id, name, rank)| ld2_protocol::types::GuildMember {
                online: world.char_name(&id).is_some(),
                id,
                name,
                rank,
                extra: Default::default(),
            })
            .collect();
        Some(ld2_protocol::types::GuildState {
            id: row.0,
            name: row.1,
            members,
            extra: Default::default(),
        })
    }

    /// Рассылка `evt/guild/updated` адресатам (свой — в outs после ack).
    pub(super) fn guild_broadcast(
        &self,
        recipients: &[String],
        payload: &serde_json::Value,
        outs: &mut Vec<Out>,
        author: &str,
    ) {
        let Some(text) = event_payload_text("evt/guild/updated", payload) else {
            return;
        };
        let world = self.state.world.lock().expect("мир");
        for addr in recipients {
            if addr == author {
                outs.push(Out::Text(text.clone()));
            } else {
                world.send_to_char(addr, text.clone());
            }
        }
    }

    /// Гильдия персонажа для снапшота (§3 `guild: null` — не состоит).
    pub(super) async fn guild_state_of(
        &self,
        char_id: &str,
    ) -> Option<ld2_protocol::types::GuildState> {
        let guild = db::guilds::guild_of(&self.state.pool, char_id)
            .await
            .ok()
            .flatten()?;
        self.guild_state(&guild.0).await
    }

    /// `cmd/guild.browse` (§7): поиск гильдий → `{guilds[]}`.
    pub(super) async fn guild_browse(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildBrowse>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let guilds = match db::guilds::browse(&self.state.pool, cmd.query.as_deref()).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect::<Vec<_>>(),
            Err(e) => {
                warn!("guild.browse: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guilds": guilds }),
        )]
    }

    /// `cmd/guild.create` (§7): создание (правило сервера: имя уникально,
    /// автор — лидер) → ack `{guild}` + журнал.
    pub(super) async fn guild_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildCreate>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let name = cmd.name.trim();
        if name.is_empty() || name.chars().count() > 50 {
            return vec![Self::err(
                seq,
                ErrorCode::Internal,
                "имя гильдии: 1..=50 символов",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "вы уже в гильдии")];
        }
        let guild_id = match db::guilds::create(&self.state.pool, name, &char_id).await {
            Ok(Some(id)) => id,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::GuildLimit, "имя занято")];
            }
            Err(e) => {
                warn!("guild.create: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let state = self.guild_state(&guild_id).await;
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guild": state }),
        )];
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.join` (§7): вступление (открытое или по приглашению —
    /// `guild_id` из инвайта) → ack `{guild}`.
    pub(super) async fn guild_join(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildJoin>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "вы уже в гильдии")];
        }
        // Приглашение (если есть) подтверждает вступление и закрывается.
        let invited = {
            let mut social = self.state.social.lock().expect("соц");
            let mut found = None;
            if let Some((invite_id, _)) = social
                .guild_invites_of(&char_id)
                .into_iter()
                .find(|(_, gid)| cmd.guild_id.as_deref().is_none_or(|g| g == gid))
            {
                if let Some(invite) = social.take_guild_invite(&char_id, &invite_id) {
                    found = Some(invite.guild_id);
                }
            }
            found
        };
        let guild_id = match cmd.guild_id.clone().or(invited) {
            Some(id) => id,
            None => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "нет приглашения в гильдию",
                )];
            }
        };
        let Some(guild) = (match db::guilds::find(&self.state.pool, &guild_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.join (гильдия): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::GuildPermission, "гильдии нет")];
        };
        match db::guilds::join(&self.state.pool, &guild_id, &char_id, "member").await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(seq, ErrorCode::GuildLimit, "уже состоите")];
            }
            Err(e) => {
                warn!("guild.join: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let state = self.guild_state(&guild_id).await;
        let mut outs = vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "guild": state }),
        )];
        // Событие — всем участникам (включая нового).
        if let Some(state) = &state {
            let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        let _ = guild;
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.leave` (§7): выход (лидер передаёт ранг первому).
    pub(super) async fn guild_leave(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Перед выходом лидера — ранг первому оставшемуся (атомарно в БД).
        let my_rank = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten();
        if my_rank.as_deref() == Some("leader") {
            let _ = self.promote_first_member(&char_id).await;
        }
        let closed = match db::guilds::leave(&self.state.pool, &char_id).await {
            Ok(id) => id,
            Err(e) => {
                warn!("guild.leave: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let Some(guild_id) = closed else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        // Оставшимся — обновлённый состав; ушедшему — guild: null.
        if let Some(state) = self.guild_state(&guild_id).await {
            let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        let null = serde_json::json!({ "guild": serde_json::Value::Null });
        self.guild_broadcast(std::slice::from_ref(&char_id), &null, &mut outs, "");
        self.guild_log_event(&guild_id, &mut outs).await;
        outs
    }

    /// `cmd/guild.invite` (§7): приглашение → `evt/guild/updated` адресату.
    pub(super) async fn guild_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildInvite>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let my_rank = db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten();
        let Some(my_rank) = my_rank else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if !matches!(my_rank.as_str(), "leader" | "officer") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нет прав приглашать",
            )];
        }
        let Some(guild) = (match db::guilds::guild_of(&self.state.pool, &char_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.invite: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        {
            let world = self.state.world.lock().expect("мир");
            if world.char_name(&cmd.player_id).is_none() {
                return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
            }
        }
        if db::guilds::rank_of(&self.state.pool, &cmd.player_id)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return vec![Self::err(seq, ErrorCode::GuildLimit, "игрок уже в гильдии")];
        }
        let from_name = self.current_name();
        let invite_id = {
            let mut social = self.state.social.lock().expect("соц");
            social.guild_invite(&cmd.player_id, &char_id, &from_name, &guild.0, &guild.1)
        };
        let payload = {
            let social = self.state.social.lock().expect("соц");
            let invite = social.guild_invite_payload(&invite_id);
            let mut payload = serde_json::json!({ "guild": serde_json::Value::Null });
            if let Some(invite) = invite {
                payload["invite"] = invite;
            }
            payload
        };
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        self.guild_broadcast(
            std::slice::from_ref(&cmd.player_id),
            &payload,
            &mut outs,
            "",
        );
        outs
    }

    /// `cmd/guild.kick` (§7): исключение (leader/officer).
    pub(super) async fn guild_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildKick>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // R1: гильдия и ранг кикера одной выборкой — кик только в своей.
        let Some((my_guild_id, my_rank)) = db::guilds::membership(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
        else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if !matches!(my_rank.as_str(), "leader" | "officer") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нет прав исключать",
            )];
        }
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нельзя исключить себя (выйдите через guild.leave)",
            )];
        }
        // Нельзя исключить лидера; офицер не исключает офицера.
        // R1: цель — только участник той же гильдии (раньше офицер A
        // выкидывал участника B: kick не фильтровал по guild_id).
        let Some((target_guild_id, target_rank)) =
            db::guilds::membership(&self.state.pool, &cmd.player_id)
                .await
                .ok()
                .flatten()
        else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "игрок не в гильдии",
            )];
        };
        if target_guild_id != my_guild_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "игрок не в вашей гильдии",
            )];
        }
        match target_rank.as_str() {
            "leader" => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "нельзя исключить лидера",
                )];
            }
            "officer" if my_rank == "officer" => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "офицер не может исключить офицера",
                )];
            }
            _ => {}
        }
        match db::guilds::kick(&self.state.pool, &my_guild_id, &cmd.player_id).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Err(e) => {
                warn!("guild.kick: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let guild_id = db::guilds::guild_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .map(|g| g.0);
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(guild_id) = &guild_id {
            if let Some(state) = self.guild_state(guild_id).await {
                let members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
                let payload = serde_json::json!({ "guild": state });
                self.guild_broadcast(&members, &payload, &mut outs, &char_id);
            }
            let null = serde_json::json!({ "guild": serde_json::Value::Null });
            self.guild_broadcast(std::slice::from_ref(&cmd.player_id), &null, &mut outs, "");
            self.guild_log_event(guild_id, &mut outs).await;
        }
        outs
    }

    /// `cmd/guild.rank_set` (§7): смена ранга (атомарно; только leader).
    pub(super) async fn guild_rank_set(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<GuildRankSet>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        if !matches!(cmd.rank.as_str(), "leader" | "officer" | "member") {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "неизвестный ранг",
            )];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let Some(guild) = (match db::guilds::guild_of(&self.state.pool, &char_id).await {
            Ok(g) => g,
            Err(e) => {
                warn!("guild.rank_set: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "вы не в гильдии",
            )];
        };
        if db::guilds::rank_of(&self.state.pool, &char_id)
            .await
            .ok()
            .flatten()
            .as_deref()
            != Some("leader")
        {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "только лидер меняет ранги",
            )];
        }
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::GuildPermission,
                "нельзя менять ранг себе",
            )];
        }
        match db::guilds::set_rank(&self.state.pool, &guild.0, &cmd.player_id, &cmd.rank).await {
            Ok(true) => {}
            Ok(false) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::GuildPermission,
                    "игрок не в гильдии",
                )];
            }
            Err(e) => {
                warn!("guild.rank_set: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
        if let Some(state) = self.guild_state(&guild.0).await {
            let mut members: Vec<String> = state.members.iter().map(|m| m.id.clone()).collect();
            if !members.contains(&cmd.player_id) {
                members.push(cmd.player_id.clone());
            }
            let payload = serde_json::json!({ "guild": state });
            self.guild_broadcast(&members, &payload, &mut outs, &char_id);
        }
        self.guild_log_event(&guild.0, &mut outs).await;
        outs
    }

    /// Реплей журнала гильдии → события `evt/guild/log` (ФР-16: системный
    /// лог при входе; кольцо — последние 50).
    pub(super) async fn guild_log_event(&self, guild_id: &str, outs: &mut Vec<Out>) {
        let entries = match db::guilds::log_entries(&self.state.pool, guild_id).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("guild (журнал {guild_id}): {e}");
                return;
            }
        };
        for (kind, text, at) in entries {
            let payload = serde_json::json!({
                "entry": { "kind": kind, "text": text, "at": at }
            });
            if let Some(t) = event_payload_text("evt/guild/log", &payload) {
                outs.push(Out::Text(t));
            }
        }
    }
}
