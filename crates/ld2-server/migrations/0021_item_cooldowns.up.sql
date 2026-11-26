-- M17/F30g: кулдаун предмета, с (использование/скорость атаки);
-- дефолт 1.0 — «по умолчанию 1 сек». Источник истины — контент.
ALTER TABLE item_defs ADD COLUMN cooldown_s DOUBLE PRECISION NOT NULL DEFAULT 1.0;
