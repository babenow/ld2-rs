-- M12.6 (PLAN-M12): профессии, рецепты, квесты, крафт-джобы.
-- Контент (professions/recipes/quests.json) сидится в таблицы по D26;
-- прогресс персонажей и джобы — персистентны (рестарт не теряет материалы).

-- ── контент ────────────────────────────────────────────────────────────────
CREATE TABLE professions (
    id        TEXT PRIMARY KEY,
    name      TEXT NOT NULL,
    -- primary занимает слот (profession_limit), secondary — нет (skills.md §5.1).
    prof_type TEXT NOT NULL DEFAULT 'primary'
);

CREATE TABLE recipes (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    profession    TEXT NOT NULL,
    rank          INTEGER NOT NULL DEFAULT 1,
    -- Станок в локации (NULL — без станка) и инструмент (item_type).
    station       TEXT,
    tool_type     TEXT,
    requires      JSONB NOT NULL DEFAULT '{}',
    inputs        JSONB NOT NULL DEFAULT '[]',
    time_s        INTEGER NOT NULL DEFAULT 1,
    xp_profession INTEGER NOT NULL DEFAULT 0,
    output_type   TEXT NOT NULL,
    output_qty    INTEGER NOT NULL DEFAULT 1,
    station_label TEXT
);

CREATE TABLE quests (
    id           TEXT PRIMARY KEY,
    title        TEXT NOT NULL,
    giver        TEXT,
    -- Цели: [{type, target, need, text}] (kill/collect …).
    goals        JSONB NOT NULL DEFAULT '[]',
    -- Награда: {xp, items:[{type, qty}]}.
    reward       JSONB NOT NULL DEFAULT '{}',
    reward_label TEXT
);

-- ── прогресс персонажей ────────────────────────────────────────────────────
CREATE TABLE character_professions (
    character_id  TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    profession_id TEXT NOT NULL REFERENCES professions (id) ON DELETE CASCADE,
    xp            BIGINT NOT NULL DEFAULT 0,
    rank          INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (character_id, profession_id)
);

CREATE TABLE character_quests (
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    quest_id     TEXT NOT NULL REFERENCES quests (id) ON DELETE CASCADE,
    -- available (ещё не взят) не хранится; active | turn_in | done.
    state        TEXT NOT NULL DEFAULT 'active',
    -- Прогресс целей: [{cur, need}] индексами в goals.
    progress     JSONB NOT NULL DEFAULT '[]',
    PRIMARY KEY (character_id, quest_id)
);

-- ── крафт-джобы (рестарт не теряет материалы — D27) ────────────────────────
-- Ингредиенты списываются при старте; джоб хранит, что выдать по готовности.
CREATE TABLE craft_jobs (
    id           BIGSERIAL PRIMARY KEY,
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    recipe_id    TEXT NOT NULL REFERENCES recipes (id) ON DELETE CASCADE,
    qty          INTEGER NOT NULL DEFAULT 1,
    -- Что выдать по готовности (output_type × output_qty × qty).
    output_type  TEXT NOT NULL,
    output_qty   INTEGER NOT NULL DEFAULT 1,
    finishes_at  TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX craft_jobs_ready_idx ON craft_jobs (finishes_at);
CREATE INDEX craft_jobs_character_idx ON craft_jobs (character_id);
