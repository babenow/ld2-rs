-- M28: сумка может ограничивать типы предметов (колчан, сумка травника…).
-- NULL/пусто — любые типы.
ALTER TABLE item_defs ADD COLUMN bag_types TEXT[];
