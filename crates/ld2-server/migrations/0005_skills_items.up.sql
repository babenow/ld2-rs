-- M12.4 (PLAN-M12, skills.md §2/§8): навыки с XP-путём, предметы
-- (инстансы + определения из контента), базовые атрибуты (§1: старт 10)
-- и журнал взятого статического лута (однократное взятие).

-- Базовые атрибуты (skills.md §1: str/dex/vit/int/cha = 10 на старте);
-- рост — квестами (вне M12.4). Гейты рангов (§2.2) читают этот столбец.
ALTER TABLE characters
    ADD COLUMN attrs JSONB NOT NULL
    DEFAULT '{"str":10,"dex":10,"vit":10,"int":10,"cha":10}';

-- Навык персонажа: только вложенные строки (снапшот отдаёт все навыки
-- контента, у невложенных — нули).
CREATE TABLE character_skills (
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    skill_id     TEXT NOT NULL,
    xp           BIGINT NOT NULL DEFAULT 0,
    rank         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (character_id, skill_id)
);

-- Определения предметов из content/items.json (сид, D26).
CREATE TABLE item_defs (
    id         TEXT PRIMARY KEY,
    item_type  TEXT NOT NULL,
    name       TEXT NOT NULL,
    rarity     TEXT NOT NULL,
    upgrade    INTEGER NOT NULL DEFAULT 0,
    stats      JSONB NOT NULL DEFAULT '{}',
    requires   JSONB,
    gold_value INTEGER NOT NULL DEFAULT 0,
    qty        INTEGER NOT NULL DEFAULT 1
);

-- Определения навыков из content/skills.json (сид, D26).
CREATE TABLE skills (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    skill_group TEXT NOT NULL
);

-- Инстансы предметов: инвентарь/экипировка персонажа (character_id NULL —
-- будущие наземные луты). uid генерируется БД.
CREATE TABLE items (
    uid TEXT PRIMARY KEY DEFAULT ('i_' || gen_random_uuid()::text),
    character_id TEXT REFERENCES characters (id) ON DELETE CASCADE,
    def_id TEXT NOT NULL REFERENCES item_defs (id),
    qty INTEGER NOT NULL DEFAULT 1,
    upgrade INTEGER NOT NULL DEFAULT 0,
    equipped_slot TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX items_character_idx ON items (character_id);

-- Взятый статический лут (uid вида loot:<entity>:<idx>) — однократно
-- НА ПЕРСОНАЖА: каждый собирает статический лок один раз (и тесты не
-- конфликтуют на одном трупе).
CREATE TABLE loot_taken (
    uid         TEXT NOT NULL,
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    taken_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (uid, character_id)
);
