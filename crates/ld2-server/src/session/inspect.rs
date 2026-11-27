//! M35: ПКМ-осмотр персонажа — уровень, экипировка и изученные
//! профессии (инвентарь/атрибуты не раскрываются).

use ld2_protocol::cmd::CharInspect;
use ld2_protocol::envelope::Envelope;
use ld2_protocol::types::CharInspectView;
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::{Out, Session};
use crate::db;

impl Session {
    pub(super) async fn char_inspect(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharInspect>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let viewer = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        // Осмотр — только персонаж в той же локации (онлайновый).
        let location = self.current_location(&viewer).await;
        let same_location = {
            let world = self.state.world.lock().expect("мир");
            world.char_location(&cmd.char_id).as_deref() == Some(location.as_str())
        };
        if !same_location {
            return vec![Self::err(
                seq,
                ErrorCode::TargetGone,
                "персонаж не в этой локации",
            )];
        }
        let row = match db::characters::get(&self.state.pool, &cmd.char_id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return vec![Self::err(seq, ErrorCode::TargetGone, "персонаж не найден")];
            }
            Err(e) => {
                warn!("inspect (строка): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let state = match self.character_state(&row).await {
            Ok(state) => state,
            Err(e) => {
                warn!("inspect (лист): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // Только надетые вещи: equip — слот→uid, инвентарь — снимки.
        let mut equip = Vec::new();
        for (slot, uid) in &state.equip {
            let Some(uid) = uid else {
                continue;
            };
            let Some(item) = state.inventory.iter().find(|i| &i.uid == uid) else {
                continue;
            };
            equip.push(ld2_protocol::types::InspectItem {
                slot: slot.clone(),
                item: item.clone(),
            });
        }
        let view = CharInspectView {
            id: row.id.clone(),
            name: row.name.clone(),
            level: state.level.unwrap_or(1),
            profile: state.profile.clone(),
            equip,
            professions: state.professions,
        };
        vec![Self::ack_payload(seq, &serde_json::json!({ "char": view }))]
    }
}
