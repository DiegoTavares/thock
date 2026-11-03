-- Supabase serves every table in the public schema through its REST API to
-- anyone holding the project's anon key. Only this service talks to the
-- database, as the tables' owner, so row-level security with no policies
-- closes that API while the owner keeps full access. Every table added later
-- must enable it too (TestEveryTableHasRowLevelSecurity checks).

do $$
declare
    table_name text;
begin
    for table_name in
        select tablename from pg_tables where schemaname = 'public'
    loop
        execute format('alter table public.%I enable row level security', table_name);
    end loop;

    -- Supabase's API roles; absent on a plain Postgres.
    if exists (select from pg_roles where rolname = 'anon') then
        revoke all on all tables in schema public from anon;
        revoke all on all sequences in schema public from anon;
    end if;
    if exists (select from pg_roles where rolname = 'authenticated') then
        revoke all on all tables in schema public from authenticated;
        revoke all on all sequences in schema public from authenticated;
    end if;
end
$$;
