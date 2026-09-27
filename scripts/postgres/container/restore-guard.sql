SET ROLE codex_owner;
SELECT pg_advisory_xact_lock(1414676819, 1);
-- RESTRICT (the default), not CASCADE: any contents make this fail and roll back.
-- pg_restore recreates the schema from the archive in the same transaction.
DROP SCHEMA codex_storage RESTRICT;
