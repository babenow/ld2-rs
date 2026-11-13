-- M18/F48: «чем бьёт» из контента оружия (вместо вывода по навыку).
-- hits_as — творительный для строки лога («мечем», «секирой»,
-- «из лука»); NULL → фолбэк weapon_hits_as(skill, item_type).
ALTER TABLE item_defs ADD COLUMN hits_as TEXT;
