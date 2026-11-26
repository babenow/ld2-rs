-- Откат миграции 0011 (гильдии, M12.7d). Данные теряются.
DROP INDEX IF EXISTS guild_log_guild_idx;
DROP TABLE IF EXISTS guild_log;
DROP INDEX IF EXISTS guild_members_character_idx;
DROP TABLE IF EXISTS guild_members;
DROP TABLE IF EXISTS guilds;
