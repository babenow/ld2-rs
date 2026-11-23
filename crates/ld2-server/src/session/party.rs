//! RF1r: группа (M22) — создание, приглашение/ответ, выход и кик.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::{PartyAccept, PartyInvite, PartyKick};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::ErrorCode;

use super::event_payload_text;
use super::{Out, Session};

impl Session {
    /// Рассылка `evt/party/updated` адресатам (свой конверт — в ack-ветке
    /// нельзя: события строго после своего ack, §2.2 — поэтому событие
    /// автору идёт через `outs`, остальным — в каналы соединений).
    pub(super) fn party_broadcast(
        &self,
        recipients: &[String],
        payload: &serde_json::Value,
        outs: &mut Vec<Out>,
        author: &str,
    ) {
        let text = event_payload_text("evt/party/updated", payload);
        let Some(text) = text else { return };
        let world = self.state.world.lock().expect("мир");
        for addr in recipients {
            if addr == author {
                outs.push(Out::Text(text.clone()));
            } else {
                world.send_to_char(addr, text.clone());
            }
        }
    }

    /// Payload `evt/party/updated` `{party}` (+ опц. `invite`, §11).
    pub(super) fn party_payload(
        party: Option<ld2_protocol::types::PartyState>,
        invite: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({ "party": party });
        if let Some(invite) = invite {
            payload["invite"] = invite;
        }
        payload
    }

    /// `cmd/party.create` (§7): создать группу (лидер — автор) → `{party}`.
    pub(super) async fn party_create(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let name = self.current_name();
        let mut social = self.state.social.lock().expect("соц");
        match social.create_party(&char_id, &name) {
            Some(party) => {
                drop(social);
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "party": party }),
                )]
            }
            None => {
                drop(social);
                vec![Self::err(seq, ErrorCode::Internal, "вы уже в группе")]
            }
        }
    }

    /// `cmd/party.invite` (§7): приглашение → `evt/party/updated` c
    /// `invite` адресату (§11).
    pub(super) async fn party_invite(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyInvite>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Адресат по id: в мире (онлайн) — иначе отказ.
        let target_name = {
            let world = self.state.world.lock().expect("мир");
            match world.char_name(&cmd.player_id) {
                Some(name) => name,
                None => {
                    return vec![Self::err(seq, ErrorCode::TargetGone, "игрок не в сети")];
                }
            }
        };
        let _ = target_name;
        let name = self.current_name();
        let mut social = self.state.social.lock().expect("соц");
        match social.invite(&char_id, &name, &cmd.player_id) {
            Ok(invite_id) => {
                let invite = social.invite_payload(&invite_id);
                let payload = Self::party_payload(None, invite);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                self.party_broadcast(
                    std::slice::from_ref(&cmd.player_id),
                    &payload,
                    &mut outs,
                    "",
                );
                outs
            }
            Err(e) => {
                drop(social);
                vec![Self::err(seq, ErrorCode::PartyFull, e.message())]
            }
        }
    }

    /// `cmd/party.accept` (§7): принять приглашение → `{party}` +
    /// `evt/party/updated` всем участникам.
    pub(super) async fn party_accept(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.party_answer(env, seq, true).await
    }

    /// `cmd/party.decline` (§7): отклонить → `{}`.
    pub(super) async fn party_decline(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        self.party_answer(env, seq, false).await
    }

    /// Общий обработчик accept/decline.
    pub(super) async fn party_answer(
        &mut self,
        env: &Envelope,
        seq: u32,
        accept: bool,
    ) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyAccept>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let name = self.current_name();
        let max = self.state.config.gameplay.party_max_members;
        let mut social = self.state.social.lock().expect("соц");
        if !accept {
            if social.decline_invite(&char_id, &cmd.invite_id) {
                return vec![Self::ack_payload(seq, &serde_json::json!({}))];
            }
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "приглашение не найдено",
            )];
        }
        match social.accept_invite(&char_id, &name, &cmd.invite_id, max) {
            Ok((party, members)) => {
                let payload = Self::party_payload(Some(party.clone()), None);
                drop(social);
                let mut outs = vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "party": party }),
                )];
                self.party_broadcast(&members, &payload, &mut outs, &char_id);
                outs
            }
            Err(e) => {
                drop(social);
                let code = match e {
                    crate::social::InviteError::Full => ErrorCode::PartyFull,
                    _ => ErrorCode::TargetGone,
                };
                vec![Self::err(seq, code, e.message())]
            }
        }
    }

    /// `cmd/party.leave` (§7): выйти → `{}` + `evt/party/updated` бывшим
    /// участникам (`party: null` — если группа распалась).
    pub(super) async fn party_leave(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut social = self.state.social.lock().expect("соц");
        match social.leave_party(&char_id) {
            Some((party, members)) => {
                // Оставшимся — обновлённый состав; ушедшему — party: null.
                let remaining: Vec<String> = members
                    .iter()
                    .filter(|id| *id != &char_id)
                    .cloned()
                    .collect();
                let payload = Self::party_payload(party, None);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                let left = Self::party_payload(None, None);
                self.party_broadcast(&remaining, &payload, &mut outs, "");
                self.party_broadcast(std::slice::from_ref(&char_id), &left, &mut outs, "");
                outs
            }
            None => {
                drop(social);
                vec![Self::err(seq, ErrorCode::TargetGone, "вы не в группе")]
            }
        }
    }

    /// `cmd/party.kick` (§7): исключить участника (только лидер).
    pub(super) async fn party_kick(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<PartyKick>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let mut social = self.state.social.lock().expect("соц");
        match social.kick_member(&char_id, &cmd.player_id) {
            Ok((party, members)) => {
                // Оставшимся — состав без исключённого; исключённому — null.
                let remaining: Vec<String> = members
                    .iter()
                    .filter(|id| *id != &cmd.player_id)
                    .cloned()
                    .collect();
                let payload = Self::party_payload(party, None);
                drop(social);
                let mut outs = vec![Self::ack_payload(seq, &serde_json::json!({}))];
                self.party_broadcast(&remaining, &payload, &mut outs, &char_id);
                let removed = Self::party_payload(None, None);
                self.party_broadcast(
                    std::slice::from_ref(&cmd.player_id),
                    &removed,
                    &mut outs,
                    "",
                );
                outs
            }
            Err(e) => {
                drop(social);
                vec![Self::err(seq, ErrorCode::GuildPermission, e.message())]
            }
        }
    }
}
