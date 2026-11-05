-- M13.1 (PLAN-M13, D33/ADR-007): email аккаунта — обязателен на регистрации,
-- нормализуется приложением (trim+lowercase), UNIQUE только для заполненных
-- (DEFAULT '' — существующие dev-строки переживают миграцию).
ALTER TABLE accounts
    ADD COLUMN email TEXT NOT NULL DEFAULT '';

CREATE UNIQUE INDEX accounts_email_idx ON accounts (email) WHERE email <> '';
