-- M12.7 (PLAN-M12): история чата — кольцевой буфер в БД (`server_history`
-- из [gameplay.chat], D25). Обрезка — на вставке (см. db::chat::push).
CREATE TABLE chat_history (
    id        BIGSERIAL PRIMARY KEY,
    channel   TEXT NOT NULL,
    from_char TEXT NOT NULL,
    name      TEXT NOT NULL,
    text      TEXT NOT NULL,
    sent_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chat_history_channel_idx ON chat_history (channel, id);
