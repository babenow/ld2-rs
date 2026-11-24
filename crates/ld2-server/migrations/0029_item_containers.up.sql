-- M28: контейнеры предметов (система сумок, WoW-стиль).
-- container: 'backpack' (базовый рюкзак), 'bags' (слоты под сумки 0..3)
-- или uid надетой сумки (её содержимое).
ALTER TABLE item_defs ADD COLUMN bag_slots INTEGER;

ALTER TABLE items ADD COLUMN container TEXT NOT NULL DEFAULT 'backpack';
ALTER TABLE items ADD COLUMN slot_index INTEGER;

-- В контейнере не может быть двух предметов на одной позиции; строки
-- без позиции (старые данные) не индексируются — их разложит миграция.
CREATE UNIQUE INDEX items_container_slot_uniq
    ON items (character_id, container, slot_index)
    WHERE character_id IS NOT NULL AND slot_index IS NOT NULL;
