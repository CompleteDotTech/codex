-- Test-only role graph for codex-postgres-runtime's real PostgreSQL test.
-- These roles are deliberately absent from the production fixture roles.
CREATE ROLE codex_bootstrap_graph_bridge NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE codex_bootstrap_graph_principal NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
GRANT codex_owner TO codex_bootstrap_graph_bridge WITH INHERIT FALSE, SET FALSE;
GRANT codex_bootstrap_graph_bridge TO codex_migrator WITH INHERIT FALSE, SET FALSE, ADMIN TRUE;
GRANT codex_runtime TO codex_bootstrap_graph_bridge WITH INHERIT FALSE, SET FALSE, ADMIN TRUE;
