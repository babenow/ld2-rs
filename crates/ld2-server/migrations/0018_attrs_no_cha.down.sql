-- Откат: cha возвращается в дефолт и в строки, где её нет.
ALTER TABLE characters
    ALTER COLUMN attrs SET DEFAULT '{"str":10,"dex":10,"vit":10,"int":10,"cha":10}';
UPDATE characters SET attrs = attrs || '{"cha":10}'::jsonb
 WHERE NOT (attrs ? 'cha');
