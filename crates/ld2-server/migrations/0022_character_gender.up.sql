-- M18/F34: пол персонажа — задаётся при создании, нужен диалогам NPC
-- (F35: NPC «знает» имя и пол). Существующие персонажи — `male`.
ALTER TABLE characters
    ADD COLUMN gender TEXT NOT NULL DEFAULT 'male'
    CHECK (gender IN ('male', 'female'));
