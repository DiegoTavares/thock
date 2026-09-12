package main

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// Each file under migrations/ is applied once, in name order, inside its own
// transaction, and recorded in schema_migrations. Every instance runs this at
// startup; the advisory lock keeps two of them from applying the same file.
const migrationsLock = 7_400_501

func openPool(ctx context.Context, databaseURL string) (*pgxpool.Pool, error) {
	config, err := pgxpool.ParseConfig(databaseURL)
	if err != nil {
		return nil, fmt.Errorf("DATABASE_URL: %w", err)
	}
	// Supabase's pooler runs in transaction mode, where named prepared
	// statements don't survive between queries. Exec mode sends each query
	// with its parameters in one round trip without preparing anything.
	config.ConnConfig.DefaultQueryExecMode = pgx.QueryExecModeExec
	if config.MaxConns < 4 {
		config.MaxConns = 4
	}
	config.MaxConnIdleTime = 5 * time.Minute
	pool, err := pgxpool.NewWithConfig(ctx, config)
	if err != nil {
		return nil, err
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("connecting to postgres: %w", err)
	}
	return pool, nil
}

type migration struct {
	name string
	sql  string
}

func loadMigrations() ([]migration, error) {
	entries, err := fs.ReadDir(migrationFiles, "migrations")
	if err != nil {
		return nil, err
	}
	var migrations []migration
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		raw, err := migrationFiles.ReadFile("migrations/" + entry.Name())
		if err != nil {
			return nil, err
		}
		migrations = append(migrations, migration{name: entry.Name(), sql: string(raw)})
	}
	sort.Slice(migrations, func(i, j int) bool { return migrations[i].name < migrations[j].name })
	if len(migrations) == 0 {
		return nil, errors.New("no migrations are embedded")
	}
	return migrations, nil
}

// migrate applies every pending migration and returns the names it applied.
func migrate(ctx context.Context, pool *pgxpool.Pool) ([]string, error) {
	migrations, err := loadMigrations()
	if err != nil {
		return nil, err
	}
	conn, err := pool.Acquire(ctx)
	if err != nil {
		return nil, err
	}
	defer conn.Release()

	// Session-level lock so it spans the per-migration transactions below;
	// released explicitly and, failing that, when the connection closes.
	if _, err := conn.Exec(ctx, "select pg_advisory_lock($1)", migrationsLock); err != nil {
		return nil, fmt.Errorf("taking the migration lock: %w", err)
	}
	defer func() {
		unlockCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if _, err := conn.Exec(unlockCtx, "select pg_advisory_unlock($1)", migrationsLock); err != nil {
			logf("warning: releasing the migration lock: %v", err)
		}
	}()

	_, err = conn.Exec(ctx, `create table if not exists schema_migrations (
		name text primary key,
		applied_at timestamptz not null default now()
	)`)
	if err != nil {
		return nil, fmt.Errorf("creating schema_migrations: %w", err)
	}
	applied := map[string]bool{}
	rows, err := conn.Query(ctx, "select name from schema_migrations")
	if err != nil {
		return nil, err
	}
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			rows.Close()
			return nil, err
		}
		applied[name] = true
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}

	var newlyApplied []string
	for _, m := range migrations {
		if applied[m.name] {
			continue
		}
		err := pgx.BeginFunc(ctx, conn, func(tx pgx.Tx) error {
			if _, err := tx.Exec(ctx, m.sql); err != nil {
				return err
			}
			_, err := tx.Exec(ctx, "insert into schema_migrations (name) values ($1)", m.name)
			return err
		})
		if err != nil {
			return newlyApplied, fmt.Errorf("migration %s: %w", m.name, err)
		}
		newlyApplied = append(newlyApplied, m.name)
	}
	return newlyApplied, nil
}
