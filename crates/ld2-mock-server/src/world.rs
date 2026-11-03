//! Общее состояние мока на уровне **сервера** (один [`World`] на все соединения).
//!
//! До M8 мок держал одну [`crate::session::Session`] на все WS-соединения —
//! два клиента перетирали `authenticated`/`char`, а события (чат, пати, сделка)
//! доставлялись только вызвавшему соединению. DoD M8 («2 клиента против мока»)
//! требует разделения:
//!
//! - [`crate::session::Session`] — **per-connection**: авторизация, персонаж,
//!   локация, превью улучшений (своя на каждое WS-соединение);
//! - [`World`] — **общий**: монотонный `state_version` (переживает reconnect,
//!   §3), реестр соединений (рассылка `evt/*` между клиентами), реестр игроков
//!   (роутинг whisper/приглашений), межигровое состояние (пати/сделка/гильдия)
//!   и сохранённые состояния аккаунтов (resume возвращает персонажа, §4.5).
//!
//! **Дисциплина локов:** только `session → world`, [`World`] никогда не лочит
//! сессию — рассылка идёт через `tokio::mpsc`-сендеры без блокировок.

use std::collections::HashMap;

use ld2_protocol::types::{
    CharacterState, CharacterSummary, CrimeLogEntry, GuildState, ItemSnapshot, LocationSnapshot,
    PartyState,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::fixtures;

/// Сообщение, адресованное соединению: текст в сокет или команда сессии.
#[derive(Debug)]
pub enum Deliver {
    /// Готовый JSON-конверт — прямо в WS-сокет этого соединения.
    Socket(String),
    /// Внутренняя команда сессии (применение обмена по сделке, M8.2).
    Cmd(SessionCmd),
}

/// Внутренние команды сессии, порождаемые общим состоянием ([`World`]).
#[derive(Debug)]
pub enum SessionCmd {
    /// Сделка состоялась: применить обмен к своему персонажу.
    TradeApply {
        /// Предметы, которые я получаю.
        gain: Vec<ItemSnapshot>,
        /// uid моих предметов, которые я отдаю.
        loss: Vec<String>,
    },
}

/// Сохранённое состояние аккаунта (переживает обрыв/переподключение, §4.5).
#[derive(Debug, Clone)]
pub struct Home {
    pub chars: Vec<CharacterSummary>,
    pub char: Option<CharacterState>,
    pub location: LocationSnapshot,
    pub crime_log: Vec<CrimeLogEntry>,
}

/// Запись об игроке в реестре (роутинг whisper/приглашений, ФР-13/14).
#[derive(Debug, Clone)]
pub struct PlayerEntry {
    pub conn_id: String,
    pub name: String,
    pub char_id: String,
    pub location_id: String,
}

/// Одна сторона сделки (ФР-15: у каждого своя перспектива `you/them`).
#[derive(Debug, Clone)]
pub struct TradeSide {
    pub char_id: String,
    pub conn_id: String,
    pub items: Vec<ItemSnapshot>,
    pub gold: i64,
    pub locked: bool,
}

/// Общая на сервер сделка: хранит ОБЕ стороны, каждому соединению выдаётся
/// зеркальная перспектива (`you` = своя сторона, `them` = чужая).
#[derive(Debug)]
pub struct TradeShared {
    pub offer_id: String,
    pub a: TradeSide,
    pub b: TradeSide,
    /// `offer_id` вида `of_{open}`; сторона, открывшая сделку.
    pub opener: String,
    /// Общая версия: каждое изменение = +1 (§8, сверка в `trade.propose/accept`).
    pub version: u64,
}

/// Входящее приглашение в группу (§7 `party.accept {invite_id}`).
#[derive(Debug, Clone)]
pub struct PartyInvite {
    pub invite_id: String,
    /// Кто пригласил (`char_id`).
    pub from: String,
    /// Кого пригласили (`char_id`).
    pub to: String,
}

/// Серверное состояние, общее для всех соединений мока.
pub struct World {
    /// Монотонный счётчик состояния (§3): общий на сервер, переживает
    /// reconnect — без этого `state.resync` после обрыва бессмыслен.
    state_version: u64,
    /// Доставка сообщений в соединения: `conn_id → sender`.
    conns: HashMap<String, UnboundedSender<Deliver>>,
    /// Регистр игроков: `char_id → запись` (роутинг по имени и по id).
    players: HashMap<String, PlayerEntry>,
    /// Обратный индекс: `conn_id → char_id` (отвязка при отключении).
    conn_player: HashMap<String, String>,
    /// Сохранённые состояния аккаунтов: `login → Home`.
    homes: HashMap<String, Home>,
    /// `refresh-токен → login` (resume без повторного пароля, §4.5).
    tokens: HashMap<String, String>,
    /// Глобальный счётчик id персонажей (создание — `c{n}`; фикстуры заняты
    /// c1/c2, поэтому стартует с 3 — два аккаунта не должны конфликтовать).
    char_counter: u64,

    // ── межигровое состояние (ФР-14…16) ──
    /// Группа (одна на сервер — для DoD двух клиентов достаточно).
    pub party: Option<PartyState>,
    /// Входящие приглашения в группу: `invite_id → приглашение`.
    pub invites: HashMap<String, PartyInvite>,
    /// Сделка (одна на сервер): обе стороны + общая версия.
    pub trade: Option<TradeShared>,
    /// Гильдия (одна на сервер).
    pub guild: Option<GuildState>,
    invite_counter: u64,
}

impl World {
    pub fn new() -> Self {
        Self {
            state_version: 1_000,
            conns: HashMap::new(),
            players: HashMap::new(),
            conn_player: HashMap::new(),
            homes: HashMap::new(),
            tokens: HashMap::new(),
            char_counter: 3,
            party: None,
            invites: HashMap::new(),
            trade: None,
            guild: None,
            invite_counter: 0,
        }
    }

    // ── state_version ──────────────────────────────────────────────────────

    /// Монотонный бамп версии состояния (§3).
    pub fn bump(&mut self) -> u64 {
        self.state_version += 1;
        self.state_version
    }

    /// Текущая версия без бампа (для `state.resync`).
    pub fn version(&self) -> u64 {
        self.state_version
    }

    // ── соединения и рассылка ──────────────────────────────────────────────

    /// Регистрирует канал доставки соединения (вызывается при подключении).
    pub fn register_conn(&mut self, conn_id: &str, tx: UnboundedSender<Deliver>) {
        self.conns.insert(conn_id.to_string(), tx);
    }

    /// Отвязывает соединение: убирает канал, игрока и его связку
    /// (вызывается из `Drop` сессии — один выход на одно соединение).
    pub fn unregister_conn(&mut self, conn_id: &str) {
        self.conns.remove(conn_id);
        if let Some(char_id) = self.conn_player.remove(conn_id) {
            self.players.remove(&char_id);
        }
    }

    /// Доставка произвольного текста (готовый конверт) конкретному соединению.
    pub fn send_to_conn(&self, conn_id: &str, text: String) {
        if let Some(tx) = self.conns.get(conn_id) {
            let _ = tx.send(Deliver::Socket(text));
        }
    }

    /// Рассылка всем соединениям, кроме вызвавшего.
    pub fn broadcast(&self, text: String, except: Option<&str>) {
        for (id, tx) in &self.conns {
            if Some(id.as_str()) == except {
                continue;
            }
            let _ = tx.send(Deliver::Socket(text.clone()));
        }
    }

    /// Внутренняя команда конкретной сессии (см. [`SessionCmd`]).
    pub fn send_cmd(&self, conn_id: &str, cmd: SessionCmd) {
        if let Some(tx) = self.conns.get(conn_id) {
            let _ = tx.send(Deliver::Cmd(cmd));
        }
    }

    // ── игроки ─────────────────────────────────────────────────────────────

    /// Привязывает персонажа к соединению (вызывается из `char.select`).
    pub fn bind_player(&mut self, conn_id: &str, char_id: &str, name: &str, location_id: &str) {
        // Переселение/перепривязка: снимаем прошлые связи этого conn/char.
        if let Some(old) = self
            .conn_player
            .insert(conn_id.to_string(), char_id.to_string())
        {
            if old != char_id {
                self.players.remove(&old);
            }
        }
        self.players.insert(
            char_id.to_string(),
            PlayerEntry {
                conn_id: conn_id.to_string(),
                name: name.to_string(),
                char_id: char_id.to_string(),
                location_id: location_id.to_string(),
            },
        );
    }

    /// Обновляет локацию игрока (проверка «в одной локации», ФР-15).
    pub fn set_player_location(&mut self, char_id: &str, location_id: &str) {
        if let Some(p) = self.players.get_mut(char_id) {
            p.location_id = location_id.to_string();
        }
    }

    /// Игрок по **имени адресата** whisper (ФР-13 `/w Имя текст`).
    pub fn player_by_name(&self, name: &str) -> Option<&PlayerEntry> {
        self.players.values().find(|p| p.name == name)
    }

    /// Игрок по `char_id` (`party.invite {player_id}` и пр.).
    pub fn player_by_char(&self, char_id: &str) -> Option<&PlayerEntry> {
        self.players.get(char_id)
    }

    /// Отправляет событие игроку (если он онлайн).
    pub fn send_to_player(&self, char_id: &str, text: String) {
        if let Some(p) = self.players.get(char_id) {
            self.send_to_conn(&p.conn_id, text);
        }
    }

    /// Все онлайн-игроки (для рассылки по локации, ФР-13).
    pub fn online_players(&self) -> Vec<PlayerEntry> {
        self.players.values().cloned().collect()
    }

    /// Все участники группы (conn_id) — рассылка `evt/party/updated`.
    pub fn party_member_conns(&self) -> Vec<String> {
        let Some(party) = self.party.as_ref() else {
            return Vec::new();
        };
        party
            .members
            .iter()
            .filter_map(|m| self.players.get(&m.id).map(|p| p.conn_id.clone()))
            .collect()
    }

    // ── аккаунты ───────────────────────────────────────────────────────────

    /// Сохраняет состояние аккаунта (вызывается из `Drop` сессии).
    pub fn save_home(&mut self, login: &str, home: Home) {
        self.homes.insert(login.to_string(), home);
    }

    /// Загружает состояние аккаунта (login/resume).
    pub fn load_home(&self, login: &str) -> Option<Home> {
        self.homes.get(login).cloned()
    }

    /// Регистрирует refresh-токен аккаунта (resume по токену, §4.5).
    pub fn bind_token(&mut self, refresh: &str, login: &str) {
        self.tokens.insert(refresh.to_string(), login.to_string());
    }

    /// Логин по refresh-токену (неизвестный токен → resume без аккаунта).
    pub fn login_by_token(&self, refresh: &str) -> Option<String> {
        self.tokens.get(refresh).cloned()
    }

    /// Свежий уникальный id персонажа (`char.create`, фикстуры заняли c1/c2).
    pub fn next_char_id(&mut self) -> String {
        let id = format!("c{}", self.char_counter);
        self.char_counter += 1;
        id
    }

    // ── приглашения в группу (§7 party.accept/decline) ─────────────────────

    /// Создаёт приглашение, возвращает его `invite_id`.
    pub fn new_invite(&mut self, from: &str, to: &str) -> String {
        self.invite_counter += 1;
        let id = format!("inv_{}", self.invite_counter);
        self.invites.insert(
            id.clone(),
            PartyInvite {
                invite_id: id.clone(),
                from: from.to_string(),
                to: to.to_string(),
            },
        );
        id
    }

    /// Извлекает приглашение (accept/decline забирают его из реестра).
    pub fn take_invite(&mut self, invite_id: &str) -> Option<PartyInvite> {
        self.invites.remove(invite_id)
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// Аккаунты мока: логин → свой refresh-токен (DoD M8 — второй клиент).
pub fn refresh_token_for(login: &str) -> String {
    if login == fixtures::LOGIN {
        fixtures::REFRESH_TOKEN.to_string()
    } else {
        format!("rt_{login}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_version_starts_at_contract_floor() {
        let mut w = World::new();
        assert_eq!(w.version(), 1_000, "§3: старт-версия снапшота");
        assert_eq!(w.bump(), 1_001);
    }

    #[test]
    fn register_and_unregister_conn_tracks_players() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = World::new();
        w.register_conn("conn1", tx);
        w.bind_player("conn1", "c9", "Астра", "l_village");
        assert_eq!(w.player_by_name("Астра").unwrap().conn_id, "conn1");
        w.unregister_conn("conn1");
        assert!(w.player_by_name("Астра").is_none(), "отвязка при drop");
        assert!(w.conns.is_empty());
    }

    #[test]
    fn invites_are_created_and_taken_once() {
        let mut w = World::new();
        let id = w.new_invite("c1", "c2");
        assert_eq!(w.take_invite(&id).unwrap().to, "c2");
        assert!(w.take_invite(&id).is_none(), "повторный accept не находит");
    }

    #[test]
    fn refresh_tokens_are_distinct_per_account() {
        assert_ne!(
            refresh_token_for(fixtures::LOGIN),
            refresh_token_for(fixtures::LOGIN2)
        );
        assert_eq!(
            refresh_token_for(fixtures::LOGIN),
            fixtures::REFRESH_TOKEN,
            "demo-токен не меняем — его хранят клиенты"
        );
    }
}
