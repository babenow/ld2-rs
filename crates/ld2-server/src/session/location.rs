//! RF1e: локация персонажа (ФР-03, M12.3) — резолв и снапшот с оверлеем
//! живого мира, просмотр/переход по выходам, `state.resync` и карта мира
//! (M26).
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use ld2_protocol::names;

use ld2_protocol::cmd::{LocationMove, MapWorld, StateResync};
use ld2_protocol::envelope::{Ack, Envelope};
use ld2_protocol::evt::{LocationChanged, StateSnapshot};
use ld2_protocol::types::{EntityKind, LocationSnapshot};
use ld2_protocol::ErrorCode;
use tracing::warn;

use super::rules::location_snapshot;
use super::{out_ack, out_event, presence, Out, Session};
use crate::db;

impl Session {
    /// Локация персонажа: по `location_id`; пустой → спавн из сида (персист,
    /// D27); нет в БД (контент не засеян/удалён) → заглушка (D26).
    /// Снимок локации с оверлеем живого мира (мобы/трупы/ресурсы/
    /// наземные предметы — `World::build_location`; M20).
    pub(super) fn location_with_world(&self, snapshot: LocationSnapshot) -> LocationSnapshot {
        let mut snapshot = {
            let world = self.state.world.lock().expect("мир");
            world.build_location(snapshot)
        };
        // Присутствие: свой персонаж — не «рядом», из списка убираем
        // (у остальных он уже есть в снапшоте как `kind: player`).
        if let Some(me) = self.selected_char_id.as_deref() {
            snapshot
                .entities
                .retain(|e| !(e.kind == EntityKind::Player && e.id == me));
        }
        // M24: NPC с магазином — маркер `extra.shop` для чипов целей
        // админ-панели (клиент не знает content/shops.json).
        for entity in &mut snapshot.entities {
            if entity.kind == ld2_protocol::types::EntityKind::Npc
                && self.state.content().shops.contains_key(&entity.id)
            {
                entity
                    .extra
                    .insert("shop".to_string(), serde_json::json!(true));
            }
        }
        snapshot
    }

    pub(super) async fn resolve_location(
        &self,
        location_id: &str,
        char_id: &str,
    ) -> (String, LocationSnapshot) {
        let pool = &self.state.pool;
        if !location_id.is_empty() {
            return match db::locations::get(pool, location_id).await {
                Ok(Some(row)) => (row.id.clone(), self.location_with_world(row.snapshot())),
                Ok(None) => {
                    warn!("локация {location_id} отсутствует в сиде — заглушка");
                    (location_id.to_string(), location_snapshot(location_id))
                }
                Err(e) => {
                    warn!("чтение локации {location_id}: {e}");
                    (location_id.to_string(), location_snapshot(location_id))
                }
            };
        }
        // Пустая позиция (старый/новый персонаж) → спавн из meta.
        let spawn = match db::locations::spawn_id(pool).await {
            Ok(Some(spawn)) => spawn,
            Ok(None) => String::new(),
            Err(e) => {
                warn!("спавн из meta: {e}");
                String::new()
            }
        };
        if spawn.is_empty() {
            return (String::new(), location_snapshot(""));
        }
        if let Err(e) = db::locations::assign_spawn(pool, char_id, &spawn).await {
            warn!("спавн не персистнут: {e}");
        }
        match db::locations::get(pool, &spawn).await {
            Ok(Some(row)) => (spawn, self.location_with_world(row.snapshot())),
            Ok(None) => {
                warn!("спавн-локация {spawn} отсутствует в БД");
                let stub = location_snapshot(&spawn);
                (spawn, stub)
            }
            Err(e) => {
                warn!("чтение спавна {spawn}: {e}");
                let stub = location_snapshot(&spawn);
                (spawn, stub)
            }
        }
    }

    /// Отображаемое имя локации для сводок §5: пустой id → имя спавна.
    pub(super) async fn location_display(&self, location_id: &str) -> String {
        if !location_id.is_empty() {
            return match db::locations::location_name(&self.state.pool, location_id).await {
                Ok(Some(name)) => name,
                Ok(None) => location_id.to_string(),
                Err(e) => {
                    warn!("имя локации {location_id}: {e}");
                    location_id.to_string()
                }
            };
        }
        let spawn = db::locations::spawn_id(&self.state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if spawn.is_empty() {
            return String::new();
        }
        db::locations::location_name(&self.state.pool, &spawn)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// `cmd/location.look` (§7): полный снапшот текущей локации.
    pub(super) async fn location_look(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.location_id.clone().unwrap_or_default();
        let (location_id, location) = self.resolve_location(&current, &char_id).await;
        self.location_id = Some(location_id);
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({ "location": location }),
        )]
    }

    /// `cmd/location.move` (§7): переход по выходу — персист позиции + бамп
    /// `state_version` (§3/D27), ack с новой локацией, затем
    /// `evt/location/changed` (журнал мира, ФР-17; доставка остальным — M12.7).
    pub(super) async fn location_move(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<LocationMove>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let current = self.location_id.clone().unwrap_or_default();
        let Some(cur_row) = (match db::locations::get(&self.state.pool, &current).await {
            Ok(row) => row,
            Err(e) => {
                warn!("location.move (чтение {current}): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::ExitBlocked, "персонаж не в мире")];
        };
        let Some((_, target)) = cur_row.exit_target(&cmd.exit_id) else {
            return vec![Self::err(seq, ErrorCode::ExitBlocked, "выход недоступен")];
        };
        let state_version =
            match db::locations::move_char(&self.state.pool, &char_id, &target).await {
                Ok(v) => v as u64,
                Err(e) => {
                    warn!("location.move (персист): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        self.location_id = Some(target.clone());
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&char_id, &target);
            world.end_combat(&char_id); // смена локации прерывает бой (M12.5)
        }
        // Присутствие: соседи по старой локации видят уход, по новой —
        // появление (снапшот себе строим ниже, себя из него убираем).
        if current != target && !current.is_empty() {
            let (name, gender) = self
                .char
                .as_ref()
                .map(|c| (c.name.clone(), c.gender.clone()))
                .unwrap_or_else(|| {
                    (
                        presence::char_name(&self.state, &char_id).unwrap_or_default(),
                        "male".to_string(),
                    )
                });
            // M29 (заказчик): строка журнала — «{имя} {ушел|ушла} {выход}»;
            // выход — метка content (без неё — id), род — из персонажа.
            let went = if gender == "female" {
                "ушла"
            } else {
                "ушел"
            };
            let via = cur_row
                .exit_target(&cmd.exit_id)
                .map(|(exit, _)| exit.label)
                .unwrap_or_else(|| cmd.exit_id.clone());
            let line = format!("{name} {went} — {via}");
            presence::announce_despawn(&self.state, &current, &char_id, Some(line));
            presence::announce_spawn(
                &self.state,
                &target,
                presence::player_entity(&char_id, &name),
                Some(presence::came_line(&name, &gender)),
            );
        }
        let location = match db::locations::get(&self.state.pool, &target).await {
            Ok(Some(row)) => self.location_with_world(row.snapshot()),
            Ok(None) => {
                warn!("целевая локация {target} отсутствует в сиде — заглушка");
                location_snapshot(&target)
            }
            Err(e) => {
                warn!("чтение локации {target}: {e}");
                location_snapshot(&target)
            }
        };
        let ack = match Ack::ok_with_payload(
            seq,
            Some(state_version),
            &serde_json::json!({ "location": location.clone() }),
        ) {
            Ok(ack) => out_ack(ack),
            Err(e) => Self::err(seq, ErrorCode::Internal, format!("encode: {e}")),
        };
        let changed = out_event(
            names::EVT_LOCATION_CHANGED,
            Some(state_version),
            serde_json::to_value(LocationChanged { location }).unwrap_or_default(),
        );
        vec![ack, changed]
    }

    /// `cmd/state.resync` (§3): всегда полный снапшот (spec-легально: сервер
    /// «может достроить» delta, но полная замена стора — валидный ответ;
    /// генерация delta-патчей — отклонение M12.3). Версия — без бампа.
    pub(super) async fn state_resync(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(_cmd) = Self::payload_of::<StateResync>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let char_id = match self.require_char_id(seq) {
            Ok(id) => id,
            Err(out) => return vec![out],
        };
        let account_id = self.account_id.expect("require_auth выше");
        let Some(row) = (match db::characters::find(&self.state.pool, account_id, &char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("state.resync: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }) else {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        let state_version = row.state_version as u64;
        let (location_id, location) = self.resolve_location(&row.location_id, &row.id).await;
        self.location_id = Some(location_id);
        let char_state = match self.character_state(&row).await {
            Ok(state) => state,
            Err(e) => {
                warn!("state.resync (лист персонажа): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        let party = self.state.social.lock().expect("соц").party_of(&row.id);
        let guild = self.guild_state_of(&row.id).await;
        let snapshot = StateSnapshot {
            state_version,
            char: char_state,
            location,
            party,
            guild,
            server_ts: crate::time::now_epoch(),
            extra: Default::default(),
        };
        vec![
            out_ack(Ack::ok(seq, Some(state_version))),
            out_event(
                names::EVT_STATE_SNAPSHOT,
                Some(state_version),
                serde_json::to_value(&snapshot).unwrap_or_default(),
            ),
        ]
    }

    /// Текущая позиция из БД (источник истины): тик-смерть/респаун меняют
    /// её вне сессии — кэш синхронизируется (M12.5).
    pub(super) async fn current_location(&mut self, char_id: &str) -> String {
        match crate::db::characters::get(&self.state.pool, char_id).await {
            Ok(Some(row)) => {
                if self.location_id.as_deref() != Some(row.location_id.as_str()) {
                    self.location_id = Some(row.location_id.clone());
                }
                row.location_id
            }
            Ok(None) => self.location_id.clone().unwrap_or_default(),
            Err(e) => {
                warn!("чтение позиции {char_id}: {e}");
                self.location_id.clone().unwrap_or_default()
            }
        }
    }

    /// M26: карта-граф мира (§7 аддитивно): чанк статического графа
    /// локаций — `ack {locations, offset, total}`, повторять с
    /// `offset` до `offset >= total`. Лимит WS (64 КБ) не позволяет
    /// отдать граф целиком (~0.5 МБ) — чанки по байтовому бюджету.
    /// Граф статичен (контент-сид) — кэш один раз на процесс;
    /// висячие выходы (`to` вне контента) отфильтровываются.
    pub(super) async fn map_world(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<MapWorld>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        // M27: кэш инвалидируется hot reload'ом — при `None` строим заново
        // (под write-локом, чтобы параллельные запросы не строили дважды).
        // Снимок кэша: гвард бросаем сразу (блок), иначе временная жизнь
        // read-гварда в scrutinee `match` до конца выражения → дедлок на
        // write-локе в ветке `None` (первый запрос после hot reload).
        let cached = { self.state.map_cache.read().await.clone() };
        let cache = match cached {
            Some(cache) => cache,
            None => {
                let built = match crate::map::MapCache::build(&self.state.pool).await {
                    Ok(cache) => std::sync::Arc::new(cache),
                    Err(e) => {
                        warn!("map.world (кэш): {e}");
                        return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                    }
                };
                let mut guard = self.state.map_cache.write().await;
                match guard.as_ref() {
                    Some(cache) => cache.clone(),
                    None => {
                        *guard = Some(built.clone());
                        built
                    }
                }
            }
        };
        // Бюджет чанка: лимит WS (64 КБ) минус накладные поля ack.
        const CHUNK_BUDGET: usize = 48 * 1024;
        let (chunk, next) = cache.chunk(cmd.offset, CHUNK_BUDGET);
        vec![Self::ack_payload(
            seq,
            &serde_json::json!({
                "locations": chunk,
                "offset": next,
                "total": cache.total(),
                "root": cache.root(),
            }),
        )]
    }
}
