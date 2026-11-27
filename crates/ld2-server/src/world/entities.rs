//! Трупы, узлы ресурсов и наземный лут мира (M20/F20a).

use super::*;
use crate::consts::{ACTION_GUT, ACTION_SKIN};

const RANK_MIN: u32 = 1;
const RANK_MAX: u32 = 5;
const ITEM_MEAT: &str = "meat";
const ITEM_OFFAL: &str = "offal";
const GUT_MEAT_CHANCE: f64 = 0.25;

/// Динамический труп (убитый моб): `loot[i] = Some(def)` — `None` уже взят.
#[derive(Debug, Clone)]
pub struct LiveCorpse {
    pub id: String,
    pub location_id: String,
    pub name: String,
    pub loot: Vec<Option<String>>,
    /// M20: когда труп создан (epoch-с) — TTL-удаление (`corpse_ttl_s`).
    pub born_at: i64,
    /// Снятие шкур: флаги разделки животного (из контента моба);
    /// `None` — разделывать нельзя.
    pub harvest: Option<serde_json::Map<String, serde_json::Value>>,
    /// Уже снята шкура / уже освежено (каждое — один раз).
    pub skin_done: bool,
    pub gut_done: bool,
    /// Лут **освежевания** (мясо/потроха) — отдельный блок «Внутри»
    /// в окне осмотра, не в общем списке.
    pub inner_loot: Vec<Option<String>>,
}

/// Снятие шкур: активная разделка (сервер-джоб; ключ в мире —
/// персонаж, одно действие за раз).
#[derive(Debug, Clone)]
pub struct HarvestJob {
    pub char_id: String,
    pub corpse_id: String,
    /// `skin` | `gut`.
    pub action: String,
    /// Имя животного (без «Труп:») — для журнала клиента.
    pub name: String,
    /// Epoch-с окончания (старт +5 с).
    pub at: i64,
}

/// M38: таблица бонусных самоцветов рудной жилы.
#[derive(Debug, Clone, Default)]
pub struct GemTable {
    /// Шанс, что вместе с рудой выпадет камень (0..1).
    pub chance: f64,
    /// Доля смешанных камней среди выпавших (0..1); остальные — чистые.
    pub mixed_share: f64,
    /// id необработанных смешанных самоцветов.
    pub mixed: Vec<String>,
    /// id необработанных чистых камней.
    pub pure: Vec<String>,
}

/// M20/F20b: объект-ресурс в локации (жила/дерево/трава): `current`
/// убывает по 1 за добычу, при 0 — респавн `[min_s, max_s]`; трава
/// (`despawn_when_empty`) при исчерпании скрывается до респавна.
#[derive(Debug, Clone)]
pub struct ResourceNode {
    pub id: String,
    pub location_id: String,
    pub name: String,
    /// Предмет-выход (content `resource`), 1 за добычу.
    pub def_id: String,
    pub max: i64,
    pub current: i64,
    pub respawn: (i64, i64),
    pub despawn_when_empty: bool,
    /// Скрыт до респавна (трава после исчерпания).
    pub hidden: bool,
    /// Требуемая профессия + ранг (отказ — `E_PROFESSION`).
    pub profession: Option<String>,
    pub min_rank: i32,
    /// Тип инструмента (`item_type` в инвентаре, `E_TOOL_MISSING`).
    pub tool: Option<String>,
    /// M38: требуемый уровень профессии для добычи.
    pub required_level: u32,
    /// M38: бонусные самоцветы жилы (руда — основной ресурс).
    pub gems: Option<GemTable>,
}

impl ResourceNode {
    /// Проводной вид §6 (spawn/снапшот): запас — как шкала hp
    /// (`{cur, max}`), клиент рисует бар и патчит через `entity/move`.
    pub fn entity(&self) -> Entity {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "required_level".into(),
            serde_json::json!(self.required_level),
        );
        extra.insert("min_rank".into(), serde_json::json!(self.min_rank));
        if let Some(gems) = &self.gems {
            extra.insert("gem_chance".into(), serde_json::json!(gems.chance));
        }
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Resource,
            name: self.name.clone(),
            hp: Some(Hp {
                cur: self.current,
                max: self.max,
            }),
            tags: None,
            was_player: None,
            aggro: None,
            boss: None,
            next_attack_at: None,
            attack_interval: None,
            attacking: None,
            extra,
        }
    }
}

/// M20: предмет, лежащий в локации (выброшенный игроком): виден всем,
/// подбираем любым игроком, исчезает по `drop_ttl_s`.
#[derive(Debug, Clone)]
pub struct GroundItem {
    pub id: String,
    pub location_id: String,
    /// Название (для журнала/осмотра без запроса в БД).
    pub name: String,
    pub def_id: String,
    pub qty: i64,
    /// Уровень заточки на момент выброса — строка журнала «+N».
    pub upgrade: i64,
    /// Сокеты/бонусы на момент выброса (raw-колонки ItemRow) —
    /// восстановление при подборе (R-dnd: камни не теряются).
    pub sockets: serde_json::Value,
    pub bonuses: Option<serde_json::Value>,
    /// ItemSnapshot для отображения на земле «как в рюкзаке» —
    /// `extra["item"]` (заказчик).
    pub display: serde_json::Value,
    /// Кто бросил (char_id) — для журнала.
    pub dropped_by: String,
    /// Epoch-секунда исчезновения.
    pub expires_at: i64,
}

impl GroundItem {
    /// Entity для снапшота/спавна: extra.ground/def_id/qty/upgrade +
    /// `item` — снапшот предмета (единый вид build_location и
    /// `evt/entity/spawn`; раньше spawn не нёс upgrade).
    pub fn entity(&self) -> Entity {
        let mut extra = serde_json::Map::new();
        extra.insert("ground".into(), serde_json::json!(true));
        extra.insert("def_id".into(), serde_json::json!(self.def_id));
        extra.insert("qty".into(), serde_json::json!(self.qty));
        extra.insert("upgrade".into(), serde_json::json!(self.upgrade));
        extra.insert("item".into(), self.display.clone());
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Object,
            name: self.name.clone(),
            hp: None,
            tags: None,
            was_player: None,
            aggro: None,
            boss: None,
            next_attack_at: None,
            attack_interval: None,
            attacking: None,
            extra,
        }
    }

    /// R-dnd: есть ли состояние (заточка/камни/бонусы) — при подборе
    /// восстанавливается отдельной строкой, в «простой» стак не
    /// складывается (иначе камни теряются, заказчик).
    pub fn stateful(&self) -> bool {
        let empty = |v: &serde_json::Value| match v {
            serde_json::Value::Array(a) => a.is_empty(),
            serde_json::Value::Object(o) => o.is_empty(),
            serde_json::Value::Null => true,
            _ => false,
        };
        self.upgrade != 0
            || !empty(&self.sockets)
            || self.bonuses.as_ref().is_some_and(|b| !empty(b))
    }
}

impl LiveCorpse {
    /// Проводной вид §6 (счётчик лута — в `evt/corpse/created.loot_count`).
    /// Снятие шкур: флаги `harvest` (+`skin_done`/`gut_done`) — клиент
    /// по ним решает, показывать ли пункты меню.
    pub fn entity(&self) -> Entity {
        let mut extra = serde_json::Map::new();
        if let Some(h) = &self.harvest {
            let mut h = h.clone();
            h.insert("skin_done".into(), serde_json::json!(self.skin_done));
            h.insert("gut_done".into(), serde_json::json!(self.gut_done));
            extra.insert("harvest".into(), serde_json::Value::Object(h));
        }
        Entity {
            id: self.id.clone(),
            kind: EntityKind::Corpse,
            name: self.name.clone(),
            hp: None,
            tags: None,
            was_player: None,
            aggro: None,
            boss: None,
            next_attack_at: None,
            attack_interval: None,
            attacking: None,
            extra,
        }
    }
}

impl World {
    /// Персонажи в локации (жертвы агрессии).
    /// M24: очистить все слоты трупа (общий лут и «Внутри») →
    /// обновлённый труп для broadcast.
    pub fn corpse_clear(&mut self, corpse_id: &str) -> Option<LiveCorpse> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        for slot in corpse.loot.iter_mut().chain(corpse.inner_loot.iter_mut()) {
            *slot = None;
        }
        Some(corpse.clone())
    }
    /// M24: инъекция в общий лут трупа → обновлённый труп (для
    /// `evt/corpse/created`); `None` — трупа нет.
    pub fn corpse_loot_add(&mut self, corpse_id: &str, def_id: &str) -> Option<LiveCorpse> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        corpse.loot.push(Some(def_id.to_string()));
        Some(corpse.clone())
    }
    /// M24: убрать `count` слотов трупа начиная с `idx` (общий лут
    /// либо «Внутри» при `inner`) → обновлённый труп; `None` —
    /// трупа/слотов нет.
    pub fn corpse_slot_remove_at(
        &mut self,
        corpse_id: &str,
        inner: bool,
        idx: usize,
        count: usize,
    ) -> Option<LiveCorpse> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let slots = if inner {
            &mut corpse.inner_loot
        } else {
            &mut corpse.loot
        };
        if idx >= slots.len() {
            return None;
        }
        let end = (idx + count).min(slots.len());
        for slot in slots.iter_mut().take(end).skip(idx) {
            *slot = None;
        }
        Some(corpse.clone())
    }
    /// Труп по id (для evt/corpse/created после kill_mob).
    pub fn corpse(&self, corpse_id: &str) -> Option<&LiveCorpse> {
        self.corpses.get(corpse_id)
    }
    /// Бросить предмет в локацию (item.drop); id наземного предмета.
    /// R-dnd: состояние предмета (row — raw-колонки) + снапшот для
    /// отображения; состояние уходит на землю только при ПОЛНОМ
    /// выбросе (при частичном оно остаётся у остатка стака).
    #[allow(clippy::too_many_arguments)] // (location, name, row, qty, snap, by, ttl)
    pub fn drop_ground(
        &mut self,
        location_id: &str,
        name: &str,
        row: &crate::db::items::ItemRow,
        qty: i64,
        display: serde_json::Value,
        dropped_by: &str,
        expires_at: i64,
    ) -> String {
        self.ground_seq += 1;
        let id = format!("g{}", self.ground_seq);
        let full = qty >= i64::from(row.qty);
        self.ground_items.insert(
            id.clone(),
            GroundItem {
                id: id.clone(),
                location_id: location_id.to_string(),
                name: name.to_string(),
                def_id: row.def_id.clone(),
                qty,
                upgrade: if full { i64::from(row.upgrade) } else { 0 },
                sockets: if full {
                    row.sockets.clone()
                } else {
                    serde_json::json!([])
                },
                bonuses: if full { row.bonuses.clone() } else { None },
                display,
                dropped_by: dropped_by.to_string(),
                expires_at,
            },
        );
        id
    }
    pub fn ground_item(&self, uid: &str) -> Option<&GroundItem> {
        self.ground_items.get(uid)
    }
    /// Подобрать наземный предмет (удалить из локации).
    pub fn take_ground(&mut self, uid: &str) -> Option<GroundItem> {
        self.ground_items.remove(uid)
    }
    /// M20: просроченные наземные предметы — удалить и вернуть (для
    /// рассылки «{name} исчез»); `ttl <= 0` — не удалять (выключено).
    pub fn due_ground(&mut self, now: i64, ttl_s: i64) -> Vec<GroundItem> {
        if ttl_s <= 0 {
            return Vec::new();
        }
        let due: Vec<String> = self
            .ground_items
            .values()
            .filter(|g| now >= g.expires_at)
            .map(|g| g.id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| self.ground_items.remove(&id))
            .collect()
    }
    /// M20: трупы старше `corpse_ttl_s` — удалить и вернуть (для
    /// рассылки despawn); `ttl <= 0` — не удалять.
    pub fn due_corpses(&mut self, now: i64, ttl_s: i64) -> Vec<LiveCorpse> {
        if ttl_s <= 0 {
            return Vec::new();
        }
        let due: Vec<String> = self
            .corpses
            .values()
            .filter(|c| now >= c.born_at + ttl_s)
            .map(|c| c.id.clone())
            .collect();
        due.into_iter()
            .filter_map(|id| self.corpses.remove(&id))
            .collect()
    }
    pub fn harvest_job(&self, char_id: &str) -> Option<&HarvestJob> {
        self.harvest_jobs.get(char_id)
    }
    /// Запустить разделку (вызывающий проверил все условия).
    pub fn start_harvest(&mut self, job: HarvestJob) {
        self.harvest_jobs.insert(job.char_id.clone(), job);
    }
    /// M24: идёт ли у персонажа активная разделка (блок действий,
    /// `E_BUSY`; item-команды — исключение, прерывают по M23).
    pub fn is_harvesting(&self, char_id: &str) -> bool {
        self.harvest_jobs.contains_key(char_id)
    }
    /// M24: прервать активную разделку (взаимодействие с предметом)
    /// → джоб изымается; для skin вызывающий помечает труп
    /// «шкура испорчена» ([`Self::ruin_skin`]).
    pub fn cancel_harvest(&mut self, char_id: &str) -> Option<HarvestJob> {
        self.harvest_jobs.remove(char_id)
    }
    /// M22: шкура испорчена — повторное снятие с трупа невозможно
    /// (труп мог уже истечь — тогда просто no-op).
    pub fn ruin_skin(&mut self, corpse_id: &str) {
        if let Some(corpse) = self.corpses.get_mut(corpse_id) {
            corpse.skin_done = true;
        }
    }
    /// Джобы, срок которых наступил (извлекаются из очереди).
    pub fn due_harvests(&mut self, now: i64) -> Vec<HarvestJob> {
        let due: Vec<String> = self
            .harvest_jobs
            .iter()
            .filter(|(_, j)| j.at <= now)
            .map(|(c, _)| c.clone())
            .collect();
        due.into_iter()
            .filter_map(|c| self.harvest_jobs.remove(&c))
            .collect()
    }
    /// Завершить разделку: отметить «сделано» и добавить лут —
    /// `skin`: шкура со шансом `hide_chance(level)` иначе обрезки,
    /// **качество = ранг** (`hide_q{rank}`/`hide_scraps_q{rank}`),
    /// крит по рангу → **+2 шкуры** сверх; `gut`: мясо (25 %) либо
    /// потроха → в **отдельный** `inner_loot` («Внутри»).
    /// → `(обновлённый труп, [шкуры], [внутри])`; `None` — труп
    /// исчез или действие уже сделано.
    pub fn finish_harvest(
        &mut self,
        corpse_id: &str,
        action: &str,
        level: u32,
        rank: u32,
    ) -> Option<(LiveCorpse, Vec<String>, Vec<String>)> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let rank = rank.clamp(RANK_MIN, RANK_MAX);
        match action {
            ACTION_SKIN => {
                if corpse.skin_done {
                    return None;
                }
                corpse.skin_done = true;
                let def = if roll_chance(crate::systems::hide_chance(level)) {
                    format!("hide_q{rank}")
                } else {
                    format!("hide_scraps_q{rank}")
                };
                let mut skin = vec![def.clone()];
                corpse.loot.push(Some(def));
                // Крит (шанс по рангу): +2 шкуры текущего качества.
                if roll_chance(crate::systems::harvest_crit(rank)) {
                    for _ in 0..2 {
                        let d = format!("hide_q{rank}");
                        corpse.loot.push(Some(d.clone()));
                        skin.push(d);
                    }
                }
                // M37: редкие трофеи со шкуры (`harvest.skin_extra`):
                // def_id + шанс (например, шкура белого волка — 3 %).
                let extras: Vec<(String, f64)> = corpse
                    .harvest
                    .as_ref()
                    .and_then(|h| h.get("skin_extra"))
                    .and_then(|v| v.as_array())
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| {
                                let def = row.get("def_id")?.as_str()?.to_string();
                                let chance =
                                    row.get("chance").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                Some((def, chance))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for (def, chance) in extras {
                    if roll_chance(chance.clamp(0.0, 1.0)) {
                        corpse.loot.push(Some(def.clone()));
                        skin.push(def);
                    }
                }
                Some((corpse.clone(), skin, Vec::new()))
            }
            ACTION_GUT => {
                if corpse.gut_done {
                    return None;
                }
                corpse.gut_done = true;
                let def = if roll_chance(GUT_MEAT_CHANCE) {
                    ITEM_MEAT
                } else {
                    ITEM_OFFAL
                };
                corpse.inner_loot.push(Some(def.to_string()));
                Some((corpse.clone(), Vec::new(), vec![def.to_string()]))
            }
            _ => None,
        }
    }
    /// M20: удалить старый труп погибшего моба (вместе с лутом — по
    /// возврату респавна); `None` — уже удалён (TTL).
    pub fn remove_corpse(&mut self, corpse_id: &str) -> Option<LiveCorpse> {
        self.corpses.remove(corpse_id)
    }
    pub fn resource(&self, entity_id: &str) -> Option<&ResourceNode> {
        self.resources.get(entity_id)
    }
    /// M38: бросок бонусного самоцвета жилы. `None` — камень не выпал
    /// или таблица пуста; иначе id необработанного камня.
    pub fn roll_resource_gem(&mut self, entity_id: &str) -> Option<String> {
        let table = self.resources.get(entity_id)?.gems.clone()?;
        if table.chance <= 0.0 || self.roll01() >= table.chance {
            return None;
        }
        let group = if self.roll01() < table.mixed_share {
            &table.mixed
        } else {
            &table.pure
        };
        if group.is_empty() {
            return None;
        }
        let index = ((self.roll01() * group.len() as f64) as usize).min(group.len() - 1);
        Some(group[index].clone())
    }

    /// M20/F20b: добыть 1 единицу (валидации инструмента/ранга — на
    /// сервере до вызова): `Some(узел после)`; `None` — нет в этой
    /// локации / скрыт / исчерпан.
    pub fn try_gather(&mut self, entity_id: &str, location_id: &str) -> Option<ResourceNode> {
        let node = self.resources.get_mut(entity_id)?;
        if node.location_id != location_id || node.hidden || node.current <= 0 {
            return None;
        }
        node.current -= 1;
        let exhausted = node.current == 0;
        let (min_s, max_s, despawn) = (node.respawn.0, node.respawn.1, node.despawn_when_empty);
        if exhausted {
            // Респавн до max — по тому же диапазону min..max.
            let at = crate::time::now_epoch() + rand_range(min_s, max_s);
            self.respawn_queue.push(RespawnEntry {
                entity_id: entity_id.to_string(),
                location_id: location_id.to_string(),
                kind: RespawnKind::Resource,
                at,
            });
            if despawn {
                node.hidden = true;
            }
        }
        Some(node.clone())
    }
    /// M20/F20b: восстановить ресурс до max (шаг `respawn_step`);
    /// `(узел, был_скрыт)` — был_скрыт → `evt/entity/spawn`, иначе
    /// патч шкалы через `evt/entity/move`.
    pub fn respawn_resource(&mut self, entity_id: &str) -> Option<(ResourceNode, bool)> {
        let node = self.resources.get_mut(entity_id)?;
        let was_hidden = node.hidden;
        node.current = node.max;
        node.hidden = false;
        Some((node.clone(), was_hidden))
    }
    /// Взять лот-слот из динамического трупа; `None` — уже взят/нет слота.
    pub fn take_dynamic_loot(&mut self, corpse_id: &str, index: usize) -> Option<String> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let slot = corpse.loot.get_mut(index)?;
        slot.take()
    }
}
