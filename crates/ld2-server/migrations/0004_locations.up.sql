-- M12.3 (PLAN-M12, D26): статический мир — локации из content/ (сид),
-- плюс последний выбранный персонаж аккаунта (восстановление сессии при
-- resume: клиент шлёт state.resync и ждёт снапшот, §3/§4.5).

CREATE TABLE locations (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    flags    TEXT[] NOT NULL DEFAULT '{}',
    text     TEXT NOT NULL DEFAULT '',
    -- exits: [{id, label, dir?, to}] (с целевой локацией — для навигации);
    -- entities: статические определения §6 (игроки/трупы добавит M12.5+).
    exits    JSONB NOT NULL DEFAULT '[]',
    entities JSONB NOT NULL DEFAULT '[]'
);

ALTER TABLE accounts ADD COLUMN last_char_id TEXT;
