//! Социальное состояние в памяти (M12.7): группы (ФР-14) — эфемерно
//! (D31: один шард), сделки/гильдии — в этой же структуре (M12.7c/d).

use std::collections::HashMap;

use ld2_protocol::types::{PartyMember, PartyState};

/// Максимум участников группы берётся из конфига (`party_max_members`).
#[derive(Default)]
pub struct SocialState {
    /// party_id → группа.
    parties: HashMap<String, PartyState>,
    /// offer_id → сделка (ФР-15, M12.7c).
    trades: HashMap<String, Trade>,
    /// char_id → offer_id активной сделки (индекс для быстрого поиска).
    char_trade: HashMap<String, String>,
    trade_seq: u64,
    /// invite_id → приглашение в гильдию (эфемерно; сама гильдия — в БД).
    guild_invites: HashMap<String, GuildInvite>,
    guild_invite_seq: u64,
    /// char_id → party_id (обратный индекс для быстрого поиска).
    char_party: HashMap<String, String>,
    /// invite_id → (адресат, приглашающий, имя приглашающего, party_id).
    invites: HashMap<String, Invite>,
    party_seq: u64,
    invite_seq: u64,
}

#[derive(Debug, Clone)]
struct Invite {
    to: String,
    from: String,
    from_name: String,
    party_id: String,
}

/// Результат операции с группой: события адресатам + (опц.) ответ для автора.
#[derive(Debug, Default)]
pub struct SocialEvents {
    /// (char_id, JSON payload `evt/party/updated`).
    pub party_updates: Vec<(String, serde_json::Value)>,
    /// (char_id, JSON payload `evt/party/updated` с `invite`) — приглашение.
    pub invites: Vec<(String, serde_json::Value)>,
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

/// Предложение стороны в сделке: предметы `(uid, qty)` и золото.
#[derive(Debug, Clone, Default)]
pub struct TradeSide {
    pub items: Vec<(String, i64)>,
    pub gold: i64,
    pub locked: bool,
}

/// Сделка (ФР-15): две стороны, version-safe — каждое изменение состава
/// бампает `version` и снимает блокировки (§11 `trade_version`).
#[derive(Debug, Clone)]
pub struct Trade {
    pub id: String,
    pub a: String,
    pub b: String,
    pub version: u64,
    pub side_a: TradeSide,
    pub side_b: TradeSide,
}

impl Trade {
    /// Сторона `char_id` (None — не участник).
    pub fn side_of(&self, char_id: &str) -> Option<&TradeSide> {
        if self.a == char_id {
            Some(&self.side_a)
        } else if self.b == char_id {
            Some(&self.side_b)
        } else {
            None
        }
    }

    fn side_of_mut(&mut self, char_id: &str) -> Option<&mut TradeSide> {
        if self.a == char_id {
            Some(&mut self.side_a)
        } else if self.b == char_id {
            Some(&mut self.side_b)
        } else {
            None
        }
    }

    fn other(&self, char_id: &str) -> &str {
        if self.a == char_id {
            &self.b
        } else {
            &self.a
        }
    }
}

/// Ошибка операции со сделкой → код §9 + текст.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeError {
    /// Сделка не найдена / участник не найден.
    Gone,
    /// Уже участвует в другой сделке.
    Busy,
    /// Состав изменился после предложенной версии.
    Stale,
    /// Предмет не принадлежит стороне / нет такого.
    Item,
    /// Золота меньше предложенного.
    Gold,
}

impl TradeError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Gone => "сделка не найдена",
            Self::Busy => "участник уже в другой сделке",
            Self::Stale => "состояние сделки изменилось — обновите окно",
            Self::Item => "предмет недоступен для сделки",
            Self::Gold => "недостаточно золота",
        }
    }
}

impl SocialState {
    /// Открыть сделку между двумя игроками (оба уже проверены онлайн).
    pub fn open_trade(&mut self, a: &str, b: &str) -> Result<String, TradeError> {
        if a == b {
            return Err(TradeError::Gone);
        }
        if self.char_trade.contains_key(a) || self.char_trade.contains_key(b) {
            return Err(TradeError::Busy);
        }
        self.trade_seq += 1;
        let id = format!("of_{}", self.trade_seq);
        self.trades.insert(
            id.clone(),
            Trade {
                id: id.clone(),
                a: a.to_string(),
                b: b.to_string(),
                version: 1,
                side_a: TradeSide::default(),
                side_b: TradeSide::default(),
            },
        );
        self.char_trade.insert(a.to_string(), id.clone());
        self.char_trade.insert(b.to_string(), id.clone());
        Ok(id)
    }

    /// Сделка по id.
    pub fn trade(&self, offer_id: &str) -> Option<&Trade> {
        self.trades.get(offer_id)
    }

    /// Положить предмет в предложение стороны; состав изменился →
    /// version+1 и снятие блокировок. `qty` — сколько из стака.
    pub fn trade_add_item(
        &mut self,
        char_id: &str,
        offer_id: &str,
        item_uid: &str,
        qty: i64,
    ) -> Result<&Trade, TradeError> {
        let trade = self.trades.get_mut(offer_id).ok_or(TradeError::Gone)?;
        if trade.side_of(char_id).is_none() {
            return Err(TradeError::Gone);
        }
        if qty < 1 {
            return Err(TradeError::Item);
        }
        let other = trade.other(char_id).to_string();
        let side = trade.side_of_mut(char_id).expect("проверка выше");
        if side.items.iter().any(|(uid, _)| uid == item_uid) {
            return Err(TradeError::Item); // дважды один uid — не поддерживаем
        }
        side.items.push((item_uid.to_string(), qty));
        side.locked = false;
        trade.side_of_mut(&other).expect("другая сторона").locked = false;
        trade.version += 1;
        Ok(&*trade)
    }

    /// Золото стороны (состав изменился → version+1, блокировки сняты).
    pub fn trade_set_gold(
        &mut self,
        char_id: &str,
        offer_id: &str,
        gold: i64,
    ) -> Result<&Trade, TradeError> {
        if gold < 0 {
            return Err(TradeError::Gold);
        }
        let trade = self.trades.get_mut(offer_id).ok_or(TradeError::Gone)?;
        if trade.side_of(char_id).is_none() {
            return Err(TradeError::Gone);
        }
        let other = trade.other(char_id).to_string();
        let side = trade.side_of_mut(char_id).expect("проверка выше");
        side.gold = gold;
        side.locked = false;
        trade.side_of_mut(&other).expect("другая сторона").locked = false;
        trade.version += 1;
        Ok(&*trade)
    }

    /// Блокировка стороны (`propose`/`accept`, §7: сверка `version`).
    /// Обе заблокированы → `true` (пора выполнять).
    pub fn trade_lock(
        &mut self,
        char_id: &str,
        offer_id: &str,
        version: u64,
    ) -> Result<bool, TradeError> {
        let trade = self.trades.get_mut(offer_id).ok_or(TradeError::Gone)?;
        if trade.side_of(char_id).is_none() {
            return Err(TradeError::Gone);
        }
        if trade.version != version {
            return Err(TradeError::Stale);
        }
        let side = trade.side_of_mut(char_id).expect("проверка выше");
        side.locked = true;
        Ok(trade.side_a.locked && trade.side_b.locked)
    }

    /// Закрыть сделку → (участники, причина кодом выше не нужна).
    pub fn close_trade(&mut self, offer_id: &str) -> Option<(Vec<String>, Trade)> {
        let trade = self.trades.remove(offer_id)?;
        self.char_trade.remove(&trade.a);
        self.char_trade.remove(&trade.b);
        Some((vec![trade.a.clone(), trade.b.clone()], trade))
    }

    /// Отвязка при дисконнекте: активная сделка отменяется → участники.
    pub fn drop_trades_for(&mut self, char_id: &str) -> Option<(Vec<String>, String)> {
        let offer_id = self.char_trade.get(char_id)?.clone();
        let (a, b) = {
            let trade = self.trades.get(&offer_id)?;
            (trade.a.clone(), trade.b.clone())
        };
        self.trades.remove(&offer_id);
        self.char_trade.remove(&a);
        self.char_trade.remove(&b);
        Some((vec![a, b], offer_id))
    }

    /// Индекс сделки участника.
    pub fn trade_of(&self, char_id: &str) -> Option<&str> {
        self.char_trade.get(char_id).map(|s| s.as_str())
    }
}

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
