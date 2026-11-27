//! Сид мобов, вход в локацию и респавны (D26/M20).

use super::*;

impl World {
    /// Загрузить живых мобов из сида (вызывается после content-сида).
    /// `default_respawn` — дефолтный диапазон `[world]` для мобов
    /// без `respawn` в контенте (M20, решение 5).
    pub fn seed_mobs(
        &mut self,
        rows: &[(String, serde_json::Value)],
        default_respawn: Option<(i64, i64)>,
    ) {
        for (location_id, entities) in rows {
            let entities: Vec<Entity> =
                serde_json::from_value(entities.clone()).unwrap_or_default();
            for e in entities {
                let extra = &e.extra;
                // M20: `respawn {min_s,max_s}` у любой сущности
                // (мобам — дефолт [world]); отрицательные → 0,
                // min/max приводятся к порядку.
                let mut respawn = extra
                    .get("respawn")
                    .and_then(|v| v.as_object())
                    .and_then(|o| {
                        let min = o.get("min_s")?.as_i64()?.max(0);
                        let max = o.get("max_s")?.as_i64()?.max(0);
                        Some(if min <= max { (min, max) } else { (max, min) })
                    });
                if respawn.is_none() && e.kind == EntityKind::Monster {
                    respawn = default_respawn;
                }
                if let Some(range) = respawn {
                    self.entity_respawn.insert(e.id.clone(), range);
                }
                // M20/F20b: объект-ресурс — параметры из extra.
                if e.kind == EntityKind::Resource {
                    if let Some(def_id) = extra.get("resource").and_then(|v| v.as_str()) {
                        let max = extra
                            .get("max")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(1)
                            .max(1);
                        self.resources.insert(
                            e.id.clone(),
                            ResourceNode {
                                id: e.id.clone(),
                                location_id: location_id.clone(),
                                name: e.name.clone(),
                                def_id: def_id.to_string(),
                                max,
                                current: max,
                                respawn: respawn.unwrap_or((60, 120)),
                                despawn_when_empty: extra
                                    .get("despawn_when_empty")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false),
                                hidden: false,
                                profession: extra
                                    .get("profession")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                min_rank: extra
                                    .get("min_rank")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(1)
                                    .clamp(1, 30) as i32,
                                tool: extra.get("tool").and_then(|v| v.as_str()).map(String::from),
                            },
                        );
                    }
                    continue;
                }
                if e.kind != EntityKind::Monster {
                    continue;
                }
                let difficulty = extra
                    .get("difficulty")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    .clamp(1, 10);
                // M20: drops — строки (шанс1.0) или объекты
                // {def_id|id, chance 0..1} (таблица лута).
                let drops: Vec<DropEntry> = extra
                    .get("drops")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| {
                                if let Some(def_id) = v.as_str() {
                                    return Some(DropEntry {
                                        def_id: def_id.to_string(),
                                        chance: 1.0,
                                    });
                                }
                                let obj = v.as_object()?;
                                let def_id = obj
                                    .get("def_id")
                                    .or_else(|| obj.get("id"))
                                    .and_then(|x| x.as_str())?;
                                let chance = obj
                                    .get("chance")
                                    .and_then(|x| x.as_f64())
                                    .unwrap_or(1.0)
                                    .clamp(0.0, 1.0);
                                Some(DropEntry {
                                    def_id: def_id.to_string(),
                                    chance,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let damage = extra.get("damage").and_then(|v| v.as_i64()).unwrap_or(4);
                // M32: боевые статы — объект `combat` или плоские поля;
                // дефолты от сложности: старый контент играет без правок.
                let combat = extra.get("combat").and_then(|v| v.as_object());
                let stat = |key: &str, default: i64| {
                    combat
                        .and_then(|c| c.get(key))
                        .or_else(|| extra.get(key))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(default)
                };
                let armor = stat("armor", difficulty * 2).max(0);
                let dodge = stat("dodge", 3 + difficulty).clamp(0, 60);
                let parry = stat("parry", difficulty).clamp(0, 60);
                let resist = stat("resist", difficulty * 2).clamp(0, 80);
                let accuracy = stat("accuracy", 75 + difficulty * 2).clamp(1, 99);
                let hp_start = e.hp.map(|hp| hp.cur).unwrap_or(1);
                let mob = LiveMob {
                    id: e.id.clone(),
                    location_id: location_id.clone(),
                    name: e.name.clone(),
                    hp: hp_start,
                    hp_max: e.hp.map(|hp| hp.max).unwrap_or(1),
                    difficulty,
                    drops,
                    tags: e.tags.clone().unwrap_or_default(),
                    aggro: e.aggro.unwrap_or(false),
                    damage,
                    armor,
                    dodge,
                    parry,
                    resist,
                    accuracy,
                    next_attack_at: 0,
                    stun_until: 0,
                    root_until: 0,
                    bleed_dps: 0,
                    bleed_until: 0,
                    bleed_next_at: 0,
                    bleed_src: None,
                    burn_dps: 0,
                    burn_until: 0,
                    burn_next_at: 0,
                    burn_period: 0,
                    burn_src: None,
                    respawn,
                    hp_start,
                    harvest: extra.get("harvest").and_then(|v| v.as_object()).cloned(),
                    bonus_loot: Vec::new(),
                    // M25: босс — конфиг фаз/навыков из контента
                    // (`boss_ai`: ключ `boss` занят display-полем Entity).
                    boss: extra
                        .get("boss_ai")
                        .and_then(|v| serde_json::from_value::<BossConfig>(v.clone()).ok())
                        .map(BossRuntime::new),
                    attack_interval: 0,
                    attack_speed: extra
                        .get("attack_speed")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0),
                    target: None,
                };
                // M20: шаблон сида — источник данных при респавне.
                self.mob_seed.insert(e.id.clone(), mob.clone());
                self.mobs.insert(e.id.clone(), mob);
            }
        }
    }
    /// M37: призыв мобов приёмом босса: клоны шаблона сида с
    /// уникальными id, без респавна и без наследования босса.
    pub fn summon_mobs(
        &mut self,
        template_id: &str,
        count: i64,
        location_id: &str,
        now_ms: i64,
    ) -> Vec<LiveMob> {
        let Some(template) = self.mob_seed.get(template_id).cloned() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for _ in 0..count.clamp(1, 8) {
            self.summon_seq += 1;
            let mut mob = template.clone();
            mob.id = format!("{template_id}_summon_{}", self.summon_seq);
            mob.location_id = location_id.to_string();
            mob.hp = mob.hp_start;
            mob.hp_max = template.hp_max;
            mob.next_attack_at = now_ms + 500;
            mob.boss = None;
            mob.respawn = None;
            mob.target = None;
            self.mobs.insert(mob.id.clone(), mob.clone());
            out.push(mob);
        }
        out
    }

    /// Мёртвые мобы текущей локации уже не отдаются; снапшот §6 = статика
    /// (кроме мобов — они живут здесь) + живые мобы + динамические трупы.
    pub fn build_location(
        &self,
        mut snapshot: ld2_protocol::types::LocationSnapshot,
    ) -> ld2_protocol::types::LocationSnapshot {
        use ld2_protocol::types::EntityKind::*;
        // Мобы и ресурсы живут в мире (content-строка — только сид):
        // снимок не должен дублировать их и показывать убитых/скрытых.
        snapshot
            .entities
            .retain(|e| !matches!(e.kind, Monster | Resource));
        let loc_id = snapshot.id.clone();
        let mut mobs: Vec<&LiveMob> = self
            .mobs
            .values()
            .filter(|m| m.location_id == loc_id)
            .collect();
        mobs.sort_by(|a, b| a.id.cmp(&b.id));
        for mob in mobs {
            snapshot.entities.push(mob.entity());
        }
        let mut corpses: Vec<&LiveCorpse> = self
            .corpses
            .values()
            .filter(|c| c.location_id == loc_id)
            .collect();
        corpses.sort_by(|a, b| a.id.cmp(&b.id));
        for corpse in corpses {
            snapshot.entities.push(corpse.entity());
        }
        // Присутствие игроков: онлайн-персонажи локации — строки
        // `kind: player` (§6). Клиент рисует их в блоке «Игроки»;
        // свой персонаж отфильтровывается в `Session::location_with_world`.
        let mut players: Vec<(&String, &String)> = self
            .char_loc
            .iter()
            .filter(|(id, loc)| loc.as_str() == loc_id && self.conns.contains_key(*id))
            .filter_map(|(id, _)| self.char_names.get(id).map(|name| (id, name)))
            .collect();
        players.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
        for (id, name) in players {
            if snapshot.entities.iter().any(|e| e.id == **id) {
                continue;
            }
            snapshot.entities.push(Entity {
                id: id.clone(),
                kind: EntityKind::Player,
                name: name.clone(),
                hp: None,
                tags: None,
                was_player: None,
                aggro: None,
                boss: None,
                attacking: None,
                next_attack_at: None,
                attack_interval: None,
                extra: Default::default(),
            });
        }
        // M20: предметы, лежащие в локации (выбросы) — как объекты с
        // маркером ground (подбор через `cmd/item.use` по id).
        let mut ground: Vec<&GroundItem> = self
            .ground_items
            .values()
            .filter(|g| g.location_id == loc_id)
            .collect();
        ground.sort_by(|a, b| a.id.cmp(&b.id));
        for item in ground {
            // R-dnd: единый вид снапшота/спавна (extra.item — снапшот
            // предмета для отображения «как в рюкзаке»).
            snapshot.entities.push(item.entity());
        }
        // M20/F20b: ресурсы (скрытые — до респавна не показываются).
        let mut nodes: Vec<&ResourceNode> = self
            .resources
            .values()
            .filter(|r| r.location_id == loc_id && !r.hidden)
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        for node in nodes {
            snapshot.entities.push(node.entity());
        }
        snapshot
    }
    /// Взять слот лута трупа (`loot:` — общий список, `inner:` —
    /// «Внутри»); `None` — уже взят/нет слота.
    pub fn take_dynamic_slot(
        &mut self,
        corpse_id: &str,
        index: usize,
        inner: bool,
    ) -> Option<String> {
        let corpse = self.corpses.get_mut(corpse_id)?;
        let slot = if inner {
            corpse.inner_loot.get_mut(index)?
        } else {
            corpse.loot.get_mut(index)?
        };
        slot.take()
    }
    /// M20: статический лут сущности взят → возродить через её
    /// `respawn {min_s,max_s}` (нет поля — однократно); повторные
    /// взятия не плодят дубли записи в очереди.
    pub fn schedule_loot_respawn(&mut self, entity_id: &str) {
        let Some((min_s, max_s)) = self.entity_respawn.get(entity_id).copied() else {
            return;
        };
        let pending = self
            .respawn_queue
            .iter()
            .any(|e| e.entity_id == entity_id && matches!(e.kind, RespawnKind::Loot));
        if pending {
            return;
        }
        let at = crate::time::now_epoch() + rand_range(min_s, max_s);
        let location_id = String::new(); // лут-сбросу локация не нужна
        self.respawn_queue.push(RespawnEntry {
            entity_id: entity_id.to_string(),
            location_id,
            kind: RespawnKind::Loot,
            at,
        });
    }
    /// M20: возрождения, срок которых наступил (по возрастанию `at`);
    /// записи извлекаются — применение (возврат моба/сброс лута) за
    /// вызывающим (тик-шаг `respawn_step`).
    pub fn due_respawns(&mut self, now: i64) -> Vec<RespawnEntry> {
        let (mut due, keep): (Vec<_>, Vec<_>) =
            self.respawn_queue.drain(..).partition(|e| now >= e.at);
        self.respawn_queue = keep;
        due.sort_by_key(|e| e.at);
        due
    }
    /// M20: вернуть моба из шаблона сида (стартовый HP) — проводной
    /// вид для `evt/entity/spawn`; `None` — нет шаблона (не должен
    /// случиться: шаблон живёт с сида).
    pub fn respawn_mob(&mut self, entity_id: &str) -> Option<Entity> {
        let mut mob = self.mob_seed.get(entity_id)?.clone();
        mob.hp = mob.hp_start;
        let entity = mob.entity();
        self.mobs.insert(entity_id.to_string(), mob);
        Some(entity)
    }
}
