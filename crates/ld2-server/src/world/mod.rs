//! Мир в памяти (D31, M12.5): боевое состояние, мобы/трупы локаций и
//! реестр соединений для серверной рассылки (в т.ч. событий тика боя).
//!
//! Мобы и динамические трупы **эфемерны**: при рестарте воссоздаются из
//! сида (D26) — их hp не переживает перезапуск (отклонение M12.5).
//! Персонажи, инвентарь и лут статических трупов — в БД (D27).

use std::collections::{HashMap, HashSet};
// R15: bounded Sender<String> (канал 256 — см. transport.rs).
type OutSender = tokio::sync::mpsc::Sender<String>;

use ld2_protocol::types::{Entity, EntityKind, Hp};

mod chars;
mod entities;
mod loot;
mod mobs;
mod spawn;

pub use chars::*;
pub use entities::*;
pub use mobs::*;

/// M32: бросок ГПСЧ боевых действий — xorshift64* (детерминируется
/// конфигом `[gameplay.combat] rng_seed` в тестах).
impl World {
    /// Засеять ГПСЧ: `Some(n != 0)` — детерминированно, `None` — из ОС.
    pub fn seed_rng(&mut self, seed: Option<u64>) {
        self.rng_state = seed
            .filter(|s| *s != 0)
            .unwrap_or_else(|| {
                let mut buf = [0u8; 8];
                if getrandom::fill(&mut buf).is_ok() {
                    u64::from_le_bytes(buf).max(1)
                } else {
                    0x9E37_79B9_7F4A_7C15
                }
            })
            .max(1);
    }

    /// Бросок `[0, 1)`; незасеянный мир засевается от часов.
    pub fn roll01(&mut self) -> f64 {
        if self.rng_state == 0 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x5EED);
            self.rng_state = nanos.max(1);
        }
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64) / ((1u64 << 53) as f64)
    }

    /// Бросок `0..100` — шансы в процентах (неточность/парирование).
    pub fn roll_pct(&mut self) -> f64 {
        self.roll01() * 100.0
    }
}

/// M20: случайные секунды в `[min, max]` (респавн-диапазоны;
/// getrandom уже в графе). `max < min` → `min`.
fn rand_range(min: i64, max: i64) -> i64 {
    let min = min.max(0);
    let max = max.max(0);
    if max <= min {
        return min;
    }
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return min;
    }
    let span = (max - min + 1) as u64;
    min + (u64::from_be_bytes(buf) % span) as i64
}

/// Состояние мира, общее для всех соединений.
#[derive(Default)]
pub struct World {
    mobs: HashMap<String, LiveMob>,
    corpses: HashMap<String, LiveCorpse>,
    /// char_id → бой.
    combat: HashMap<String, CombatState>,
    /// char_id → канал текстовых конвертов соединения (рассылка тика §2.4).
    conns: HashMap<String, OutSender>,
    /// char_id → имя (для боевых строк §8).
    char_names: HashMap<String, String>,
    /// R10: char_id → владелец биндинга (peer сессии) — двойная сессия
    /// одного персонажа невозможна, отвязка только своя.
    char_owner: HashMap<String, String>,
    /// M24: «режим бога» — персонажи с неуязвимостью (админ-toggle;
    /// tick::apply_mob_hit не применяет по ним урон вовсе).
    god_chars: HashSet<String>,
    /// M24: надстройки сундуков (entity_id -> слоты ПОСЛЕ базовых
    /// defs; `None` - взято/удалено; индексы стабильны - только
    /// добавление в конец). In-memory до рестарта.
    container_extra: HashMap<String, Vec<Option<(String, i64)>>>,
    /// M24: глобальные «удалено админом» базовые слоты сундуков
    /// ((entity_id, idx); сильнее персонального `loot_taken` - не
    /// возвращается и при респавне).
    container_removed: HashSet<(String, usize)>,
    /// M24: надстройки магазинов NPC (npc_id -> def_id; `None` -
    /// удалено). In-memory до рестарта.
    shop_extra: HashMap<String, Vec<Option<String>>>,
    /// M24: удалённые админом контентные товары: (npc_id, таб, idx).
    shop_removed: HashSet<(String, usize, usize)>,
    /// char_id → текущая локация (мишени агрессивных мобов).
    char_loc: HashMap<String, String>,
    /// (char_id, skill_id) → до какого момента кулдаун (epoch-с, §4/M14).
    ability_cd: HashMap<(String, String), i64>,
    /// M17/F30f: чем бьёт персонаж (экипированное оружие) — синхронный
    /// доступ из plan_combat; обновляется при входе и смене оружия.
    char_hits: HashMap<String, &'static str>,
    /// M17/F30g: период атаки персонажа, мс — из `cooldown_s`
    /// оружия в правой руке; `None` → конфиг `combat.attack_interval_s`.
    char_attack: HashMap<String, i64>,
    /// M18/F58d (ADR-012 §5): руны экипированного оружия — авто-приёмы
    /// в `plan_combat` (как `char_hits`: вход/экипировка/инкрустация).
    char_runes: HashMap<String, Vec<SocketedRune>>,
    /// M32: боевые параметры персонажа (урон/точность/защита) — кэш
    /// для синхронного тика (вход/экипировка/обучение).
    char_combat: HashMap<String, CharCombat>,
    /// M32: ГПСЧ боевых бросков (xorshift64*); 0 — не засеян.
    rng_state: u64,
    corpse_seq: u64,
    /// M20: предметы, лежащие в локациях (выбросы) + TTL-удаление.
    ground_items: HashMap<String, GroundItem>,
    ground_seq: u64,
    /// M20: очередь возрождений (мобы и статический лут) — тёплое
    /// время мира, при рестарте сбрасывается (дизайн M20).
    respawn_queue: Vec<RespawnEntry>,
    /// M20: шаблоны мобов из сида — источник данных для возврата
    /// (живой моб удаляется при смерти).
    mob_seed: HashMap<String, LiveMob>,
    /// M20: респавн-диапазоны сущностей контента (для лута —
    /// сброс `loot:<entity>:%` по расписанию).
    entity_respawn: HashMap<String, (i64, i64)>,
    /// M20/F20b: объекты-ресурсы локаций (`current` — состояние мира).
    resources: HashMap<String, ResourceNode>,
    /// Снятие шкур: активные разделки (ключ — персонаж, одна за раз;
    /// сервер-джоб на5 с).
    harvest_jobs: HashMap<String, HarvestJob>,
    /// M39: активный сбор ресурса (ключ — персонаж, один за раз).
    gather_jobs: HashMap<String, GatherJob>,
    /// M25: дебаффы контроля персонажей (эффекты приёмов босса):
    /// char_id → (kind → until, epoch мс).
    char_effects: HashMap<String, HashMap<String, i64>>,
    /// M37: кровотечение персонажей от приёмов мобов (ДПС + срок).
    char_bleed: HashMap<String, CharBleed>,
    /// M37: счётчик призванных мобов — уникальные id (`m_3_summon_N`).
    summon_seq: u64,
}

impl World {
    // ── M24: надстройки сундуков и магазинов (in-memory) ──────────────────

    // ─── M20: предметы в локациях (выбросы) и TTL трупов ────────────

    // ─── Снятие шкур: сервер-джобы разделки (5 с) ─────────────────────────

    // ─── M20: очередь возрождений ──────────────────────────────────────────

    // ─── M20/F20b: объекты-ресурсы ────────────────────────────────────────
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R-dnd: GroundItem хранит состояние (камни/заточка/бонусы) и
    /// снапшот для отображения; entity() отдаёт всё в extra.
    #[test]
    fn ground_item_state_and_entity_extra() {
        let g = GroundItem {
            id: "g1".into(),
            location_id: "l_village".into(),
            name: "Посох подмастерья".into(),
            def_id: "staff_apprentice".into(),
            qty: 1,
            upgrade: 2,
            sockets: serde_json::json!([{ "index": 0, "rune": null }]),
            bonuses: None,
            display: serde_json::json!({ "name": "Посох подмастерья", "rarity": "rare" }),
            dropped_by: "c1".into(),
            expires_at: 0,
        };
        assert!(g.stateful(), "заточка + сокет — состояние");
        let ent = g.entity();
        assert_eq!(ent.extra["upgrade"], 2);
        assert_eq!(ent.extra["item"]["name"], "Посох подмастерья");
        assert_eq!(ent.extra["qty"], 1);
        // Простой стак (деньги/зелья) — без состояния.
        let plain = GroundItem {
            upgrade: 0,
            sockets: serde_json::json!([]),
            bonuses: None,
            ..g.clone()
        };
        assert!(!plain.stateful(), "простой стак без состояния");
        // Бонусы инстанса — тоже состояние.
        let bonused = GroundItem {
            upgrade: 0,
            sockets: serde_json::json!([]),
            bonuses: Some(serde_json::json!([{ "kind": "attr", "id": "str", "value": 5 }])),
            ..g.clone()
        };
        assert!(bonused.stateful(), "бонусы — состояние");
    }

    /// M24: «режим бога» — по-персонажный toggle неуязвимости
    /// (tick::apply_mob_hit смотрит `is_god` до применения урона).
    #[test]
    fn god_mode_toggles_per_char() {
        let mut world = World::default();
        assert!(!world.is_god("c1"), "по умолчанию — смертный");
        world.set_god("c1", true);
        assert!(world.is_god("c1"), "включён");
        assert!(!world.is_god("c2"), "не глобально");
        world.set_god("c1", false);
        assert!(!world.is_god("c1"), "выключен");
    }

    /// M20/F20a: `rand_range` — всегда в диапазоне [min, max]
    /// (респавн-таймеры задаются двумя числами).
    #[test]
    fn rand_range_stays_within_bounds() {
        for _ in 0..200 {
            let v = rand_range(3, 7);
            assert!((3..=7).contains(&v), "вне диапазона: {v}");
        }
        assert_eq!(rand_range(5, 5), 5, "равные границы");
        assert_eq!(rand_range(7, 3), 7, "max < min → min");
        assert_eq!(rand_range(-5, -1), 0, "отрицательные → 0");
    }

    /// M20/F20a: очередь возрождений — моб возвращается из шаблона
    /// сида со стартовым HP (старый труп удаляется), лут-сброс
    /// планируется один раз (дедуп повторных взятий).
    #[test]
    fn respawn_schedules_and_restores_mob() {
        let mut world = World::default();
        let row = (
            "l_test".to_string(),
            serde_json::json!([
                {"id": "m_r", "kind": "monster", "name": "Крыса",
                 "hp": {"cur": 6, "max": 6}, "difficulty": 1,
                 "respawn": {"min_s": 5, "max_s": 10}},
                {"id": "e_chest", "kind": "object", "name": "Сундук",
                 "loot": ["gold_coin"],
                 "respawn": {"min_s": 2, "max_s": 4}}
            ]),
        );
        world.seed_mobs(&[row], None);
        let base = crate::time::now_epoch();

        // Лут: взяли → одна запись в очереди (повторные — дедуп).
        world.schedule_loot_respawn("e_chest");
        world.schedule_loot_respawn("e_chest");
        // Моб убит → запись с трупом; до срока очередь пуста.
        let corpse = world.kill_mob("m_r").expect("труп создаётся");
        assert!(world.mob("m_r").is_none(), "моб мёртв");
        assert!(
            world.due_respawns(base + 1).is_empty(),
            "до минимального срока (2 с) — пусто"
        );

        // Срок прошёл: обе записи, время в диапазонах контента.
        let due = world.due_respawns(base + 60);
        assert_eq!(due.len(), 2, "лут и моб по сроку: {due:?}");
        let loot = due
            .iter()
            .find(|e| matches!(e.kind, RespawnKind::Loot))
            .expect("лут-запись");
        assert_eq!(loot.entity_id, "e_chest");
        let mob_e = due
            .iter()
            .find(|e| matches!(e.kind, RespawnKind::Mob { .. }))
            .expect("моб-запись");
        assert_eq!(mob_e.entity_id, "m_r");
        assert_eq!(mob_e.location_id, "l_test");
        assert!((base..=base + 10).contains(&mob_e.at), "at в [5,10]");
        assert!(
            matches!(
            &mob_e.kind,
            RespawnKind::Mob {
                corpse_id: Some(c)
            } if c == &corpse
            ),
            "запись несёт труп: {:?}",
            mob_e.kind
        );

        // Моб: снова в локации, стартовый HP; труп удалён; очередь пуста.
        let entity = world.respawn_mob("m_r").expect("возврат из шаблона");
        assert_eq!(entity.id, "m_r");
        assert_eq!(entity.hp.map(|h| h.cur), Some(6));
        assert!(world.mob("m_r").is_some());
        assert!(world.remove_corpse(&corpse).is_some(), "старый труп ушёл");
        assert!(world.remove_corpse(&corpse).is_none(), "уже удалён");
        assert!(
            world.due_respawns(base + 600).is_empty(),
            "очередь пуста (дедуп лута)"
        );
    }

    /// Сущности локации из снапшота (`build_location` мутирует вход).
    fn loc_entities(world: &World, loc_id: &str) -> Vec<ld2_protocol::types::Entity> {
        world
            .build_location(ld2_protocol::types::LocationSnapshot {
                id: loc_id.to_string(),
                name: loc_id.to_string(),
                flags: Vec::new(),
                text: String::new(),
                exits: Vec::new(),
                entities: Vec::new(),
                extra: Default::default(),
            })
            .entities
    }

    /// Присутствие (M29): `build_location` отдаёт онлайн-персонажей
    /// только своей локации, без HP (мир его не хранит) и без дублей.
    #[test]
    fn build_location_lists_online_players() {
        let mut world = World::default();
        let (tx_a, _rx_a) = tokio::sync::mpsc::channel(4);
        let (tx_b, _rx_b) = tokio::sync::mpsc::channel(4);
        let (tx_c, _rx_c) = tokio::sync::mpsc::channel(4);
        assert!(world.bind_char("c_a", "Аня", tx_a, "p1"));
        assert!(world.bind_char("c_b", "Боря", tx_b, "p2"));
        assert!(world.bind_char("c_c", "Вера", tx_c, "p3"));
        world.set_char_location("c_a", "l_village");
        world.set_char_location("c_b", "l_village");
        world.set_char_location("c_c", "l_forest");

        let entities = loc_entities(&world, "l_village");
        let players: Vec<&ld2_protocol::types::Entity> = entities
            .iter()
            .filter(|e| e.kind == ld2_protocol::types::EntityKind::Player)
            .collect();
        assert_eq!(players.len(), 2, "двое в деревне: {players:?}");
        assert!(players.iter().any(|e| e.id == "c_a" && e.name == "Аня"));
        assert!(players.iter().any(|e| e.id == "c_b" && e.name == "Боря"));
        assert!(
            !players.iter().any(|e| e.id == "c_c"),
            "игрок другой локации не виден"
        );
        assert!(
            players.iter().all(|e| e.hp.is_none()),
            "чужой HP не передаём"
        );

        // Отвязка (выход из мира) убирает строку из снапшота.
        assert!(world.unbind_char("c_a", "p1"));
        let after = loc_entities(&world, "l_village");
        assert!(
            !after
                .iter()
                .any(|e| e.kind == ld2_protocol::types::EntityKind::Player && e.id == "c_a"),
            "отвязанный игрок исчез"
        );
    }

    /// M39: «Кирка мастера» ×2 и «Лампа шахтёра» +2 п.п. к шансу
    /// камня; итог всегда 0..1.
    #[test]
    fn gem_chance_with_pickaxe_and_lamp_clamps() {
        assert!((World::gem_chance_with(0.0125, 1.0, 0.0) - 0.0125).abs() < 1e-9);
        assert!((World::gem_chance_with(0.0125, 2.0, 0.0) - 0.025).abs() < 1e-9);
        assert!((World::gem_chance_with(0.0125, 1.0, 0.02) - 0.0325).abs() < 1e-9);
        assert!((World::gem_chance_with(0.0125, 2.0, 0.02) - 0.045).abs() < 1e-9);
        assert_eq!(World::gem_chance_with(0.6, 2.0, 0.02), 1.0, "clamp 1");
    }

    /// M20/F20b: ресурс убывает по 1 за добычу; при 0 — респавн
    /// min..max; трава (despawn_when_empty) скрывается, жила остаётся
    /// видимой и исчерпанной до возобновления.
    #[test]
    fn resource_gather_exhausts_and_respawns() {
        let mut world = World::default();
        let row = (
            "l_t".to_string(),
            serde_json::json!([
                {"id": "r_vein", "kind": "resource", "name": "Жила",
                 "resource": "ore_copper", "max": 2,
                 "respawn": {"min_s": 5, "max_s": 10},
                 "despawn_when_empty": false,
                 "profession": "mining", "min_rank": 1, "tool": "pickaxe"},
                {"id": "r_herb", "kind": "resource", "name": "Трава",
                 "resource": "herb", "max": 1,
                 "respawn": {"min_s": 30, "max_s": 60},
                 "despawn_when_empty": true,
                 "profession": "herbalist", "min_rank": 1, "tool": "sickle"}
            ]),
        );
        world.seed_mobs(&[row], None);

        // Стартовый запас = max, параметры из контента, в снапшоте есть.
        let vein = world.resource("r_vein").expect("жида");
        assert_eq!((vein.current, vein.max), (2, 2));
        assert_eq!(vein.tool.as_deref(), Some("pickaxe"));
        assert!(loc_entities(&world, "l_t")
            .iter()
            .any(|e| e.id == "r_vein" && e.kind == EntityKind::Resource));

        // Добыча: −1 за действие; чужая локация — отказ.
        assert!(
            world.try_gather("r_vein", "l_x").is_none(),
            "не в своей локации"
        );
        assert_eq!(world.try_gather("r_vein", "l_t").unwrap().current, 1);
        // Исчерпание: жила остаётся видимой, в очереди респавн min..max.
        let after = world.try_gather("r_vein", "l_t").unwrap();
        assert_eq!(after.current, 0);
        assert!(!after.hidden, "жида остаётся на месте");
        let base = crate::time::now_epoch();
        assert!(world.due_respawns(base + 4).is_empty(), "до min (5 с)");
        let due = world.due_respawns(base + 60);
        assert!(
            due.iter().any(|e| e.entity_id == "r_vein"
                && matches!(e.kind, RespawnKind::Resource)
                && (base..=base + 10).contains(&e.at)),
            "респавн в [5,10]: {due:?}"
        );
        let (node, was_hidden) = world.respawn_resource("r_vein").unwrap();
        assert!(!was_hidden, "жида не скрывалась");
        assert_eq!(node.current, 2, "восстановление до max");

        // Трава: исчерпание скрывает до респавна (despawn_when_empty).
        let after = world.try_gather("r_herb", "l_t").unwrap();
        assert_eq!((after.current, after.hidden), (0, true));
        assert!(
            !loc_entities(&world, "l_t").iter().any(|e| e.id == "r_herb"),
            "исчерпанная трава скрыта"
        );
        assert!(
            world
                .due_respawns(base + 60)
                .iter()
                .any(|e| e.entity_id == "r_herb"),
            "респавн травы в очереди"
        );
        let (node, was_hidden) = world.respawn_resource("r_herb").unwrap();
        assert!(was_hidden, "была скрыта");
        assert_eq!((node.current, node.hidden), (1, false), "вернулась");
    }

    /// M20: дефолт `[world]` применяется только к мобам без своего
    /// `respawn`; сущности без respawn и дефолта не возрождаются.
    #[test]
    fn respawn_default_fills_only_unconfigured_mobs() {
        let mut world = World::default();
        let row = (
            "l_t".to_string(),
            serde_json::json!([
                {"id": "m_a", "kind": "monster", "name": "A",
                 "hp": {"cur": 3, "max": 3}},
                {"id": "m_b", "kind": "monster", "name": "B",
                 "hp": {"cur": 3, "max": 3},
                 "respawn": {"min_s": 40, "max_s": 50}},
                {"id": "e_c", "kind": "object", "name": "C"}
            ]),
        );
        world.seed_mobs(&[row], Some((60, 120)));
        assert_eq!(world.entity_respawn.get("m_a"), Some(&(60, 120)));
        assert_eq!(world.entity_respawn.get("m_b"), Some(&(40, 50)));
        assert!(
            !world.entity_respawn.contains_key("e_c"),
            "дефолт не для не-мобов"
        );
    }

    /// M18/F58d: дебаффы рун — стан блокирует атаки до срока, bleed
    /// тикает по расписанию, корни хранятся (движение — позже).
    #[test]
    fn rune_debuffs_stun_bleed_root_lifecycle() {
        let mut world = World::default();
        let row = (
            "l_forest".to_string(),
            serde_json::json!([{
                "id": "m_w", "kind": "monster", "name": "Волк",
                "hp": { "cur": 18, "max": 18 },
                "difficulty": 3, "aggro": true, "damage": 6
            }]),
        );
        world.seed_mobs(&[row], None);
        let now = 1_000_000_i64;

        // Стан: активен ровно duration, потом снимается.
        assert!(!world.mob_stunned("m_w", now), "до эффекта не стан");
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "stun".to_string(),
                value: 0,
                duration_s: 3,
                period_s: 1,
            },
            "c1",
            now,
        );
        assert!(world.mob_stunned("m_w", now + 2_999), "стан активен");
        assert!(!world.mob_stunned("m_w", now + 3_001), "стан истёк");

        // Кровотечение: первый тик через секунду, dps из контента,
        // источник сохранён для XP.
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "bleed".to_string(),
                value: 3,
                duration_s: 6,
                period_s: 1,
            },
            "c1",
            now,
        );
        assert_eq!(world.bleeding_mobs(now), Vec::new(), "тиков ещё не было");
        let due = world.bleeding_mobs(now + 1_000);
        assert_eq!(due.len(), 1, "пора тикнуть");
        assert_eq!(due[0].1, 3, "dps из эффекта");
        assert_eq!(due[0].3.as_deref(), Some("c1"), "источник для XP");
        world.advance_bleed_tick("m_w", now + 1_000);
        assert_eq!(
            world.bleeding_mobs(now + 1_500),
            Vec::new(),
            "следующий тик только через секунду"
        );

        // Срок истёк — снимаем (bleed_dps обнуляется).
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "bleed".to_string(),
                value: 3,
                duration_s: 2,
                period_s: 1,
            },
            "c1",
            now,
        );
        world.advance_bleed_tick("m_w", now + 5_000);
        let mobs = world.bleeding_mobs(now + 6_000);
        assert!(mobs.is_empty(), "истёкший DoT не тикает: {mobs:?}");

        // Регрессия: частые пере-наложения (шанс 1.0) продлевают срок,
        // но НЕ сбрасывают тик — иначе DoT никогда не дотикает.
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "bleed".to_string(),
                value: 3,
                duration_s: 6,
                period_s: 1,
            },
            "c1",
            now,
        );
        let next_before = world.bleeding_mobs(now + 1_000);
        assert_eq!(next_before.len(), 1, "тикер назначен");
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "bleed".to_string(),
                value: 3,
                duration_s: 6,
                period_s: 1,
            },
            "c1",
            now + 500,
        );
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "bleed".to_string(),
                value: 3,
                duration_s: 6,
                period_s: 1,
            },
            "c1",
            now + 900,
        );
        let due = world.bleeding_mobs(now + 1_000);
        assert_eq!(due.len(), 1, "тик не сброшен пере-наложением");
        world.advance_bleed_tick("m_w", now + 1_000);

        // Корни: поле хранится (перемещение мобов появится позже) и не
        // путается со станом — стан к этому моменту уже истёк.
        world.apply_rune_effect(
            "m_w",
            &RuneEffect {
                kind: "root".to_string(),
                value: 0,
                duration_s: 5,
                period_s: 1,
            },
            "c1",
            now,
        );
        assert!(
            !world.mob_stunned("m_w", now + 10_000),
            "root не блокирует атаки (≠ стан)"
        );
    }

    /// M17/F30g: период атаки — из cooldown_s оружия; снятие оружия
    /// убирает значение (в plan берётся конфиг).
    #[test]
    fn char_attack_ms_set_get_and_clear() {
        let mut world = World::default();
        assert_eq!(world.char_attack_ms("c1"), None, "до входа — конфиг");
        world.set_char_attack_ms("c1", Some(1_500));
        assert_eq!(world.char_attack_ms("c1"), Some(1_500));
        world.set_char_attack_ms("c1", None);
        assert_eq!(world.char_attack_ms("c1"), None, "оружие снято");
    }

    /// M37: кровотечение персонажа — тик раз в секунду и срок.
    #[test]
    fn char_bleed_ticks_and_expires() {
        let mut world = World::default();
        let now = 10_000;
        assert!(world.bleeding_chars(now).is_empty(), "без кровотечения");
        world.set_char_bleed("c1", 2, now + 2_500, now);
        assert!(
            world.bleeding_chars(now).is_empty(),
            "первый тик — через 1 с"
        );
        assert_eq!(
            world.bleeding_chars(now + 1_000),
            vec![("c1".to_string(), 2)],
            "тик кровотечения"
        );
        world.advance_char_bleed("c1", now + 1_000);
        assert!(
            world.bleeding_chars(now + 1_500).is_empty(),
            "не чаще раза в секунду"
        );
        assert_eq!(world.bleeding_chars(now + 2_000).len(), 1, "второй тик");
        world.advance_char_bleed("c1", now + 2_000);
        assert!(world.bleeding_chars(now + 3_000).is_empty(), "срок истёк");
    }

    /// M37: призыв — клоны шаблона с уникальными id, без респавна.
    #[test]
    fn summon_mobs_clones_template_with_unique_ids() {
        let mut world = World::default();
        let row = (
            "l_test".to_string(),
            serde_json::json!([
                {"id": "m_w", "kind": "monster", "name": "Волк",
                 "hp": {"cur": 18, "max": 18}, "aggro": true,
                 "difficulty": 3, "respawn": {"min_s": 60, "max_s": 120}}
            ]),
        );
        world.seed_mobs(&[row], None);
        let spawned = world.summon_mobs("m_w", 2, "l_test", 1_000);
        assert_eq!(spawned.len(), 2, "два волка");
        assert_ne!(spawned[0].id, spawned[1].id, "уникальные id");
        for mob in &spawned {
            assert_eq!(mob.location_id, "l_test");
            assert_eq!((mob.hp, mob.hp_max), (18, 18), "полное HP");
            assert!(mob.respawn.is_none(), "призванные не респавнятся");
            assert!(world.mob(&mob.id).is_some(), "в мире");
        }
    }

    /// M39: множитель/прибавка повышают шанс шкуры и трофея.
    #[test]
    fn hide_chance_with_tools_clamps() {
        use crate::systems::hide_chance_with;
        assert!((hide_chance_with(0.03, 2.0, 0.0) - 0.06).abs() < 1e-9);
        assert!((hide_chance_with(0.03, 1.0, 0.02) - 0.05).abs() < 1e-9);
        assert!((hide_chance_with(0.03, 2.0, 0.02) - 0.08).abs() < 1e-9);
        assert_eq!(hide_chance_with(0.9, 2.0, 0.02), 1.0);
    }

    /// M39: множитель ножа гарантированно поднимает шкуру и трофей.
    #[test]
    fn harvest_tool_multiplier_boosts_hide_and_extra() {
        let mut world = World::default();
        let row = (
            "l_test".to_string(),
            serde_json::json!([
                {"id": "m_bonus", "kind": "monster", "name": "Белый волк",
                 "hp": {"cur": 10, "max": 10},
                 "harvest": {"level": 45, "skin": true, "skin_extra": [
                     {"def_id": "pelt_white_wolf", "chance": 0.5}
                 ]}}
            ]),
        );
        world.seed_mobs(&[row], None);
        let corpse_id = world.kill_mob("m_bonus").expect("труп");
        let (_corpse, skin, _) = world
            .finish_harvest(&corpse_id, "skin", 20, 3, 3.0, 0.0)
            .expect("шкура");
        assert!(
            skin.contains(&"hide_q3".to_string()),
            "усиленный шанс шкуры: {skin:?}"
        );
        assert!(
            skin.contains(&"pelt_white_wolf".to_string()),
            "усиленный шанс трофея: {skin:?}"
        );
    }

    /// M37: редкий трофей со снятия шкуры (`harvest.skin_extra`).
    #[test]
    fn skin_extra_drops_configured_loot() {
        let mut world = World::default();
        let row = (
            "l_test".to_string(),
            serde_json::json!([
                {"id": "m_pelt", "kind": "monster", "name": "Белый волк",
                 "hp": {"cur": 10, "max": 10},
                 "harvest": {"level": 45, "skin": true, "skin_extra": [
                     {"def_id": "pelt_white_wolf", "chance": 1.0},
                     {"def_id": "pelt_never", "chance": 0.0}
                 ]}},
                {"id": "m_no_pelt", "kind": "monster", "name": "Волк",
                 "hp": {"cur": 10, "max": 10},
                 "harvest": {"level": 45, "skin": true, "skin_extra": [
                     {"def_id": "pelt_white_wolf", "chance": 0.0}
                 ]}}
            ]),
        );
        world.seed_mobs(&[row], None);

        let corpse_id = world.kill_mob("m_pelt").expect("труп");
        let (corpse, skin, _) = world
            .finish_harvest(&corpse_id, "skin", 20, 3, 1.0, 0.0)
            .expect("шкура");
        assert!(
            skin.contains(&"pelt_white_wolf".to_string()),
            "трофей в шкурах: {skin:?}"
        );
        assert!(
            corpse.loot.iter().flatten().any(|d| d == "pelt_white_wolf"),
            "трофей в луте трупа"
        );
        assert!(
            !skin.contains(&"pelt_never".to_string()),
            "шанс 0 не выпадает"
        );

        let corpse_id = world.kill_mob("m_no_pelt").expect("труп");
        let (corpse, skin, _) = world
            .finish_harvest(&corpse_id, "skin", 20, 3, 1.0, 0.0)
            .expect("шкура");
        assert!(
            !corpse.loot.iter().flatten().any(|d| d == "pelt_white_wolf")
                && !skin.contains(&"pelt_white_wolf".to_string()),
            "без шанса трофея нет"
        );
    }
}
