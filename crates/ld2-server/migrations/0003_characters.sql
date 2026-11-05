-- M12.2 (PLAN-M12): персонажи аккаунтов (ФР-02, §5/§3).
-- state_version — монотонная версия состояния персонажа (§3): растёт на
-- char.select, переживает reconnect. Удаление — soft (deleted_at).

CREATE TABLE characters (
    id            TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    account_id    BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    profile       TEXT NOT NULL,
    location_id   TEXT NOT NULL DEFAULT '',
    hp_cur        INTEGER NOT NULL DEFAULT 100,
    hp_max        INTEGER NOT NULL DEFAULT 100,
    mp_cur        INTEGER NOT NULL DEFAULT 60,
    mp_max        INTEGER NOT NULL DEFAULT 60,
    xp_pool       BIGINT NOT NULL DEFAULT 0,
    state_version BIGINT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at    TIMESTAMPTZ
);

-- Имя уникально среди живых персонажей (после char.delete имя
-- освобождается): обычный UNIQUE не годится — NULL = NULL не TRUE.
CREATE UNIQUE INDEX characters_name_alive_idx ON characters (name)
    WHERE deleted_at IS NULL;

CREATE INDEX characters_account_idx ON characters (account_id)
    WHERE deleted_at IS NULL;
