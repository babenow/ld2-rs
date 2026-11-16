-- M18/F58d (ADR-012 §6): баффы рун — стак с лимитом и таймер снятия.
-- Паттерн crime_statuses: ключ (character_id, kind), переживает рестарт.
CREATE TABLE char_buffs (
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    -- Отображаемое название (сервер — истина, §8); title+stacks — всё,
    -- что нужно UI (param/value/pct — сервер применяет к параметрам).
    title        TEXT NOT NULL,
    param        TEXT NOT NULL,
    value        BIGINT NOT NULL,
    pct          BOOLEAN NOT NULL DEFAULT FALSE,
    stacks       INTEGER NOT NULL DEFAULT 1,
    expires_at   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (character_id, kind)
);

-- Тикер снятия (F58d-2) ходит по онлайн-персонажам.
CREATE INDEX char_buffs_expires_idx ON char_buffs (expires_at);
