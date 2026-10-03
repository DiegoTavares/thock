// Thock's budget circuit breaker (spec: thock/specs/v36-budget-guard.md).
//
// Receives Cloud Billing budget notifications through a Pub/Sub push
// subscription and, once a project's actual spend reaches a set multiple of
// its budget, takes the project's public surface offline. `restore` puts it
// back:
//
//	budget-guard                                 serve (Cloud Run)
//	budget-guard restore [-dry-run] <project>    undo a trip
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"golang.org/x/oauth2/google"
)

const defaultRatio = 2.0

type budgetTarget struct {
	project string
	ratio   float64
}

// parseBudgets reads BUDGETS: comma-separated `<budget id>=<project>[@<ratio>]`.
func parseBudgets(value string) (map[string]budgetTarget, error) {
	budgets := map[string]budgetTarget{}
	for entry := range strings.SplitSeq(value, ",") {
		entry = strings.TrimSpace(entry)
		if entry == "" {
			continue
		}
		budgetID, target, found := strings.Cut(entry, "=")
		if !found || budgetID == "" || target == "" {
			return nil, fmt.Errorf("budget entry %q is not <budget id>=<project>[@<ratio>]", entry)
		}
		project, ratioText, hasRatio := strings.Cut(target, "@")
		ratio := defaultRatio
		if hasRatio {
			parsed, err := strconv.ParseFloat(ratioText, 64)
			if err != nil || parsed < 1 {
				return nil, fmt.Errorf("budget entry %q: the ratio must be a number of at least 1", entry)
			}
			ratio = parsed
		}
		budgets[budgetID] = budgetTarget{project: project, ratio: ratio}
	}
	if len(budgets) == 0 {
		return nil, errors.New("BUDGETS names no budgets")
	}
	return budgets, nil
}

type pushEnvelope struct {
	Message struct {
		Data       string            `json:"data"`
		Attributes map[string]string `json:"attributes"`
		MessageID  string            `json:"messageId"`
	} `json:"message"`
}

type budgetNotification struct {
	BudgetDisplayName string  `json:"budgetDisplayName"`
	CostAmount        float64 `json:"costAmount"`
	BudgetAmount      float64 `json:"budgetAmount"`
	CurrencyCode      string  `json:"currencyCode"`
	CostIntervalStart string  `json:"costIntervalStart"`
}

type server struct {
	guard   *guard
	budgets map[string]budgetTarget
}

func (s *server) routes() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /pubsub", s.handlePush)
	mux.HandleFunc("GET /health", func(w http.ResponseWriter, _ *http.Request) {
		fmt.Fprintln(w, "ok")
	})
	return mux
}

// handlePush always acknowledges: a failed action is logged and retried by
// the budget's next notification, not by Pub/Sub redelivering this one in a
// tight loop.
func (s *server) handlePush(w http.ResponseWriter, r *http.Request) {
	defer w.WriteHeader(http.StatusNoContent)

	var envelope pushEnvelope
	if err := json.NewDecoder(io.LimitReader(r.Body, 1<<20)).Decode(&envelope); err != nil {
		logEvent("ERROR", map[string]any{"error": "malformed push: " + err.Error()})
		return
	}
	budgetID := envelope.Message.Attributes["budgetId"]
	target, known := s.budgets[budgetID]
	if !known {
		return
	}
	data, err := base64.StdEncoding.DecodeString(envelope.Message.Data)
	if err != nil {
		logEvent("ERROR", map[string]any{"budget": budgetID, "error": "malformed message data: " + err.Error()})
		return
	}
	var notification budgetNotification
	if err := json.Unmarshal(data, &notification); err != nil {
		logEvent("ERROR", map[string]any{"budget": budgetID, "error": "malformed notification: " + err.Error()})
		return
	}
	if notification.BudgetAmount <= 0 || notification.CostAmount/notification.BudgetAmount < target.ratio {
		return
	}

	logEvent("WARNING", map[string]any{
		"action":   "trip",
		"budget":   notification.BudgetDisplayName,
		"project":  target.project,
		"cost":     notification.CostAmount,
		"amount":   notification.BudgetAmount,
		"currency": notification.CurrencyCode,
		"dry_run":  s.guard.dryRun,
	})
	// Detached so Pub/Sub giving up on the push doesn't abandon a trip
	// halfway through.
	ctx, cancel := context.WithTimeout(context.WithoutCancel(r.Context()), 2*time.Minute)
	defer cancel()
	if err := s.guard.trip(ctx, target.project, billingPeriod(notification.CostIntervalStart)); err != nil {
		logEvent("ERROR", map[string]any{"action": "trip", "project": target.project, "error": err.Error()})
	}
}

// billingPeriod is the month a cost interval starts in, "2026-10". Budgets
// report intervals starting at Pacific midnight, which is still the right
// month in UTC.
func billingPeriod(intervalStart string) string {
	start, err := time.Parse(time.RFC3339, intervalStart)
	if err != nil {
		return ""
	}
	return start.UTC().Format("2006-01")
}

func newGuard(ctx context.Context, dryRun bool) (*guard, error) {
	client, err := google.DefaultClient(ctx, "https://www.googleapis.com/auth/cloud-platform")
	if err != nil {
		return nil, fmt.Errorf("loading Google credentials: %w", err)
	}
	client.Timeout = 30 * time.Second
	return &guard{cloud: newRestCloud(client), dryRun: dryRun}, nil
}

func runRestore(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("restore", flag.ContinueOnError)
	dryRun := flags.Bool("dry-run", false, "print what would be restored without changing anything")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() != 1 {
		return errors.New("usage: budget-guard restore [-dry-run] <project>")
	}
	g, err := newGuard(ctx, *dryRun)
	if err != nil {
		return err
	}
	return g.restore(ctx, flags.Arg(0), time.Now().UTC().Format("2006-01"))
}

func main() {
	log.SetFlags(0)
	ctx := context.Background()

	if len(os.Args) > 1 && os.Args[1] == "restore" {
		if err := runRestore(ctx, os.Args[2:]); err != nil {
			log.Fatal(err)
		}
		return
	}

	budgets, err := parseBudgets(os.Getenv("BUDGETS"))
	if err != nil {
		log.Fatal(err)
	}
	dryRun := false
	if value := os.Getenv("DRY_RUN"); value != "" {
		if dryRun, err = strconv.ParseBool(value); err != nil {
			log.Fatalf("DRY_RUN=%q is not a boolean", value)
		}
	}
	g, err := newGuard(ctx, dryRun)
	if err != nil {
		log.Fatal(err)
	}
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	s := &server{guard: g, budgets: budgets}
	log.Printf("budget guard listening on :%s, watching %d budget(s), dry run %t", port, len(budgets), g.dryRun)
	log.Fatal(http.ListenAndServe(":"+port, s.routes()))
}
