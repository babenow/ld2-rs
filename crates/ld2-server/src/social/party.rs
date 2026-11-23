//! Группы (ФР-14, M12.7b): создание, приглашения, вход/выход, исключение.

use ld2_protocol::types::{PartyMember, PartyState};

use super::SocialState;

#[derive(Debug, Clone)]
pub(super) struct Invite {
    to: String,
    from: String,
    from_name: String,
    party_id: String,
}

impl SocialState {
    /// Создать группу (лидер — `char_id`); `None` — уже состоит в другой.
    pub fn create_party(&mut self, char_id: &str, name: &str) -> Option<PartyState> {
        if self.char_party.contains_key(char_id) {
            return None;
        }
        self.party_seq += 1;
        let id = format!("pt_{}", self.party_seq);
        let party = PartyState {
            id: id.clone(),
            leader: char_id.to_string(),
            members: vec![member(char_id, name)],
            extra: Default::default(),
        };
        self.parties.insert(id.clone(), party.clone());
        self.char_party.insert(char_id.to_string(), id);
        Some(party)
    }

    /// Пригласить игрока в группу приглашающего; `Err` — причина отказа.
    /// M14: группы у приглашающего ещё нет — она создаётся автоматически
    /// (окно группы при этом скрыто, пока участников <2).
    pub fn invite(
        &mut self,
        from_id: &str,
        from_name: &str,
        to_id: &str,
    ) -> Result<String, InviteError> {
        if from_id == to_id {
            return Err(InviteError::SelfInvite);
        }
        // Отказы адресата — ДО автосоздания (иначе при отказе остаётся
        // пустая группа приглашающего).
        if self.char_party.contains_key(to_id) {
            return Err(InviteError::TargetInParty);
        }
        let party_id = match self.char_party.get(from_id).cloned() {
            Some(id) => id,
            None => {
                self.create_party(from_id, from_name)
                    .ok_or(InviteError::NoParty)?
                    .id
            }
        };
        // Одно активное приглашение на адресата (перезапись).
        self.invites.retain(|_, i| i.to != to_id);
        self.invite_seq += 1;
        let invite_id = format!("inv_{}", self.invite_seq);
        self.invites.insert(
            invite_id.clone(),
            Invite {
                to: to_id.to_string(),
                from: from_id.to_string(),
                from_name: from_name.to_string(),
                party_id,
            },
        );
        Ok(invite_id)
    }

    /// Принять приглашение → обновлённая группа; события — всем участникам
    /// (включая нового) и `party: null` бывшему лидеру-одиночке не нужен:
    /// группа уже существует.
    pub fn accept_invite(
        &mut self,
        char_id: &str,
        name: &str,
        invite_id: &str,
        max_members: u32,
    ) -> Result<(PartyState, Vec<String>), InviteError> {
        let invite = self
            .invites
            .remove(invite_id)
            .ok_or(InviteError::NoInvite)?;
        if invite.to != char_id {
            // Чужое приглашение вернуть в карту нельзя (moved) — отказ.
            return Err(InviteError::NoInvite);
        }
        let party = self
            .parties
            .get_mut(&invite.party_id)
            .ok_or(InviteError::NoParty)?;
        if party.members.len() as u32 >= max_members {
            return Err(InviteError::Full);
        }
        if party.members.iter().any(|m| m.id == char_id) {
            return Err(InviteError::AlreadyIn);
        }
        party.members.push(member(char_id, name));
        let updated = party.clone();
        let members: Vec<String> = updated.members.iter().map(|m| m.id.clone()).collect();
        self.char_party
            .insert(char_id.to_string(), updated.id.clone());
        Ok((updated, members))
    }

    /// Отклонить приглашение; `false` — не найдено/чужое.
    pub fn decline_invite(&mut self, char_id: &str, invite_id: &str) -> bool {
        self.invites.get(invite_id).is_some_and(|i| i.to == char_id)
            && self.invites.remove(invite_id).is_some()
    }

    /// Покинуть группу → (старая группа без участника, её участники);
    /// `None` — игрок не в группе. Лидерство переходит первому участнику;
    /// группа из одного участника распускается (`party: null` участникам).
    pub fn leave_party(&mut self, char_id: &str) -> Option<(Option<PartyState>, Vec<String>)> {
        let party_id = self.char_party.remove(char_id)?;
        let old_members: Vec<String> = self
            .parties
            .get(&party_id)
            .map(|p| p.members.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        let party = self.parties.get_mut(&party_id)?;
        party.members.retain(|m| m.id != char_id);
        if party.members.is_empty() {
            self.parties.remove(&party_id);
            return Some((None, old_members));
        }
        if party.leader == char_id {
            party.leader = party.members[0].id.clone();
        }
        let updated = party.clone();
        Some((Some(updated), old_members))
    }

    /// Исключить участника (только лидер); события — всем оставшимся.
    pub fn kick_member(
        &mut self,
        leader_id: &str,
        target_id: &str,
    ) -> Result<(Option<PartyState>, Vec<String>), InviteError> {
        let party_id = self
            .char_party
            .get(leader_id)
            .cloned()
            .ok_or(InviteError::NoParty)?;
        let party = self.parties.get(&party_id).ok_or(InviteError::NoParty)?;
        if party.leader != leader_id {
            return Err(InviteError::NotLeader);
        }
        if target_id == leader_id {
            return Err(InviteError::NotLeader);
        }
        if !party.members.iter().any(|m| m.id == target_id) {
            return Err(InviteError::NoTarget);
        }
        self.leave_party(target_id).ok_or(InviteError::NoTarget)
    }

    /// Группа игрока (`evt/party/updated` / снапшот §3).
    pub fn party_of(&self, char_id: &str) -> Option<PartyState> {
        let id = self.char_party.get(char_id)?;
        self.parties.get(id).cloned()
    }

    /// Участники группы игрока (для рассылки событий).
    pub fn party_members(&self, char_id: &str) -> Vec<String> {
        self.char_party
            .get(char_id)
            .and_then(|id| self.parties.get(id))
            .map(|p| p.members.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    }

    /// Участники группы по id.
    pub fn members_of(&self, party_id: &str) -> Vec<String> {
        self.parties
            .get(party_id)
            .map(|p| p.members.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    }

    /// Payload приглашения для `evt/party/updated` (§11 `invite`).
    pub fn invite_payload(&self, invite_id: &str) -> Option<serde_json::Value> {
        let i = self.invites.get(invite_id)?;
        Some(serde_json::json!({
            "invite_id": invite_id,
            "from": i.from,
            "from_name": i.from_name,
        }))
    }

    /// Отвязка при дисконнекте: игрок остаётся в группе (эфемерно),
    /// но активные приглашения ему — снимаются.
    pub fn drop_invites_for(&mut self, char_id: &str) {
        self.invites.retain(|_, i| i.to != char_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteError {
    NoParty,
    SelfInvite,
    TargetInParty,
    NoInvite,
    Full,
    AlreadyIn,
    NotLeader,
    NoTarget,
}

impl InviteError {
    /// Человеческий текст для `ack.error.message` (§9 — fallback).
    pub fn message(self) -> &'static str {
        match self {
            Self::NoParty => "вы не в группе",
            Self::SelfInvite => "нельзя пригласить себя",
            Self::TargetInParty => "игрок уже в группе",
            Self::NoInvite => "приглашение не найдено",
            Self::Full => "группа заполнена",
            Self::AlreadyIn => "вы уже в этой группе",
            Self::NotLeader => "только лидер может это сделать",
            Self::NoTarget => "игрок не в группе",
        }
    }
}

fn member(id: &str, name: &str) -> PartyMember {
    PartyMember {
        id: id.to_string(),
        name: name.to_string(),
        hp: None,
        location_id: None,
        ready: None,
        extra: Default::default(),
    }
}
