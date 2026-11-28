//! RF1q: обмен (M22) — открытие, добавление предметов и золота, фиксация,
//! исполнение транзакции и отмена.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::cmd::{TradeAddItem, TradeCancel, TradeOpen, TradePropose, TradeSetGold};
use ld2_protocol::envelope::Envelope;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::event_payload_text;
use super::rules::item_snapshot;
use super::{Out, Session};
use crate::db;

impl Session {
    /// Payload `evt/trade/state` для конкретного участника (зеркальная
    /// перспектива §8: `you` — своя сторона, `them` — чужая).
    pub(super) async fn trade_state_payload(
        &self,
        char_id: &str,
        trade: &crate::social::Trade,
    ) -> serde_json::Value {
        let pool = &self.state.pool;
        let (mine, theirs) = if trade.a == char_id {
            (&trade.side_a, &trade.side_b)
        } else {
            (&trade.side_b, &trade.side_a)
        };
        let snapshots = |side: &crate::social::TradeSide| {
            let pool = pool.clone();
            let uids: Vec<String> = side.items.iter().map(|(u, _)| u.clone()).collect();
            async move {
                let mut out = Vec::new();
                for uid in uids {
                    if let Ok(Some(item)) = db::items::find_any(&pool, &uid).await {
                        if let Ok(Some(def)) = db::items::def(&pool, &item.def_id).await {
                            out.push(item_snapshot(&item, &def));
                        }
                    }
                }
                out
            }
        };
        let you = snapshots(mine).await;
        let them = snapshots(theirs).await;
        serde_json::json!({
            "offer_id": trade.id,
            "version": trade.version,
            "you": you,
            "them": them,
            "you_gold": mine.gold,
            "them_gold": theirs.gold,
            "you_locked": mine.locked,
            "them_locked": theirs.locked,
        })
    }

    /// Рассылка `evt/trade/state` обоим участникам (зеркала разные).
    pub(super) async fn trade_broadcast_state(&self, trade: &crate::social::Trade) {
        let (a, b) = (trade.a.clone(), trade.b.clone());
        let payload_a = self.trade_state_payload(&a, trade).await;
        let payload_b = self.trade_state_payload(&b, trade).await;
        let text_a = event_payload_text(names::EVT_TRADE_STATE, &payload_a);
        let text_b = event_payload_text(names::EVT_TRADE_STATE, &payload_b);
        let world = self.state.world.lock().expect("мир");
        if let Some(t) = text_a {
            world.send_to_char(&a, t);
        }
        if let Some(t) = text_b {
            world.send_to_char(&b, t);
        }
    }

    /// Закрыть сделку событием `evt/trade/closed` (§8).
    pub(super) async fn trade_close_event(
        &self,
        offer_id: &str,
        participants: &[String],
        reason: &str,
        result: Option<serde_json::Value>,
    ) {
        let mut payload = serde_json::json!({ "offer_id": offer_id, "reason": reason });
        if let Some(result) = result {
            payload["result"] = result;
        }
        let Some(text) = event_payload_text(names::EVT_TRADE_CLOSED, &payload) else {
            return;
        };
        let world = self.state.world.lock().expect("мир");
        for addr in participants {
            world.send_to_char(addr, text.clone());
        }
    }

    /// `cmd/trade.open` (§7): открыть сделку — оба онлайн и в одной
    /// локации (ФР-15) → ack `{offer_id}` + `evt/trade/state` обоим.
    pub(super) async fn trade_open(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeOpen>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        if cmd.player_id == char_id {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "нельзя торговать с собой",
            )];
        }
        let location = self.current_location(&char_id).await;
        {
            let world = self.state.world.lock().expect("мир");
            let partner_loc = world.char_location(&cmd.player_id);
            if partner_loc.as_deref() != Some(location.as_str()) {
                return vec![Self::err(
                    seq,
                    ErrorCode::TradePartnerGone,
                    "партнёр не в вашей локации",
                )];
            }
            if world.char_name(&cmd.player_id).is_none() {
                return vec![Self::err(
                    seq,
                    ErrorCode::TradePartnerGone,
                    "партнёр не в сети",
                )];
            }
        }
        let opened = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .open_trade(&char_id, &cmd.player_id)
                .map(|id| social.trade(&id).cloned().expect("создана"))
        };
        match opened {
            Ok(trade) => {
                let offer_id = trade.id.clone();
                self.trade_broadcast_state(&trade).await;
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "offer_id": offer_id }),
                )]
            }
            Err(e) => vec![Self::err(seq, ErrorCode::TradeStale, e.message())],
        }
    }

    /// `cmd/trade.add_item` (§7): предмет в предложение (проверка
    /// владельца) → version+1, блокировки сняты → state обоим.
    pub(super) async fn trade_add_item(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeAddItem>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let pool = &self.state.pool;
        // Владелец и остаток стака — до изменения состава.
        let Some(item) = (match db::items::find(pool, &cmd.item_uid, &char_id).await {
            Ok(item) => item,
            Err(e) => {
                warn!("trade.add (поиск): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ItemGone, "предмета нет")];
        };
        // M39: персональный (привязанный) предмет нельзя передавать.
        if item.personal {
            return vec![Self::err(
                seq,
                ErrorCode::ItemPersonal,
                "персональный предмет нельзя передавать",
            )];
        }
        // R8: надетый предмет в сделку нельзя (перенос сбрасывал бы
        // `equipped_slot` у получателя — бонусы без экипирования).
        if item.equipped_slot.is_some() {
            return vec![Self::err(
                seq,
                ErrorCode::Busy,
                "предмет надет — снимите его перед сделкой",
            )];
        }
        let qty = cmd
            .qty
            .unwrap_or(i64::from(item.qty))
            .clamp(1, i64::from(item.qty));

        let added = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .trade_add_item(&char_id, &cmd.offer_id, &cmd.item_uid, qty)
                .cloned()
        };
        let trade = match added {
            Ok(trade) => trade,
            Err(e) => {
                let code = match e {
                    crate::social::TradeError::Stale => ErrorCode::TradeStale,
                    crate::social::TradeError::Gone => ErrorCode::TradePartnerGone,
                    _ => ErrorCode::ItemGone,
                };
                return vec![Self::err(seq, code, e.message())];
            }
        };
        self.trade_broadcast_state(&trade).await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/trade.set_gold` (§7): золото стороны (баланс — из БД).
    pub(super) async fn trade_set_gold(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeSetGold>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let balance =
            match db::items::count_by_type(&self.state.pool, &char_id, db::items::MONEY_TYPE).await
            {
                Ok(gold) => gold,
                Err(e) => {
                    warn!("trade.gold (баланс): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        if cmd.gold > balance {
            return vec![Self::err(
                seq,
                ErrorCode::TradeStale,
                format!("золота меньше: {balance}"),
            )];
        }
        let set = {
            let mut social = self.state.social.lock().expect("соц");
            social
                .trade_set_gold(&char_id, &cmd.offer_id, cmd.gold)
                .cloned()
        };
        let trade = match set {
            Ok(trade) => trade,
            Err(e) => {
                let code = match e {
                    crate::social::TradeError::Stale => ErrorCode::TradeStale,
                    _ => ErrorCode::TradePartnerGone,
                };
                return vec![Self::err(seq, code, e.message())];
            }
        };
        self.trade_broadcast_state(&trade).await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }

    /// `cmd/trade.propose` / `cmd/trade.accept` (§7): блокировка стороны
    /// со сверкой `version` (устаревшая → `E_TRADE_STALE`); обе стороны
    /// заблокированы → исполнение в одной транзакции (D27).
    pub(super) async fn trade_lock_cmd(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradePropose>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // R9: лок и клон сделки — под одним локом мьютекса (между
        // двумя скоупами партнёр успевал изменить состав/версию).
        let (both_locked, trade) = {
            let mut social = self.state.social.lock().expect("соц");
            let both_locked = match social.trade_lock(&char_id, &cmd.offer_id, cmd.version) {
                Ok(locked) => locked,
                Err(e) => {
                    drop(social);
                    let code = match e {
                        crate::social::TradeError::Stale => ErrorCode::TradeStale,
                        _ => ErrorCode::TradePartnerGone,
                    };
                    return vec![Self::err(seq, code, e.message())];
                }
            };
            (both_locked, social.trade(&cmd.offer_id).cloned())
        };
        let Some(trade) = trade else {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "сделка не найдена",
            )];
        };
        if !both_locked {
            self.trade_broadcast_state(&trade).await;
            return vec![Self::ack_payload(seq, &serde_json::json!({}))];
        }
        // R9: исполнение — только если обе стороны всё ещё заблокированы
        // и версия не изменилась (изменение состава бампает version и
        // снимает блокировки; исполнение неподтверждённого клона =
        // обход партнёра).
        let confirmed = {
            let social = self.state.social.lock().expect("соц");
            social
                .trade(&trade.id)
                .filter(|t| t.version == trade.version && t.side_a.locked && t.side_b.locked)
                .cloned()
        };
        let Some(trade) = confirmed else {
            self.trade_broadcast_state(&trade).await;
            return vec![Self::err(
                seq,
                ErrorCode::TradeStale,
                "сделка изменилась — подтвердите заново",
            )];
        };
        // Оба заблокированы → исполнение одной транзакцией (предметы+золото).
        match self.trade_execute(&trade).await {
            Ok(result) => {
                {
                    let mut social = self.state.social.lock().expect("соц");
                    social.close_trade(&trade.id);
                }
                self.trade_close_event(
                    &trade.id,
                    &[trade.a.clone(), trade.b.clone()],
                    "done",
                    Some(result),
                )
                .await;
                vec![Self::ack_payload(seq, &serde_json::json!({}))]
            }
            Err(e) => {
                {
                    let mut social = self.state.social.lock().expect("соц");
                    social.close_trade(&trade.id);
                }
                self.trade_close_event(
                    &trade.id,
                    &[trade.a.clone(), trade.b.clone()],
                    "failed",
                    Some(serde_json::json!({ "error": e })),
                )
                .await;
                vec![Self::err(seq, ErrorCode::TradeStale, e)]
            }
        }
    }

    /// Исполнение сделки: перемещение предметов и золота сторон **одной**
    /// транзакцией (D27); при любом конфликте — откат и `failed`.
    pub(super) async fn trade_execute(
        &self,
        trade: &crate::social::Trade,
    ) -> Result<serde_json::Value, String> {
        let pool = &self.state.pool;
        let mut tx = pool.begin().await.map_err(|e| format!("tx: {e}"))?;
        let sides = [
            (&trade.a, &trade.b, &trade.side_a),
            (&trade.b, &trade.a, &trade.side_b),
        ];
        let mut moved = Vec::new();
        for (from, to, side) in sides {
            for (uid, qty) in &side.items {
                // Предмет всё ещё у стороны? Частичный перенос стака.
                let cur: Option<(i32, String, bool)> = sqlx::query_as(
                    "SELECT qty, character_id, personal FROM items WHERE uid = $1 FOR UPDATE",
                )
                .bind(uid)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| format!("читаю {uid}: {e}"))?;
                let Some((cur_qty, owner, personal)) = cur else {
                    return Err(format!("предмет {uid} исчез"));
                };
                if personal {
                    return Err(format!("персональный предмет {uid} нельзя передавать"));
                }
                if owner != *from {
                    return Err(format!("предмет {uid} не у владельца"));
                }
                if i64::from(cur_qty) < *qty {
                    return Err(format!("предмет {uid}: не хватает стака"));
                }
                if i64::from(cur_qty) == *qty {
                    // R8: перенос сбрасывает «надетость» — получатель
                    // применяет бонусы только после экипирования.
                    sqlx::query(
                        "UPDATE items SET character_id = $2, equipped_slot = NULL WHERE uid = $1",
                    )
                    .bind(uid)
                    .bind(to)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("перенос {uid}: {e}"))?;
                } else {
                    sqlx::query("UPDATE items SET qty = qty - $2 WHERE uid = $1")
                        .bind(uid)
                        .bind(qty)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("уменьшение {uid}: {e}"))?;
                    let def_id: String =
                        sqlx::query_scalar("SELECT def_id FROM items WHERE uid = $1")
                            .bind(uid)
                            .fetch_one(&mut *tx)
                            .await
                            .map_err(|e| format!("деф {uid}: {e}"))?;
                    sqlx::query(
                        "INSERT INTO items (character_id, def_id, qty, sort_order) \
                         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
                         FROM items WHERE character_id = $1), 0))",
                    )
                    .bind(to)
                    .bind(&def_id)
                    .bind(qty)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("вставка {uid}: {e}"))?;
                }
                moved.push(serde_json::json!({ "uid": uid, "qty": qty }));
            }
            // F29a: деньги — стаки монет: списание у `from` (стаки
            // убывают, пустые удаляются) и начисление `to` (в старый
            // стак либо новым) — той же транзакцией, что и предметы.
            if side.gold > 0 {
                let rows: Vec<(String, i32)> = sqlx::query_as(
                    "SELECT i.uid, i.qty FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND d.item_type = $2 \
                     ORDER BY i.created_at, i.uid FOR UPDATE",
                )
                .bind(from)
                .bind(db::items::MONEY_TYPE)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| format!("монеты {from}: {e}"))?;
                let mut left = side.gold;
                for (uid, qty) in rows {
                    if left <= 0 {
                        break;
                    }
                    let take = i64::from(qty).min(left);
                    left -= take;
                    let rest = qty - take as i32;
                    if rest <= 0 {
                        sqlx::query("DELETE FROM items WHERE uid = $1")
                            .bind(&uid)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| format!("списание {uid}: {e}"))?;
                    } else {
                        sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
                            .bind(&uid)
                            .bind(rest)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| format!("стак {uid}: {e}"))?;
                    }
                }
                if left > 0 {
                    return Err(format!("недостаточно монет у {from}"));
                }
                let target: Option<String> = sqlx::query_scalar(
                    "SELECT i.uid FROM items i \
                     JOIN item_defs d ON d.id = i.def_id \
                     WHERE i.character_id = $1 AND d.item_type = $2 \
                     ORDER BY i.created_at, i.uid LIMIT 1 FOR UPDATE",
                )
                .bind(to)
                .bind(db::items::MONEY_TYPE)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| format!("стак {to}: {e}"))?;
                if let Some(uid) = target {
                    sqlx::query("UPDATE items SET qty = qty + $2 WHERE uid = $1")
                        .bind(&uid)
                        .bind(side.gold)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("зачисление {uid}: {e}"))?;
                } else {
                    let def_id: String = sqlx::query_scalar(
                        "SELECT id FROM item_defs WHERE item_type = $1 ORDER BY id LIMIT 1",
                    )
                    .bind(db::items::MONEY_TYPE)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| format!("определение валюты: {e}"))?
                    .ok_or_else(|| "нет определения валюты в контенте".to_string())?;
                    sqlx::query(
                        "INSERT INTO items (character_id, def_id, qty, sort_order) \
                         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
                         FROM items WHERE character_id = $1), 0))",
                    )
                    .bind(to)
                    .bind(&def_id)
                    .bind(side.gold)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("зачисление {to}: {e}"))?;
                }
            }
        }
        tx.commit().await.map_err(|e| format!("commit: {e}"))?;
        Ok(serde_json::json!({ "moved": moved }))
    }

    /// `cmd/trade.cancel` (§7): отмена любой стороной.
    pub(super) async fn trade_cancel(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<TradeCancel>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let closed = {
            let mut social = self.state.social.lock().expect("соц");
            let is_member = social
                .trade(&cmd.offer_id)
                .is_some_and(|t| t.a == char_id || t.b == char_id);
            if is_member {
                social.close_trade(&cmd.offer_id)
            } else {
                None
            }
        };
        let Some((participants, trade)) = closed else {
            return vec![Self::err(
                seq,
                ErrorCode::TradePartnerGone,
                "сделка не найдена",
            )];
        };
        self.trade_close_event(&trade.id, &participants, "cancelled", None)
            .await;
        vec![Self::ack_payload(seq, &serde_json::json!({}))]
    }
}
