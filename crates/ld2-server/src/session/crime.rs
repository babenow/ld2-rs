//! RF1t: преступления и розыск (M19) — журнал, добавление статуса и его
//! расчёт по сроку.
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::cmd::CrimeLog;
use ld2_protocol::envelope::Envelope;
use ld2_protocol::evt::CrimeAdded;
use ld2_protocol::types::{CrimeLogEntry, CrimeStatus, Severity};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::{Out, Session};
use crate::db;

impl Session {
    /// `cmd/crime.log` (§7): журнал нарушений (новые первые), limit —
    /// из payload (по умолчанию 50, максимум 500).
    pub(super) async fn crime_log(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CrimeLog>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let limit = cmd.limit.unwrap_or(50).clamp(1, 500) as i64;
        let entries = match db::crime::log(&self.state.pool, &char_id, limit).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(kind, at, delta_s, total_s)| CrimeLogEntry {
                    kind,
                    at,
                    delta_s,
                    total_s,
                })
                .collect::<Vec<_>>(),
            Err(e) => {
                warn!("crime.log: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "entries": entries }),
        )]
    }

    /// Статус из строки БД: severity — из `crime.kinds` конфига (D25).
    pub(super) fn crime_status(&self, kind: &str, expires_at: i64) -> CrimeStatus {
        let severity = self
            .state
            .config
            .gameplay
            .crime
            .kinds
            .iter()
            .find(|k| k.kind == kind)
            .map(|k| crate::auth::severity(&k.severity))
            .unwrap_or(Severity::Unknown);
        CrimeStatus {
            kind: kind.to_string(),
            severity,
            expires_at,
        }
    }

    /// Новое нарушение (§10): blend-эскалация срока → статус + журнал в БД
    /// (D27) → `evt/crime/added` адресату. `None` — неизвестный kind.
    pub(super) async fn apply_crime(&self, char_id: &str, kind: &str) -> Option<(String, String)> {
        let cfg = &self.state.config.gameplay.crime;
        // §5: ключи base — с суффиксом срока (`marauder_s`); kind — без.
        let base = cfg
            .base
            .get(kind)
            .or_else(|| cfg.base.get(&format!("{kind}_s")))
            .copied()? as i64;
        let now = crate::time::now_epoch();
        let t_cur = db::crime::total_term(&self.state.pool, char_id, now)
            .await
            .unwrap_or_else(|e| {
                warn!("crime (общий срок {char_id}): {e}");
                0
            });
        let (total, delta) =
            crate::systems::crime_blend(t_cur, base, cfg.t_max_s as i64, cfg.blend_k);
        if delta <= 0 {
            return None; // потолок §10 — вклада нет, статус не растёт
        }
        let (expires_at, _) =
            match db::crime::add_status(&self.state.pool, char_id, kind, now, delta, total).await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("crime (добавление {kind} {char_id}): {e}");
                    return None;
                }
            };
        let payload = CrimeAdded {
            status: self.crime_status(kind, expires_at),
            log_entry: CrimeLogEntry {
                kind: kind.to_string(),
                at: now,
                delta_s: delta,
                total_s: total,
            },
        };
        let text = crate::session::event_text(
            "evt/crime/added",
            None,
            serde_json::to_value(&payload).unwrap_or_default(),
        );
        if text.is_empty() {
            return None;
        }
        Some((char_id.to_string(), text))
    }
}
