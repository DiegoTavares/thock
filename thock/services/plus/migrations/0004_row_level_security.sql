-- Supabase serves every table in the public schema through its REST API to
-- anyone holding the project's anon key. Only this service talks to the
-- database, as the tables' owner, so row-level security with no policies
-- closes that API while the owner keeps full access. Every table added later
-- must enable it too (TestEveryTableHasRowLevelSecurity checks).

do $$
declare
    table_name text;
    role_name text;
begin
    for table_name in
        select tablename from pg_tables where schemaname = 'public'
    loop
        execute format('alter table public.%I enable row level security', table_name);
    end loop;

    -- Supabase's API roles; absent on a plain Postgres. Supabase's default
    -- privileges grant them every new table, view and function, so those are
    -- revoked too: a view runs as its owner and would skip row-level security.
    foreach role_name in array array['anon', 'authenticated'] loop
        if exists (select from pg_roles where rolname = role_name) then
            execute format('revoke all on all tables in schema public from %I', role_name);
            execute format('revoke all on all sequences in schema public from %I', role_name);
            execute format('revoke all on all functions in schema public from %I', role_name);
            execute format('alter default privileges in schema public revoke all on tables from %I', role_name);
            execute format('alter default privileges in schema public revoke all on sequences from %I', role_name);
            execute format('alter default privileges in schema public revoke all on functions from %I', role_name);
        end if;
    end loop;
end
$$;
