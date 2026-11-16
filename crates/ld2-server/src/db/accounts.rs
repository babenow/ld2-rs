//! Репозиторий аккаунтов и refresh-токенов (миграция 0002, D29/D30, ADR-006).

use sqlx::PgPool;

/// Строка `accounts` для входа (запросы уже отфильтровывают баны, D30).
#[derive(Debug, sqlx::FromRow)]
pub struct AccountRow {
    pub id: i64,
    pub password_hash: String,
}

/// Новый аккаунт; `None` — логин **или email** занят (UNIQUE →
/// `E_LOGIN_TAKEN`/`E_EMAIL_TAKEN`; вызывающий заранее проверяет
/// `email_exists`, чтобы различить причины). `email` — уже нормализованный
/// (D33, ADR-007). `password_hash` — argon2id PHC-строка (D30): хэширует
/// вызывающий слой, репозиторий только хранит.
pub async fn create_account(
    pool: &PgPool,
    login: &str,
    password_hash: &str,
    email: &str,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO accounts (login, password_hash, email) VALUES ($1, $2, $3) \
         ON CONFLICT (login) DO NOTHING RETURNING id",
    )
    .bind(login)
    .bind(password_hash)
    .bind(email)
    .fetch_optional(pool)
    .await
}

/// M14/F19: роль аккаунта (модерация чата); неизвестный id → ошибка
/// вызывающего (в finish_login падение маскируется под `player`).
pub async fn role(pool: &PgPool, account_id: i64) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT role FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
}

/// M18/F39: скидка аккаунта в магазинах (bp; премиум — ручной SQL,
/// 1000 = −10%). 0 — без скидки.
pub async fn shop_discount_bp(pool: &PgPool, account_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT shop_discount_bp FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
}

/// Занят ли email (дешёвая проверка до argon2, D33).
pub async fn email_exists(pool: &PgPool, email: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM accounts WHERE email = $1)")
        .bind(email)
        .fetch_one(pool)
        .await
}

/// Занят ли логин — **включая** баны (забаненный логин не переиспользуется).
/// Дешёвая проверка до argon2 (защита от DoS при регистрации).
pub async fn login_exists(pool: &PgPool, login: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM accounts WHERE login = $1)")
        .bind(login)
        .fetch_one(pool)
        .await
}

/// Аккаунт для входа. Забаненные не находятся — login отвечает как неверный
/// пароль (`E_BAD_CREDENTIALS`): факт бана не раскрывается (D30).
pub async fn find_for_login(pool: &PgPool, login: &str) -> Result<Option<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
        "SELECT id, password_hash FROM accounts \
         WHERE login = $1 AND (banned_until IS NULL OR banned_until < now())",
    )
    .bind(login)
    .fetch_optional(pool)
    .await
}

/// Действующий refresh-токен → `(account_id, login)` (отозван/истёк/бан →
/// `None` → `E_AUTH_EXPIRED`, §4.5).
pub async fn account_by_refresh(
    pool: &PgPool,
    refresh_hash: &str,
) -> Result<Option<(i64, String)>, sqlx::Error> {
    sqlx::query_as::<_, (i64, String)>(
        "SELECT a.id, a.login FROM refresh_tokens r \
         JOIN accounts a ON a.id = r.account_id \
         WHERE r.token_hash = $1 AND r.revoked_at IS NULL AND r.expires_at > now() \
           AND (a.banned_until IS NULL OR a.banned_until < now())",
    )
    .bind(refresh_hash)
    .fetch_optional(pool)
    .await
}

/// Выдать refresh-токен (TTL — из конфига, D29); хэш — в колонку, сам токен
/// клиенту (D30: в БД только sha256).
pub async fn issue_refresh(
    pool: &PgPool,
    account_id: i64,
    refresh_hash: &str,
    expires_epoch_s: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO refresh_tokens (account_id, token_hash, expires_at) \
         VALUES ($1, $2, to_timestamp($3))",
    )
    .bind(account_id)
    .bind(refresh_hash)
    .bind(expires_epoch_s)
    .execute(pool)
    .await?;
    Ok(())
}

/// Отзыв **всех** refresh аккаунта (`auth.logout` §4.5).
pub async fn revoke_refreshes(pool: &PgPool, account_id: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now() \
         WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Фиксация входа (best-effort: ошибка не ломает логин).
pub async fn touch_login(pool: &PgPool, account_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE accounts SET last_login_at = now() WHERE id = $1")
        .bind(account_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Последний выбранный персонаж (восстановление сессии при login/resume:
/// клиент шлёт `state.resync` и ждёт снапшот, §3/§4.5).
pub async fn last_char(pool: &PgPool, account_id: i64) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT last_char_id FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_optional(pool)
        .await
}

/// Запомнить выбранный персонажа (best-effort, на `char.select`).
pub async fn set_last_char(
    pool: &PgPool,
    account_id: i64,
    char_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE accounts SET last_char_id = $2 WHERE id = $1")
        .bind(account_id)
        .bind(char_id)
        .execute(pool)
        .await?;
    Ok(())
}
