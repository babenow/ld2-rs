DROP INDEX IF EXISTS items_container_slot_uniq;
ALTER TABLE items DROP COLUMN IF EXISTS slot_index;
ALTER TABLE items DROP COLUMN IF EXISTS container;
ALTER TABLE item_defs DROP COLUMN IF EXISTS bag_slots;
