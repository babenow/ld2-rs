//! Сделки (ФР-15, M12.7c): стороны, версии, блокировки, закрытие.

use super::SocialState;

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
