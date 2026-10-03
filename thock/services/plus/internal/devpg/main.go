// Command devpg hands out a throwaway Postgres database for local runs and
// the integration script (thock/script/integration).
//
// With THOCK_PLUS_TEST_DATABASE_URL set (a role that may create databases,
// as CI's service container provides) it creates a fresh database there;
// otherwise it starts an embedded server. Either way it writes the database
// URL to the file named by its only argument, then holds until SIGINT or
// SIGTERM and removes what it made.
package main

import (
	"context"
	"fmt"
	"log"
	"net"
	"net/url"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	embeddedpostgres "github.com/fergusstrange/embedded-postgres"
	"github.com/jackc/pgx/v5"
)

func main() {
	if len(os.Args) != 2 {
		log.Fatal("usage: devpg <file to write the database URL to>")
	}
	databaseURL, cleanup, err := provision()
	if err != nil {
		log.Fatal(err)
	}
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, syscall.SIGINT, syscall.SIGTERM)
	// Written last and atomically: the script treats the file's existence as
	// "ready".
	urlFile := os.Args[1]
	if err := os.WriteFile(urlFile+".tmp", []byte(databaseURL), 0o600); err != nil {
		cleanup()
		log.Fatal(err)
	}
	if err := os.Rename(urlFile+".tmp", urlFile); err != nil {
		cleanup()
		log.Fatal(err)
	}
	<-stop
	cleanup()
}

func provision() (string, func(), error) {
	if serverURL := os.Getenv("THOCK_PLUS_TEST_DATABASE_URL"); serverURL != "" {
		return freshDatabase(serverURL)
	}
	return embedded()
}

func freshDatabase(serverURL string) (string, func(), error) {
	ctx := context.Background()
	name := fmt.Sprintf("thock_integration_%d", os.Getpid())
	admin, err := pgx.Connect(ctx, serverURL)
	if err != nil {
		return "", nil, fmt.Errorf("connecting to %s: %w", redacted(serverURL), err)
	}
	defer admin.Close(ctx)
	if _, err := admin.Exec(ctx, "create database "+name); err != nil {
		return "", nil, fmt.Errorf("creating %s: %w", name, err)
	}
	parsed, err := url.Parse(serverURL)
	if err != nil {
		return "", nil, err
	}
	parsed.Path = "/" + name
	cleanup := func() {
		admin, err := pgx.Connect(ctx, serverURL)
		if err != nil {
			log.Printf("reconnecting to drop %s: %v", name, err)
			return
		}
		defer admin.Close(ctx)
		if _, err := admin.Exec(ctx, "drop database "+name+" with (force)"); err != nil {
			log.Printf("dropping %s: %v", name, err)
		}
	}
	return parsed.String(), cleanup, nil
}

func embedded() (string, func(), error) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return "", nil, err
	}
	port := uint32(listener.Addr().(*net.TCPAddr).Port)
	if err := listener.Close(); err != nil {
		return "", nil, err
	}
	runtimeDir, err := os.MkdirTemp("", "thock-devpg-")
	if err != nil {
		return "", nil, err
	}
	// Shared with the package tests, so the binary is downloaded once.
	cache := filepath.Join(os.TempDir(), "thock-plus-embedded-postgres")
	server := embeddedpostgres.NewDatabase(embeddedpostgres.DefaultConfig().
		Version(embeddedpostgres.V16).
		Port(port).
		Username("thock").
		Password("thock").
		Database("thock").
		RuntimePath(runtimeDir).
		CachePath(cache).
		StartTimeout(90 * time.Second).
		Logger(nil))
	if err := server.Start(); err != nil {
		if removeErr := os.RemoveAll(runtimeDir); removeErr != nil {
			log.Printf("removing %s: %v", runtimeDir, removeErr)
		}
		return "", nil, fmt.Errorf("starting embedded postgres: %w", err)
	}
	cleanup := func() {
		if err := server.Stop(); err != nil {
			log.Printf("stopping embedded postgres: %v", err)
		}
		if err := os.RemoveAll(runtimeDir); err != nil {
			log.Printf("removing %s: %v", runtimeDir, err)
		}
	}
	return fmt.Sprintf("postgres://thock:thock@localhost:%d/thock?sslmode=disable", port), cleanup, nil
}

func redacted(raw string) string {
	parsed, err := url.Parse(raw)
	if err != nil {
		return "the database server"
	}
	return parsed.Redacted()
}
