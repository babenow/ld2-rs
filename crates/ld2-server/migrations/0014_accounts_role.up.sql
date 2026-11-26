-- M14/F19 (PLAN-M14): роль аккаунта — модерация чата (system-канал
-- и удаление сообщений). Назначение — SQL: UPDATE accounts SET role=…
ALTER TABLE accounts
    ADD COLUMN role TEXT NOT NULL DEFAULT 'player'
    CONSTRAINT accounts_role_valid
    CHECK (role IN ('player', 'admin', 'moderator'));
