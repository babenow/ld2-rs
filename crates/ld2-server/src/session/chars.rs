//! RF1d: персонажи (ФР-02, §5/§3) — список, создание, выбор и удаление,
//! восстановление последней сессии, сводки для `char.list`/`session.start`
//! и полный лист персонажа (§3).
//!
//! Методы `impl Session` переехали как есть; поля `Session` по-прежнему
//! приватны для модуля сессии.

use std::collections::{BTreeMap, HashMap};

use ld2_protocol::cmd::{CharCreate, CharDelete, CharSelect};
use ld2_protocol::envelope::{Ack, Envelope};
use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::{
    CharacterState, CharacterSummary, CrimeState, EquipMap, Hp, Mp, SkillSnapshot,
};
use ld2_protocol::ErrorCode;
use serde_json::Value;
use tracing::warn;

use super::rules::{item_snapshot, summary_of};
use super::{out_ack, out_event, refresh_char_runes, Out, Session, PROFILES};
use crate::db;

impl Session {
    pub(super) async fn char_list(&mut self, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let account_id = self.account_id.expect("require_auth выше");
        match db::characters::list(&self.state.pool, account_id).await {
            Ok(rows) => {
                let mut characters = Vec::with_capacity(rows.len());
                for row in &rows {
                    characters.push(self.summary_for(row).await);
                }
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "characters": characters }),
                )]
            }
            Err(e) => {
                warn!("char.list: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    pub(super) async fn char_create(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharCreate>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account_id = self.account_id.expect("require_auth выше");
        let pool = &self.state.pool;
        // Лимит слотов — из конфига (D25, DoD M2: конфиг, не код).
        let limit = i64::from(self.state.config.gameplay.char_slot_limit);
        match db::characters::count_alive(pool, account_id).await {
            Ok(count) if count >= limit => {
                return vec![Self::err(
                    seq,
                    ErrorCode::CharLimit,
                    "все слоты персонажей заняты",
                )];
            }
            Ok(_) => {}
            Err(e) => {
                warn!("char.create (лимит): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        if cmd.name.trim().chars().count() < 2 {
            return vec![Self::err(
                seq,
                ErrorCode::NameTaken,
                "имя занято или некорректно",
            )];
        }
        if !PROFILES.contains(&cmd.profile.as_str()) {
            return vec![Self::err(
                seq,
                ErrorCode::BadProfile,
                format!("неизвестный профиль: {}", cmd.profile),
            )];
        }
        // M18/F34: пол — `male`/`female`; старые клиенты не шлют пол →
        // `male` (обратная совместимость §11).
        let gender = match cmd.gender.as_deref() {
            None | Some("male") => "male",
            Some("female") => "female",
            Some(other) => {
                return vec![Self::err(
                    seq,
                    ErrorCode::BadGender,
                    format!("неизвестный пол: {other}"),
                )];
            }
        };
        match db::characters::create(pool, account_id, &cmd.name, &cmd.profile, gender).await {
            Ok(Some(row)) => {
                // M17/F30e: стартовый ресурс по профилю (WoW): воин —
                // ярость с нуля, лучник — полная энергия; мана (маг) —
                // дефолт колонок.
                let res = &self.state.config.gameplay.resource;
                let resource = match cmd.profile.as_str() {
                    "warrior" => Some((0, res.rage_max as i32)),
                    "archer" => Some((res.energy_max as i32, res.energy_max as i32)),
                    _ => None,
                };
                if let Some((cur, max)) = resource {
                    if let Err(e) = db::characters::set_resource(pool, &row.id, cur, max).await {
                        warn!("char.create (ресурс): {e}");
                    }
                }
                // M28/B2: стартовые сумки (4 слота) — сразу при создании.
                let inv = &self.state.config.gameplay.inventory;
                if let Err(e) = db::items::grant_bags(
                    pool,
                    &row.id,
                    i32::try_from(inv.bag_slots).unwrap_or(0),
                    db::items::STARTER_BAG_DEF,
                )
                .await
                {
                    warn!("char.create (сумки): {e}");
                }
                let character = self.summary_for(&row).await;
                vec![Self::ack_payload(
                    seq,
                    &serde_json::json!({ "character": character }),
                )]
            }
            Ok(None) => {
                vec![Self::err(
                    seq,
                    ErrorCode::NameTaken,
                    "имя занято или некорректно",
                )]
            }
            Err(e) => {
                warn!("char.create: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    pub(super) async fn char_select(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharSelect>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account_id = self.account_id.expect("require_auth выше");
        let Some(row) =
            (match db::characters::find(&self.state.pool, account_id, &cmd.char_id).await {
                Ok(row) => row,
                Err(e) => {
                    warn!("char.select: {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            })
        else {
            // Паритет с моком: не найден/чужой → «персонаж не найден».
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        };
        // §3: state_version монотонен на сессию персонажа и переживает
        // reconnect — бамп атомарный в БД.
        let state_version =
            match db::characters::bump_state_version(&self.state.pool, &row.id).await {
                Ok(v) => v as u64,
                Err(e) => {
                    warn!("char.select (state_version): {e}");
                    return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
                }
            };
        // M12.3: позиция (пустая → спавн с персистом, D27) и выбор —
        // для восстановления сессии при resume (§4.5).
        let (location_id, location) = self.resolve_location(&row.location_id, &row.id).await;
        let char_state = match self.character_state(&row).await {
            Ok(state) => state,
            Err(e) => {
                warn!("char.select (лист персонажа): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        // R10: привязка мира — до присвоения состояния сессии; занятый
        // другой сессией персонаж → `E_BUSY` (две WS-сессии не действуют
        // одним персонажем).
        let bound = {
            let mut world = self.state.world.lock().expect("мир");
            world.bind_char(
                &row.id,
                &row.name,
                self.out_tx.clone(),
                &self.peer.to_string(),
            )
        };
        if !bound {
            return vec![Self::err(
                seq,
                ErrorCode::Busy,
                "персонаж уже в игре в другой сессии",
            )];
        }
        self.char = Some(char_state);
        self.location_id = Some(location_id.clone());
        self.selected_char_id = Some(row.id.clone());
        // Реестр мира (M12.5): рассылка тика боя + мишени агрессии.
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_location(&row.id, &location_id);
        }
        // Присутствие: соседи по локации видят вход в игру.
        super::presence::announce_spawn(
            &self.state,
            &location_id,
            super::presence::player_entity(&row.id, &row.name),
            None,
        );
        // M17/F30f: чем бьёт — по экипированному оружию (кэш мира).
        self.refresh_char_hits_as(&row.id).await;
        if let Some(account_id) = self.account_id {
            if let Err(e) = db::accounts::set_last_char(&self.state.pool, account_id, &row.id).await
            {
                warn!("last_char_id не обновлён: {e}");
            }
        }
        let party = self.state.social.lock().expect("соц").party_of(&row.id);
        let guild = self.guild_state_of(&row.id).await;
        let snapshot = StateSnapshot {
            state_version,
            char: self.char.clone().expect("выше"),
            location,
            party,
            guild,
            server_ts: crate::time::now_epoch(),
            extra: Default::default(),
        };
        let mut outs = vec![
            out_ack(Ack::ok(seq, Some(state_version))),
            out_event(
                "evt/state/snapshot",
                Some(state_version),
                serde_json::to_value(&snapshot).unwrap_or_default(),
            ),
        ];
        // ФР-13/§5: реплей истории чата (server_history) после снапшота.
        outs.extend(self.chat_history_events().await);
        // M25: активные дебаффы контроля после перезахода — на
        // отображение (стан открывает окно с прогрессбаром).
        {
            let now = crate::time::now_ms();
            let world = self.state.world.lock().expect("мир");
            for kind in ["stun", "silence", "blind"] {
                if let Some(until) = world.char_effect_until(&row.id, kind, now) {
                    outs.push(out_event(
                        "evt/char/afflict",
                        None,
                        serde_json::to_value(ld2_protocol::evt::CharAfflict {
                            kind: kind.to_string(),
                            title: crate::world::effect_title(kind),
                            until_ms: until,
                        })
                        .unwrap_or_default(),
                    ));
                }
            }
        }
        outs
    }

    pub(super) async fn char_delete(&mut self, env: &Envelope, seq: u32) -> Vec<Out> {
        if let Some(err) = self.require_auth(seq) {
            return vec![err];
        }
        let Ok(cmd) = Self::payload_of::<CharDelete>(env, seq) else {
            return vec![Self::err(seq, ErrorCode::Internal, "некорректный payload")];
        };
        let account_id = self.account_id.expect("require_auth выше");
        let pool = &self.state.pool;
        let found = match db::characters::find(pool, account_id, &cmd.char_id).await {
            Ok(row) => row,
            Err(e) => {
                warn!("char.delete: {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        };
        if found.is_none() {
            return vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")];
        }
        match db::characters::count_alive(pool, account_id).await {
            Ok(count) if count <= 1 => {
                return vec![Self::err(
                    seq,
                    ErrorCode::CharLimit,
                    "нельзя удалить последнего персонажа",
                )];
            }
            Ok(_) => {}
            Err(e) => {
                warn!("char.delete (лимит): {e}");
                return vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")];
            }
        }
        match db::characters::soft_delete(pool, account_id, &cmd.char_id).await {
            Ok(0) => vec![Self::err(seq, ErrorCode::NameTaken, "персонаж не найден")],
            Ok(_) => {
                if self.selected_char_id.as_deref() == Some(cmd.char_id.as_str()) {
                    self.selected_char_id = None;
                    self.char = None;
                }
                vec![out_ack(Ack::ok(seq, None))]
            }
            Err(e) => {
                warn!("char.delete: {e}");
                vec![Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")]
            }
        }
    }

    /// Поднять последнего выбранного персонажа (если жив) + его локацию —
    /// resume → `state.resync` → снапшот (§3/§4.5).
    pub(super) async fn restore_last_char(&mut self, account_id: i64) {
        let last = match db::accounts::last_char(&self.state.pool, account_id).await {
            Ok(last) => last,
            Err(e) => {
                warn!("last_char_id: {e}");
                return;
            }
        };
        let Some(last) = last else {
            return;
        };
        match db::characters::find(&self.state.pool, account_id, &last).await {
            Ok(Some(row)) => {
                let (location_id, _) = self.resolve_location(&row.location_id, &row.id).await;
                match self.character_state(&row).await {
                    Ok(state) => {
                        // R10: занят другой живой сессией —
                        // авто-восстановление пропускаем (клиент останется
                        // на экране персонажей; повторный select даст
                        // понятный `E_BUSY`).
                        let bound = {
                            let mut world = self.state.world.lock().expect("мир");
                            world.bind_char(
                                &row.id,
                                &row.name,
                                self.out_tx.clone(),
                                &self.peer.to_string(),
                            )
                        };
                        if !bound {
                            warn!("восстановление: {} уже в игре — пропуск", row.id);
                        } else {
                            self.char = Some(state);
                            self.location_id = Some(location_id.clone());
                            self.selected_char_id = Some(row.id.clone());
                            {
                                let mut world = self.state.world.lock().expect("мир");
                                world.set_char_location(&row.id, &location_id);
                            }
                            // Присутствие: соседи видят авто-вход в игру.
                            super::presence::announce_spawn(
                                &self.state,
                                &location_id,
                                super::presence::player_entity(&row.id, &row.name),
                                None,
                            );
                            // M17/F30f: чем бьёт — по экипированному оружию.
                            self.refresh_char_hits_as(&row.id).await;
                        }
                    }
                    Err(e) => warn!("восстановление листа персонажа: {e}"),
                }
            }
            Ok(None) => {} // удалён — клиент начнёт с экрана персонажей
            Err(e) => warn!("восстановление персонажа {last}: {e}"),
        }
    }

    /// Сводка §5 для строки БД — с именем локации из сида.
    pub(super) async fn summary_for(&self, row: &db::characters::CharacterRow) -> CharacterSummary {
        let location = self.location_display(&row.location_id).await;
        summary_of(row, location)
    }

    /// Полный лист персонажа §3: строка БД + атрибуты (skills.md §1) +
    /// **все** навыки контента (вложенные — со своим XP, нет — нули) +
    /// инвентарь/экипировка (слоты — из конфига, D25).
    pub(super) async fn character_state(
        &self,
        row: &db::characters::CharacterRow,
    ) -> Result<CharacterState, sqlx::Error> {
        let pool = &self.state.pool;
        let mut attrs: BTreeMap<String, i64> =
            serde_json::from_value(row.attrs.clone()).unwrap_or_default();

        let mut learned = HashMap::new();
        for skill in db::skills::learned(pool, &row.id).await? {
            learned.insert(skill.skill_id, (skill.xp, skill.rank));
        }
        let mut skills = Vec::new();
        for skill in db::skills::content_skills(pool).await? {
            let (xp, rank) = learned.get(&skill.id).copied().unwrap_or((0, 0));
            let rank_u32 = rank.max(0) as u32;
            skills.push(SkillSnapshot {
                id: skill.id,
                name: Some(skill.name),
                // M14/F17: пустое описание старого сида → None (без поля).
                description: (!skill.description.is_empty()).then_some(skill.description),
                // M15/F28: группа (skills.md §3) — группировка вкладки.
                group: (!skill.skill_group.is_empty()).then_some(skill.skill_group),
                rank: rank_u32,
                xp,
                xp_to_next: crate::systems::xp_to_next(rank_u32, &self.state.config.skills),
            });
        }

        let mut equip: EquipMap = self
            .state
            .config
            .gameplay
            .inventory
            .equip_slots
            .iter()
            .map(|slot| (slot.clone(), None))
            .collect();
        let mut inventory = Vec::new();
        // M17/F30c: raw-сокеты надетой экипировки — из них считаются
        // бонусы самоцветов (правило контента, вручную). M18/F58f
        // (ADR-012): самоцветы разрешены и в оружии — бонус берётся
        // из любой надетой экипировки, не только брони.
        let mut equipped_sockets: Vec<Value> = Vec::new();
        for item in db::items::inventory(pool, &row.id).await? {
            let Some(def) = db::items::def(pool, &item.def_id).await? else {
                warn!("нет определения {} для предмета {}", item.def_id, item.uid);
                continue;
            };
            if let Some(slot) = &item.equipped_slot {
                equip.insert(slot.clone(), Some(item.uid.clone()));
                if def.sockets > 0 {
                    equipped_sockets.push(item.sockets.clone());
                }
            }
            inventory.push(item_snapshot(&item, &def));
        }
        // M17/F30c: самоцветы в сокетах надетой экипировки улучшают
        // основные характеристики — бонус в снапшот-attrs (виден в
        // «Атрибутах»/«Параметрах», участвует в требованиях и гейтах);
        // руны и прочие вставки отсекаются ниже по типу определения.
        for sockets_json in &equipped_sockets {
            let sockets: Vec<Value> =
                serde_json::from_value(sockets_json.clone()).unwrap_or_default();
            for socket in sockets {
                let Some(gem_id) = socket.get("def_id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Ok(Some(gem)) = db::items::def(pool, gem_id).await else {
                    continue;
                };
                if gem.item_type != "gem" {
                    continue;
                }
                let (Some(attr), Some(value)) = (
                    gem.stats.get("attr").and_then(|v| v.as_str()),
                    gem.stats.get("value").and_then(|v| v.as_i64()),
                ) else {
                    continue;
                };
                *attrs.entry(attr.to_string()).or_insert(0) += value;
            }
        }

        // M15/F24: уровень и шкала опыта (окно персонажа).
        let (level, xp_in_level, xp_to_next) =
            crate::systems::level_for_total(row.total_xp, &self.state.config.gameplay.level);

        // M15/F26: производные характеристики (вкладка «Параметры») —
        // из атрибутов, навыков и именно снаряжённых предметов.
        let equip_uids: std::collections::HashSet<&str> =
            equip.values().flatten().map(String::as_str).collect();
        let equipped: Vec<ld2_protocol::types::ItemSnapshot> = inventory
            .iter()
            .filter(|i| equip_uids.contains(i.uid.as_str()))
            .cloned()
            .collect();
        // M18/F47/ADR-010: бонусы надетых предметов — атрибуты в attrs
        // (туда же, где считаются самоцветы), навыки к рангу снапшота;
        // параметры применяет stats::derive.
        let mut skill_bonus: BTreeMap<String, i64> = BTreeMap::new();
        for item in &equipped {
            for bonus in item.bonuses.iter().flatten() {
                match bonus.kind.as_str() {
                    "attr" => *attrs.entry(bonus.id.clone()).or_insert(0) += bonus.value,
                    "skill" => *skill_bonus.entry(bonus.id.clone()).or_insert(0) += bonus.value,
                    _ => {}
                }
            }
        }
        for skill in skills.iter_mut() {
            if let Some(bonus) = skill_bonus.get(&skill.id) {
                skill.rank = (i64::from(skill.rank) + *bonus).max(0) as u32;
            }
        }
        let mut derived = crate::stats::derive(
            &attrs,
            &skills,
            &equipped,
            self.state.config.gameplay.combat.base_damage,
        );
        // M18/F58d (ADR-012 §6): активные баффы рун — из БД; применяются
        // к параметрам той же механикой, что бонусы предметов.
        let now = crate::time::now_epoch();
        let buff_rows = match db::buffs::active(pool, &row.id, now).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("баффы {}: {e}", row.id);
                Vec::new()
            }
        };
        let buffs: Vec<ld2_protocol::types::BuffState> = buff_rows
            .iter()
            .map(
                |(kind, title, stacks, expires_at, ..)| ld2_protocol::types::BuffState {
                    kind: kind.clone(),
                    title: title.clone(),
                    stacks: (*stacks).max(0) as u32,
                    expires_at: *expires_at,
                    extra: Default::default(),
                },
            )
            .collect();
        crate::stats::apply_buffs(
            &mut derived,
            &buff_rows
                .iter()
                .map(|(_, _, stacks, _, param, value, pct)| (param.clone(), *value, *pct, *stacks))
                .collect::<Vec<_>>(),
        );

        Ok(CharacterState {
            id: row.id.clone(),
            name: row.name.clone(),
            // M18/F58d (ADR-012 §6): активные баффы рун (стак + таймер).
            buffs,
            // M17/F30b: класс для правил экипировки на клиенте (F30e —
            // ресурс боя).
            profile: Some(row.profile.clone()),
            // M18/F34: пол для диалогов NPC (F35).
            gender: row.gender.clone(),
            attrs,
            skills,
            // Изученные профессии (§6, M12.6).
            professions: match db::professions::learned(pool, &row.id).await {
                Ok(rows) => rows
                    .iter()
                    .map(|(id, xp, rank, level)| {
                        self.profession_snapshot(id, *xp, *rank, *level as u32)
                    })
                    .collect(),
                Err(e) => {
                    warn!("профессии {}: {e}", row.id);
                    Vec::new()
                }
            },
            hp: Hp {
                cur: i64::from(row.hp_cur),
                max: i64::from(row.hp_max),
            },
            // M17/F30e: ресурс по профилю — ярость/энергия/мана.
            mp: {
                let max = crate::systems::resource_max(
                    row.profile.as_str(),
                    &self.state.config.gameplay.resource,
                    i64::from(row.mp_max),
                );
                Mp {
                    cur: i64::from(row.mp_cur).clamp(0, max),
                    max,
                }
            },
            xp_pool: row.xp_pool,
            level: Some(level),
            total_xp: Some(row.total_xp),
            xp_in_level: Some(xp_in_level),
            xp_to_next: Some(xp_to_next),
            derived: Some(derived),
            equip,
            inventory,
            crime: CrimeState {
                // Активные статусы §10 (переживают рестарт, D27).
                statuses: match db::crime::active_statuses(pool, &row.id, crate::time::now_epoch())
                    .await
                {
                    Ok(rows) => rows
                        .into_iter()
                        .map(|(kind, expires_at)| self.crime_status(&kind, expires_at))
                        .collect(),
                    Err(e) => {
                        warn!("crime (статусы {}): {e}", row.id);
                        Vec::new()
                    }
                },
            },
            // Журнал квестов: контент + прогресс (§6, ФР-12).
            quests: {
                let defs = db::quests::defs(pool).await.unwrap_or_default();
                let progress = db::quests::progress(pool, &row.id)
                    .await
                    .unwrap_or_default();
                defs.iter()
                    .map(|def| {
                        let p = progress.iter().find(|(id, _, _)| id == &def.0);
                        db::quests::build_state(def, p)
                    })
                    .collect()
            },
            extra: Default::default(),
        })
    }

    /// Перечитать лист персонажа из БД в сессию (после мутаций, для ack
    /// с `char` §7).
    pub(super) async fn reload_char(
        &mut self,
        seq: u32,
        char_id: &str,
    ) -> Result<CharacterState, Out> {
        let row = db::characters::get(&self.state.pool, char_id)
            .await
            .map_err(|e| {
                warn!("reload листа (БД): {e}");
                Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")
            })?
            .ok_or_else(|| Self::err(seq, ErrorCode::NameTaken, "персонаж не найден"))?;
        self.character_state(&row)
            .await
            .map_err(|e| {
                warn!("reload листа: {e}");
                Self::err(seq, ErrorCode::Internal, "внутренняя ошибка")
            })
            .inspect(|state| {
                self.char = Some(state.clone());
            })
    }

    /// M17/F30f: обновить «чем бьёт» в мире — по экипированному
    /// оружию (main_hand); без него — «руками». Вызывается при входе
    /// в мир и после экипировки/снятия.
    pub(super) async fn refresh_char_hits_as(&self, char_id: &str) {
        let main = db::items::find_equipped_in_slot(&self.state.pool, char_id, "main_hand")
            .await
            .ok()
            .flatten();
        let def = match &main {
            Some(item) => db::items::def(&self.state.pool, &item.def_id)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let hits = def
            .as_ref()
            .map(|d| crate::combat_log::weapon_hits_as(d.skill.as_deref(), d.item_type.as_str()))
            .unwrap_or("руками");
        // M17/F30g: скорость атаки — cooldown_s оружия (нет оружия —
        // значение снимается, в plan берётся конфиг).
        let attack_ms = def
            .as_ref()
            .map(|d| (d.cooldown_s.max(0.001) * 1000.0).round() as i64);
        {
            let mut world = self.state.world.lock().expect("мир");
            world.set_char_hits_as(char_id, hits);
            world.set_char_attack_ms(char_id, attack_ms);
        }
        // M18/F58d: то же оружие — источник рун для авто-приёма.
        refresh_char_runes(&self.state, char_id).await;
    }
}
