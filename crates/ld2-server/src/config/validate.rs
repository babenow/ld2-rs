//! Загрузка и валидация конфига (fail-fast, все проблемы сразу).

use super::*;

impl Config {
    /// Чтение файла → десериализация → валидация (fail-fast).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|err| ConfigError::Io {
            path: path.to_path_buf(),
            err,
        })?;
        let cfg: Config = toml::from_str(&text).map_err(|toml_err| ConfigError::Parse {
            path: path.to_path_buf(),
            toml_err: Box::new(toml_err),
        })?;
        let problems = cfg.problems();
        if !problems.is_empty() {
            return Err(ConfigError::Invalid {
                path: path.to_path_buf(),
                problems,
            });
        }
        Ok(cfg)
    }

    /// Все проблемы конфига сразу (пустой вектор — конфиг валиден).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.check_server(&mut out);
        self.check_database(&mut out);
        self.check_auth(&mut out);
        self.check_gameplay(&mut out);
        self.check_discovery(&mut out);
        self.check_upgrade(&mut out);
        self.check_quests(&mut out);
        self.check_rates(&mut out);
        self.check_shop(&mut out);
        self.check_professions(&mut out);
        self.check_level(&mut out);
        self.check_world(&mut out);
        self.check_skills(&mut out);
        self.check_premium(&mut out);
        self.check_resources(&mut out);
        self.check_combat(&mut out);
        out
    }

    fn check_server(&self, out: &mut Vec<String>) {
        // [server]
        if let Err(msg) = check_host_port(&self.server.bind) {
            out.push(format!("server.bind: {msg}"));
        }
        if self.server.name.trim().is_empty() {
            out.push("server.name: не должно быть пустым".into());
        }
        if self.server.region.trim().is_empty() {
            out.push("server.region: не должно быть пустым".into());
        }
        if tracing_subscriber::EnvFilter::try_new(&self.server.log_level).is_err() {
            out.push(format!(
                "server.log_level: неверная директива {:?} (примеры: \"info\", \"info,sqlx=warn\")",
                self.server.log_level
            ));
        }
    }

    fn check_database(&self, out: &mut Vec<String>) {
        // [database] — значение не логируется и не цитируется в ошибках (D30)
        if !self.database.url.starts_with("postgres://")
            && !self.database.url.starts_with("postgresql://")
        {
            out.push("database.url: ожидается URL, начинающийся с postgres://".into());
        }
        if self.database.max_connections == 0 {
            out.push("database.max_connections: должен быть ≥ 1".into());
        }
    }

    fn check_auth(&self, out: &mut Vec<String>) {
        // [auth]
        if self.auth.access_ttl_s < 60 {
            out.push("auth.access_ttl_s: должен быть ≥ 60".into());
        }
        if self.auth.refresh_ttl_s <= self.auth.access_ttl_s {
            out.push("auth.refresh_ttl_s: должен быть больше auth.access_ttl_s".into());
        }
        if self.auth.min_login_len < 3 {
            out.push("auth.min_login_len: должен быть ≥ 3".into());
        }
        if self.auth.min_password_len < 6 {
            out.push("auth.min_password_len: должен быть ≥ 6".into());
        }
    }

    fn check_gameplay(&self, out: &mut Vec<String>) {
        // [gameplay]
        if self.gameplay.char_slot_limit == 0 {
            out.push("gameplay.char_slot_limit: должен быть ≥ 1".into());
        }
        if self.gameplay.cmd_rate_per_min > 10_000 {
            out.push("gameplay.cmd_rate_per_min: должен быть в 0..=10_000 (0 — выключен)".into());
        }
        check_features(&self.gameplay.features, out);
        check_crime(&self.gameplay.crime, out);
        check_chat(&self.gameplay.chat, out);
        check_inventory(&self.gameplay.inventory, out);
    }

    fn check_discovery(&self, out: &mut Vec<String>) {
        // [discovery] (M19): адрес — WS-URL.
        if let Some(address) = self.discovery.address.as_deref() {
            if !address.is_empty()
                && !address.starts_with("ws://")
                && !address.starts_with("wss://")
            {
                out.push("discovery.address: должен начинаться с ws:// или wss://".into());
            }
        }
    }

    fn check_upgrade(&self, out: &mut Vec<String>) {
        if self.gameplay.upgrade.max_level == 0 {
            out.push("gameplay.upgrade.max_level: должен быть ≥ 1".into());
        }
        if self.gameplay.upgrade.gold_base < 0 || self.gameplay.upgrade.gold_per_level < 0 {
            out.push("gameplay.upgrade.gold_*: не может быть отрицательным".into());
        }
        if !(0.0..=1.0).contains(&self.gameplay.upgrade.chance_base) {
            out.push(format!(
                "gameplay.upgrade.chance_base: должен быть в 0..=1 (получено: {})",
                self.gameplay.upgrade.chance_base
            ));
        }
        if !(0.0..=1.0).contains(&self.gameplay.upgrade.chance_per_level) {
            out.push(format!(
                "gameplay.upgrade.chance_per_level: должен быть в 0..=1 (получено: {})",
                self.gameplay.upgrade.chance_per_level
            ));
        }

        if self.gameplay.upgrade.duration_s <= 0.0 {
            out.push(format!(
                "gameplay.upgrade.duration_s: должен быть > 0 (получено: {})",
                self.gameplay.upgrade.duration_s
            ));
        }

        if self.gameplay.party_max_members < 2 {
            out.push("gameplay.party_max_members: должен быть ≥ 2".into());
        }
    }

    fn check_quests(&self, out: &mut Vec<String>) {
        // [gameplay.quests] (M18/F36)
        if self.gameplay.quests.max_active == 0 {
            out.push("gameplay.quests.max_active: должен быть ≥ 1".into());
        }
    }

    fn check_rates(&self, out: &mut Vec<String>) {
        // [gameplay.rates] (M18/F54): множитель шансов.
        if !(0.0..=10.0).contains(&self.gameplay.rates.chance) {
            out.push(format!(
                "gameplay.rates.chance: должен быть в 0.0..=10.0 (получено: {})",
                self.gameplay.rates.chance
            ));
        }
    }

    fn check_shop(&self, out: &mut Vec<String>) {
        // [gameplay.shop] (M18/F39)
        if self.gameplay.shop.server_markup < 1.0 {
            out.push("gameplay.shop.server_markup: должен быть ≥ 1.0".into());
        }
        if !(0..=10_000).contains(&self.gameplay.shop.max_discount_bp) {
            out.push("gameplay.shop.max_discount_bp: должен быть 0..=10000".into());
        }
        for (skill, per_rank) in &self.gameplay.shop.skill_discounts {
            if *per_rank < 0.0 || *per_rank >= 1.0 {
                out.push(format!(
                    "gameplay.shop.skill_discounts.{skill}: должен быть 0.0..1.0"
                ));
            }
        }
    }

    fn check_professions(&self, out: &mut Vec<String>) {
        // [gameplay.professions] (M12.6; M21 — две шкалы)
        if self.gameplay.professions.xp_per_craft_base < 0
            || self.gameplay.professions.xp_per_craft_per_rank < 0
        {
            out.push("gameplay.professions.xp_per_craft_*: должно быть ≥ 0".into());
        }
        if self.gameplay.professions.level_xp_base < 1 {
            out.push("gameplay.professions.level_xp_base: должен быть ≥ 1".into());
        }
        if self.gameplay.professions.level_xp_growth < 1.0 {
            out.push(format!(
                "gameplay.professions.level_xp_growth: должен быть ≥ 1.0 (получено: {})",
                self.gameplay.professions.level_xp_growth
            ));
        }
        if self.gameplay.professions.rank_xp_mult.is_empty() {
            out.push("gameplay.professions.rank_xp_mult: список не должен быть пустым".into());
        }
        if self
            .gameplay
            .professions
            .rank_xp_mult
            .iter()
            .any(|m| !m.is_finite() || *m < 1.0)
        {
            out.push("gameplay.professions.rank_xp_mult: множители должны быть ≥ 1.0".into());
        }
        if self
            .gameplay
            .professions
            .rank_xp_mult
            .windows(2)
            .any(|w| w[1] < w[0])
        {
            out.push("gameplay.professions.rank_xp_mult: множители не должны убывать".into());
        }
        let p = &self.gameplay.professions;
        if !(0.0..=1.0).contains(&p.mining_chance_base) {
            out.push("gameplay.professions.mining_chance_base: должен быть 0..1".into());
        }
        if !(0.0..=1.0).contains(&p.mining_chance_min) {
            out.push("gameplay.professions.mining_chance_min: должен быть 0..1".into());
        }
        if p.mining_chance_min > p.mining_chance_base {
            out.push("gameplay.professions.mining_chance_min: не больше mining_chance_base".into());
        }
        if p.mining_chance_per_level < 0.0 {
            out.push("gameplay.professions.mining_chance_per_level: должен быть ≥ 0".into());
        }
        if p.mining_master_gem_mult < 1.0 {
            out.push("gameplay.professions.mining_master_gem_mult: должен быть ≥ 1.0".into());
        }
        if !(0.0..=0.02).contains(&p.mining_lamp_gem_bonus) {
            out.push("gameplay.professions.mining_lamp_gem_bonus: должен быть 0..0.02".into());
        }
        if !(0.0..=1.0).contains(&p.mining_lamp_double_ore_chance) {
            out.push("gameplay.professions.mining_lamp_double_ore_chance: должен быть 0..1".into());
        }
        if self.gameplay.professions.max_level == 0 {
            out.push("gameplay.professions.max_level: должен быть ≥ 1".into());
        }
        if self.gameplay.professions.rank_span == 0 {
            out.push("gameplay.professions.rank_span: должен быть ≥ 1".into());
        }
        if self.gameplay.professions.rank_up_gold_base < 0
            || self.gameplay.professions.rank_up_gold_growth < 1.0
        {
            out.push(format!(
                "gameplay.professions.rank_up_gold_*: base ≥ 0, growth ≥ 1.0 (получено: {} / {})",
                self.gameplay.professions.rank_up_gold_base,
                self.gameplay.professions.rank_up_gold_growth
            ));
        }
        if !(0.0..=1.0).contains(&self.gameplay.professions.fail_xp_ratio) {
            out.push(format!(
                "gameplay.professions.fail_xp_ratio: должен быть в 0..=1 (получено: {})",
                self.gameplay.professions.fail_xp_ratio
            ));
        }
    }

    fn check_level(&self, out: &mut Vec<String>) {
        // M15/F24: квадратичная кривая XP = round(A×level²+B×level+C).
        let lv = &self.gameplay.level;
        if !lv.a.is_finite() || lv.a < 0.0 || !lv.b.is_finite() || lv.b < 0.0 {
            out.push("gameplay.level.a/b: должны быть конечными и ≥ 0".into());
        }
        if !lv.c.is_finite() || lv.c < 0.0 {
            out.push("gameplay.level.c: должен быть конечным и ≥ 0".into());
        }
        if lv.a + lv.b + lv.c < 1.0 {
            out.push(format!(
                "gameplay.level: XP(1) = a+b+c должен быть ≥ 1 (получено: {})",
                lv.a + lv.b + lv.c
            ));
        }
        if lv.max_level < 1 {
            out.push("gameplay.level.max_level: должен быть ≥ 1".into());
        }

        if !matches!(self.gameplay.magic.rune_recharge.trim(), "time" | "item") {
            out.push(
                "gameplay.magic.rune_recharge: ожидается \"time\" или \"item\" (M18/F58c)".into(),
            );
        }
        if !self.gameplay.magic.rune_recharge_s.is_finite()
            || self.gameplay.magic.rune_recharge_s <= 0.0
        {
            out.push("gameplay.magic.rune_recharge_s: должен быть > 0".into());
        }
        if self.gameplay.magic.mana.rune_cost == 0 {
            out.push("gameplay.magic.mana.rune_cost: должен быть ≥ 1".into());
        }
        if self.gameplay.magic.mana.regen_in_pct_s < 0.0 {
            out.push(format!(
                "gameplay.magic.mana.regen_in_pct_s: не может быть отрицательным (получено: {})",
                self.gameplay.magic.mana.regen_in_pct_s
            ));
        }
        // M30: формулы максимумов и множитель духовности (character-params.md §1.1).
        if self.gameplay.magic.mana.mp_max_base < 0 {
            out.push("gameplay.magic.mana.mp_max_base: должен быть ≥ 0".into());
        }
        for (key, value) in [
            ("mp_per_int", self.gameplay.magic.mana.mp_per_int),
            ("mp_per_arcana", self.gameplay.magic.mana.mp_per_arcana),
            ("mp_per_spir", self.gameplay.magic.mana.mp_per_spir),
            (
                "spirit_regen_pct",
                self.gameplay.magic.mana.spirit_regen_pct,
            ),
        ] {
            if !value.is_finite() || value < 0.0 {
                out.push(format!(
                    "gameplay.magic.mana.{key}: должен быть ≥ 0 (получено: {value})"
                ));
            }
        }
        // M30: здоровье — формула максимума и реген (character-params.md §2.1/§3.1).
        let health = &self.gameplay.health;
        if health.hp_max_base < 1 {
            out.push("gameplay.health.hp_max_base: должен быть ≥ 1".into());
        }
        for (key, value) in [
            ("hp_per_vit", health.hp_per_vit),
            ("hp_per_level", health.hp_per_level),
            ("regen_in_pct_s", health.regen_in_pct_s),
        ] {
            if !value.is_finite() || value < 0.0 {
                out.push(format!(
                    "gameplay.health.{key}: должен быть ≥ 0 (получено: {value})"
                ));
            }
        }
        if health.regen_out_s == 0 {
            out.push("gameplay.health.regen_out_s: должен быть ≥ 1".into());
        }
    }

    fn check_world(&self, out: &mut Vec<String>) {
        // [world]
        if self.world.content_dir.as_os_str().is_empty() {
            out.push("world.content_dir: не должно быть пустым".into());
        }
        if !(1..=100).contains(&self.world.tick_hz) {
            out.push(format!(
                "world.tick_hz: должен быть в 1..=100 (получено: {})",
                self.world.tick_hz
            ));
        }
        if self.world.autosave_interval_s == 0 {
            out.push("world.autosave_interval_s: должен быть ≥ 1".into());
        }
    }

    fn check_skills(&self, out: &mut Vec<String>) {
        // [skills] (skills.md §8)
        if self.skills.base_xp < 1 {
            out.push("skills.base_xp: должен быть ≥ 1".into());
        }
        if self.skills.growth <= 1.0 {
            out.push(format!(
                "skills.growth: должен быть > 1.0 (получено: {})",
                self.skills.growth
            ));
        }
        if self.skills.rank_max == 0 {
            out.push("skills.rank_max: должен быть ≥ 1".into());
        }
        if self.skills.ranks_per_level == 0 {
            out.push("skills.ranks_per_level: должен быть ≥ 1".into());
        }
        let mut gate_ranks = HashSet::new();
        for (i, gate) in self.skills.gates.iter().enumerate() {
            if gate.rank == 0 || gate.rank > self.skills.rank_max {
                out.push(format!(
                    "skills.gates[{i}].rank: должен быть в 1..={} (получено: {})",
                    self.skills.rank_max, gate.rank
                ));
            }
            if gate.attr < 1 {
                out.push(format!("skills.gates[{i}].attr: должен быть ≥ 1"));
            }
            if !gate_ranks.insert(gate.rank) {
                out.push(format!("skills.gates: дубликат ранга {}", gate.rank));
            }
        }
        if self.skills.kill_xp_base < 0 || self.skills.kill_xp_per_difficulty < 0 {
            out.push("skills.kill_xp_*: не может быть отрицательным".into());
        }
    }

    fn check_premium(&self, out: &mut Vec<String>) {
        // M33: множители премиума — положительные и не меньше 1.
        if self.gameplay.premium.xp_mult < 1.0 {
            out.push("gameplay.premium.xp_mult: должен быть ≥ 1.0".into());
        }
        if self.gameplay.premium.profession_mult < 1.0 {
            out.push("gameplay.premium.profession_mult: должен быть ≥ 1.0".into());
        }
    }

    fn check_resources(&self, out: &mut Vec<String>) {
        // M17/F30e: параметры ресурсов по классам — конечные и положительные.
        {
            let res = &self.gameplay.resource;
            if res.rage_max == 0 || res.energy_max == 0 {
                out.push("gameplay.resource: rage_max/energy_max должны быть > 0".into());
            }
            if !res.rage_gain_pct.is_finite() || !(0.0..=1.0).contains(&res.rage_gain_pct) {
                out.push("gameplay.resource.rage_gain_pct: должен быть в [0, 1]".into());
            }
            if !res.rage_decay_s.is_finite() || res.rage_decay_s <= 0.0 {
                out.push("gameplay.resource.rage_decay_s: должен быть > 0".into());
            }
            if !res.energy_regen_s.is_finite() || res.energy_regen_s <= 0.0 {
                out.push("gameplay.resource.energy_regen_s: должен быть > 0".into());
            }
        }
        for (id, act) in &self.skills.active {
            if !act.cooldown_s.is_finite() || act.cooldown_s <= 0.0 {
                out.push(format!("skills.active.{id}.cooldown_s: должен быть > 0"));
            }
            if act.damage_mult < 1.0 {
                out.push(format!("skills.active.{id}.damage_mult: должен быть ≥ 1.0"));
            }
            if act.req_rank > self.skills.rank_max {
                out.push(format!(
                    "skills.active.{id}.req_rank: больше skills.rank_max ({})",
                    self.skills.rank_max
                ));
            }
        }
    }

    fn check_combat(&self, out: &mut Vec<String>) {
        // [gameplay.combat] (M12.5)
        // M25: минимальная скорость атаки моба — пол интервала.
        if !(self.gameplay.combat.min_attack_interval_s.is_finite()
            && self.gameplay.combat.min_attack_interval_s >= 0.1
            && self.gameplay.combat.min_attack_interval_s <= 60.0)
        {
            out.push(format!(
                "gameplay.combat.min_attack_interval_s: должен быть в 0.1..=60 (получено: {})",
                self.gameplay.combat.min_attack_interval_s
            ));
        }
        if !(self.gameplay.combat.attack_interval_s >= 0.1
            && self.gameplay.combat.attack_interval_s <= 60.0)
        {
            out.push(format!(
                "gameplay.combat.attack_interval_s: должен быть в 0.1..=60 (получено: {})",
                self.gameplay.combat.attack_interval_s
            ));
        }
        if self.gameplay.combat.base_damage < 1 {
            out.push("gameplay.combat.base_damage: должен быть ≥ 1".into());
        }
        if !(0.0..=1.0).contains(&self.gameplay.combat.flee_chance) {
            out.push(format!(
                "gameplay.combat.flee_chance: должен быть в 0..=1 (получено: {})",
                self.gameplay.combat.flee_chance
            ));
        }
    }
}

/// Известные capability-флаги §5: ключ обязан иметь ожидаемый тип.
/// Неизвестные ключи пропускаются — фичи открыты для расширения сервером.
fn check_features(features: &HashMap<String, FeatureValue>, out: &mut Vec<String>) {
    const BOOL_KEYS: &[&str] = &[
        "loot.roll",
        "respawn.inn",
        "guild.bank",
        "trade.value_hint",
        "chat.slash",
    ];
    const STR_KEYS: &[&str] = &["upgrade.rules"];

    for (key, value) in features {
        if BOOL_KEYS.contains(&key.as_str()) && !matches!(value, FeatureValue::Bool(_)) {
            out.push(format!(
                "gameplay.features.{key}: ожидалось булево значение"
            ));
        }
        if STR_KEYS.contains(&key.as_str()) && !matches!(value, FeatureValue::Str(_)) {
            out.push(format!("gameplay.features.{key}: ожидалась строка"));
        }
    }
}

fn check_crime(crime: &CrimeSection, out: &mut Vec<String>) {
    if crime.t_max_s == 0 {
        out.push("gameplay.crime.t_max_s: должен быть > 0".into());
    }
    if !(crime.blend_k > 0.0 && crime.blend_k <= 1.0) {
        out.push(format!(
            "gameplay.crime.blend_k: должен быть в (0; 1] (получено: {})",
            crime.blend_k
        ));
    }
    if crime.base.is_empty() {
        out.push("gameplay.crime.base: не должно быть пустым".into());
    }
    for (kind, secs) in &crime.base {
        if kind.trim().is_empty() {
            out.push("gameplay.crime.base: пустой ключ".into());
        }
        if *secs == 0 {
            out.push(format!("gameplay.crime.base.{kind}: должен быть > 0"));
        }
    }
    if crime.kinds.is_empty() {
        out.push("gameplay.crime.kinds: не должно быть пустым".into());
    }
    let mut seen = HashSet::new();
    for (i, k) in crime.kinds.iter().enumerate() {
        if k.kind.trim().is_empty() {
            out.push(format!(
                "gameplay.crime.kinds[{i}].kind: не должно быть пустым"
            ));
        }
        if k.title.trim().is_empty() {
            out.push(format!(
                "gameplay.crime.kinds[{i}].title: не должно быть пустым"
            ));
        }
        if k.severity.trim().is_empty() {
            out.push(format!(
                "gameplay.crime.kinds[{i}].severity: не должно быть пустым"
            ));
        }
        if !k.kind.trim().is_empty() && !seen.insert(k.kind.as_str()) {
            out.push(format!("gameplay.crime.kinds: дубликат kind {:?}", k.kind));
        }
    }
}

fn check_chat(chat: &ChatSection, out: &mut Vec<String>) {
    if chat.max_len == 0 {
        out.push("gameplay.chat.max_len: должен быть ≥ 1".into());
    }
    if chat.cooldown_s < 0.0 {
        out.push(format!(
            "gameplay.chat.cooldown_s: не может быть отрицательным (получено: {})",
            chat.cooldown_s
        ));
    }
    if chat.channels.is_empty() {
        out.push("gameplay.chat.channels: не должно быть пустым".into());
    }
    let mut seen = HashSet::new();
    for ch in &chat.channels {
        if ch.trim().is_empty() {
            out.push("gameplay.chat.channels: пустой канал".into());
        }
        if !ch.trim().is_empty() && !seen.insert(ch.as_str()) {
            out.push(format!("gameplay.chat.channels: дубликат {ch:?}"));
        }
    }
}

fn check_inventory(inv: &InventorySection, out: &mut Vec<String>) {
    if inv.slots == 0 {
        out.push("gameplay.inventory.slots: должен быть ≥ 1".into());
    }
    if inv.equip_slots.is_empty() {
        out.push("gameplay.inventory.equip_slots: не должно быть пустым".into());
    }
    let mut seen = HashSet::new();
    for slot in &inv.equip_slots {
        if slot.trim().is_empty() {
            out.push("gameplay.inventory.equip_slots: пустой слот".into());
        }
        if !slot.trim().is_empty() && !seen.insert(slot.as_str()) {
            out.push(format!("gameplay.inventory.equip_slots: дубликат {slot:?}"));
        }
    }
}

/// `host:port` с непустым хостом и портом 0–65535; IPv6 — в квадратных скобках.
fn check_host_port(bind: &str) -> Result<(), String> {
    let (host, port) = bind
        .rsplit_once(':')
        .ok_or_else(|| "ожидается host:port".to_string())?;
    if host.is_empty() {
        return Err("хост пуст (ожидается host:port)".into());
    }
    if host.contains(':') && !host.starts_with('[') {
        return Err("IPv6-адрес — в квадратных скобках: [::1]:24817".into());
    }
    port.parse::<u16>()
        .map_err(|_| format!("порт {port:?} не число 0–65535"))?;
    Ok(())
}
