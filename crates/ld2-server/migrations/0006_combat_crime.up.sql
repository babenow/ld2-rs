-- M12.5 (PLAN-M12): бой/магия/преступность.
-- Сокеты посохов (ADR-002), навык оружия для XP убийств (skills.md §2.3),
-- статусы преступности и журнал нарушений (§10 — таймеры переживают
-- рестарт в БД).

ALTER TABLE items ADD COLUMN sockets JSONB NOT NULL DEFAULT '[]';

-- Навык, который качается убийством этим предметом (контент: "skill").
ALTER TABLE item_defs ADD COLUMN skill TEXT;

-- Активные статусы (kind → expires_at): kinds сосуществуют независимо (§10).
CREATE TABLE crime_statuses (
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    expires_at   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (character_id, kind)
);

-- Журнал нарушений (§7 crime.log → {entries[]}).
CREATE TABLE crime_log (
    id           BIGSERIAL PRIMARY KEY,
    character_id TEXT NOT NULL REFERENCES characters (id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    at           TIMESTAMPTZ NOT NULL,
    delta_s      BIGINT NOT NULL,
    total_s      BIGINT NOT NULL
);

CREATE INDEX crime_log_character_idx ON crime_log (character_id, id);
