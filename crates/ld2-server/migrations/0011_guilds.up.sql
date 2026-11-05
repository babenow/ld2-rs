-- M12.7d (PLAN-M12): гильдии (ФР-16) — write-through в БД + журнал.
-- Банк гильдии в v1 не реализуется (место зарезервировано, ФР-16).

CREATE TABLE guilds (
    id         TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    name       TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE guild_members (
    guild_id   TEXT NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    -- leader | officer | member (ранг меняется атомарно, §7).
    rank       TEXT NOT NULL DEFAULT 'member',
    joined_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, character_id)
);

-- Уникальность: персонаж состоит максимум в одной гильдии.
CREATE UNIQUE INDEX guild_members_character_idx ON guild_members (character_id);

CREATE TABLE guild_log (
    id        BIGSERIAL PRIMARY KEY,
    guild_id  TEXT NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    kind      TEXT NOT NULL,
    text      TEXT NOT NULL,
    at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX guild_log_guild_idx ON guild_log (guild_id, id);
