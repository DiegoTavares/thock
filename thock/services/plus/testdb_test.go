package main

import (
	"context"
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	embeddedpostgres "github.com/fergusstrange/embedded-postgres"
	"github.com/jackc/pgx/v5"
)

// Tests run against a real Postgres: the one named by
// THOCK_PLUS_TEST_DATABASE_URL when set (a role that may create databases),
// otherwise an embedded server started once for the package. Every test gets
// its own freshly migrated database, so the schema and the migrations are
// exercised on every run.
var testDatabase struct {
	once     sync.Once
	adminURL string
	stop     func()
	err      error
	seq      int
	mu       sync.Mutex
}

func TestMain(m *testing.M) {
	code := m.Run()
	if testDatabase.stop != nil {
		testDatabase.stop()
	}
	os.Exit(code)
}

func startTestDatabase() (string, error) {
	testDatabase.once.Do(func() {
		if fromEnv := os.Getenv("THOCK_PLUS_TEST_DATABASE_URL"); fromEnv != "" {
			testDatabase.adminURL = fromEnv
			return
		}
		port, err := freePort()
		if err != nil {
			testDatabase.err = err
			return
		}
		runtimeDir, err := os.MkdirTemp("", "thock-plus-pg-")
		if err != nil {
			testDatabase.err = err
			return
		}
		cache := filepath.Join(os.TempDir(), "thock-plus-embedded-postgres")
		config := embeddedpostgres.DefaultConfig().
			Version(embeddedpostgres.V16).
			Port(port).
			Username("thock").
			Password("thock").
			Database("postgres").
			RuntimePath(runtimeDir).
			CachePath(cache).
			StartTimeout(90 * time.Second).
			Logger(nil)
		server := embeddedpostgres.NewDatabase(config)
		if err := server.Start(); err != nil {
			testDatabase.err = fmt.Errorf("starting embedded postgres: %w", err)
			return
		}
		testDatabase.stop = func() {
			if err := server.Stop(); err != nil {
				fmt.Fprintf(os.Stderr, "stopping embedded postgres: %v\n", err)
			}
			if err := os.RemoveAll(runtimeDir); err != nil {
				fmt.Fprintf(os.Stderr, "removing %s: %v\n", runtimeDir, err)
			}
		}
		testDatabase.adminURL = fmt.Sprintf("postgres://thock:thock@localhost:%d/postgres?sslmode=disable", port)
	})
	return testDatabase.adminURL, testDatabase.err
}

func freePort() (uint32, error) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0, err
	}
	defer listener.Close()
	return uint32(listener.Addr().(*net.TCPAddr).Port), nil
}

// freshDatabaseURL creates an empty database on the test server and returns
// a URL pointing at it. It is dropped when the test ends.
func freshDatabaseURL(t *testing.T) string {
	t.Helper()
	adminURL, err := startTestDatabase()
	if err != nil {
		t.Fatalf("test database: %v", err)
	}
	testDatabase.mu.Lock()
	testDatabase.seq++
	name := fmt.Sprintf("thock_test_%d_%d", os.Getpid(), testDatabase.seq)
	testDatabase.mu.Unlock()

	ctx := context.Background()
	admin, err := pgx.Connect(ctx, adminURL)
	if err != nil {
		t.Fatalf("connecting to the test server: %v", err)
	}
	if _, err := admin.Exec(ctx, "create database "+name); err != nil {
		t.Fatalf("creating %s: %v", name, err)
	}
	if err := admin.Close(ctx); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		admin, err := pgx.Connect(ctx, adminURL)
		if err != nil {
			t.Logf("reconnecting to drop %s: %v", name, err)
			return
		}
		defer admin.Close(ctx)
		if _, err := admin.Exec(ctx, "drop database "+name+" with (force)"); err != nil {
			t.Logf("dropping %s: %v", name, err)
		}
	})

	parsed, err := url.Parse(adminURL)
	if err != nil {
		t.Fatal(err)
	}
	parsed.Path = "/" + name
	return parsed.String()
}
