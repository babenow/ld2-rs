-- M12.2 (PLAN-M12, D29/D30): аккаунты и refresh-токены (ADR-006).
-- Пароли — argon2id PHC-строка; токены в БД — только sha256-хэш (D30).

CREATE TABLE accounts (
    id            BIGSERIAL PRIMARY KEY,
    login         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at TIMESTAMPTZ,
    -- Будущий бан через SQL/админ-инструмент (вне M12); login при бане
    -- отвечает E_BAD_CREDENTIALS — факт бана не раскрывается (D30).
    banned_until  TIMESTAMPTZ
);

CREATE TABLE refresh_tokens (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX refresh_tokens_account_idx ON refresh_tokens (account_id);
