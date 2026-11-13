-- M18/F35: кулдауны тайминг-строк диалогов NPC (`every_s`): строка
-- показывается персонажу один раз за период; shown_at — epoch-секунды
-- сервера (сверяется в Rust с now_epoch, чистая функция видимости).
CREATE TABLE character_dialog_lines (
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    npc_id       TEXT NOT NULL,
    line_id      TEXT NOT NULL,
    shown_at     BIGINT NOT NULL,
    PRIMARY KEY (character_id, npc_id, line_id)
);
