package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sync"
	"time"
)

// The model gateway that hands out per-user, budget-capped keys. OpenRouter's
// provisioning API is the first driver (spec decision 6); LiteLLM would be
// the second. Nothing above this interface knows which one is wired in.
type gateway interface {
	// Mint creates a key that stops working once its cumulative spend
	// reaches limitUSD.
	mint(ctx context.Context, name string, limitUSD float64) (gatewayKey, error)
	// Usage reports the key's cumulative spend in dollars.
	usage(ctx context.Context, hash string) (float64, error)
	// Configure raises the spend cap and flips the key on or off.
	configure(ctx context.Context, hash string, limitUSD float64, disabled bool) error
	revoke(ctx context.Context, hash string) error
}

type openRouterGateway struct {
	baseURL       string
	managementKey string
	client        *http.Client
}

func newOpenRouterGateway(managementKey string) *openRouterGateway {
	return &openRouterGateway{
		baseURL:       "https://openrouter.ai/api/v1/keys",
		managementKey: managementKey,
		client:        &http.Client{Timeout: 15 * time.Second},
	}
}

type openRouterKeyData struct {
	Hash     string  `json:"hash"`
	Label    string  `json:"label"`
	Name     string  `json:"name"`
	Disabled bool    `json:"disabled"`
	Limit    float64 `json:"limit"`
	Usage    float64 `json:"usage"`
}

func (g *openRouterGateway) call(ctx context.Context, method, path string, body any, out any) error {
	var reader io.Reader
	if body != nil {
		raw, err := json.Marshal(body)
		if err != nil {
			return err
		}
		reader = bytes.NewReader(raw)
	}
	request, err := http.NewRequestWithContext(ctx, method, g.baseURL+path, reader)
	if err != nil {
		return err
	}
	request.Header.Set("Authorization", "Bearer "+g.managementKey)
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	response, err := g.client.Do(request)
	if err != nil {
		return fmt.Errorf("openrouter %s %s: %w", method, path, err)
	}
	defer response.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return err
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("openrouter %s %s: %s: %s", method, path, response.Status, bytes.TrimSpace(raw))
	}
	if out != nil && len(raw) > 0 {
		if err := json.Unmarshal(raw, out); err != nil {
			return fmt.Errorf("openrouter %s %s: unreadable response: %w", method, path, err)
		}
	}
	return nil
}

func (g *openRouterGateway) mint(ctx context.Context, name string, limitUSD float64) (gatewayKey, error) {
	var created struct {
		Data openRouterKeyData `json:"data"`
		Key  string            `json:"key"`
	}
	body := map[string]any{"name": name, "limit": limitUSD}
	if err := g.call(ctx, http.MethodPost, "", body, &created); err != nil {
		return gatewayKey{}, err
	}
	if created.Key == "" || created.Data.Hash == "" {
		return gatewayKey{}, fmt.Errorf("openrouter returned a key without a secret or hash")
	}
	return gatewayKey{Hash: created.Data.Hash, Secret: created.Key, LimitUSD: limitUSD}, nil
}

func (g *openRouterGateway) usage(ctx context.Context, hash string) (float64, error) {
	var fetched struct {
		Data openRouterKeyData `json:"data"`
	}
	if err := g.call(ctx, http.MethodGet, "/"+hash, nil, &fetched); err != nil {
		return 0, err
	}
	return fetched.Data.Usage, nil
}

func (g *openRouterGateway) configure(ctx context.Context, hash string, limitUSD float64, disabled bool) error {
	body := map[string]any{"limit": limitUSD, "disabled": disabled}
	return g.call(ctx, http.MethodPatch, "/"+hash, body, nil)
}

func (g *openRouterGateway) revoke(ctx context.Context, hash string) error {
	return g.call(ctx, http.MethodDelete, "/"+hash, nil, nil)
}

// fakeGateway stands in when no management key is configured: local runs
// and tests. Keys are made up, and usage is whatever a test (or the admin
// endpoint) sets, so the allowance loop can be exercised without spending.
type fakeGateway struct {
	mu   sync.Mutex
	keys map[string]*fakeKey
	seq  int
}

type fakeKey struct {
	Secret   string
	LimitUSD float64
	Disabled bool
	UsageUSD float64
	Revoked  bool
}

func newFakeGateway() *fakeGateway {
	return &fakeGateway{keys: map[string]*fakeKey{}}
}

func (g *fakeGateway) mint(_ context.Context, name string, limitUSD float64) (gatewayKey, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.seq++
	hash := fmt.Sprintf("fake-%d", g.seq)
	secret := fmt.Sprintf("sk-or-v1-fake-%s-%d", name, g.seq)
	g.keys[hash] = &fakeKey{Secret: secret, LimitUSD: limitUSD}
	return gatewayKey{Hash: hash, Secret: secret, LimitUSD: limitUSD}, nil
}

func (g *fakeGateway) usage(_ context.Context, hash string) (float64, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	key, ok := g.keys[hash]
	if !ok || key.Revoked {
		return 0, fmt.Errorf("no such key %q", hash)
	}
	return key.UsageUSD, nil
}

func (g *fakeGateway) configure(_ context.Context, hash string, limitUSD float64, disabled bool) error {
	g.mu.Lock()
	defer g.mu.Unlock()
	key, ok := g.keys[hash]
	if !ok || key.Revoked {
		return fmt.Errorf("no such key %q", hash)
	}
	key.LimitUSD = limitUSD
	key.Disabled = disabled
	return nil
}

func (g *fakeGateway) revoke(_ context.Context, hash string) error {
	g.mu.Lock()
	defer g.mu.Unlock()
	key, ok := g.keys[hash]
	if !ok {
		return fmt.Errorf("no such key %q", hash)
	}
	key.Revoked = true
	return nil
}

// spend simulates gateway traffic against a key (tests and local runs).
func (g *fakeGateway) spend(hash string, usd float64) error {
	g.mu.Lock()
	defer g.mu.Unlock()
	key, ok := g.keys[hash]
	if !ok || key.Revoked {
		return fmt.Errorf("no such key %q", hash)
	}
	if key.Disabled {
		return fmt.Errorf("key %q is disabled", hash)
	}
	key.UsageUSD += usd
	return nil
}

// mintedAndRevoked reports whether exactly `minted` keys were ever created
// and `revoked` of them have since been deleted.
func (g *fakeGateway) mintedAndRevoked(minted, revoked int) bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	count := 0
	for _, key := range g.keys {
		if key.Revoked {
			count++
		}
	}
	return len(g.keys) == minted && count == revoked
}

func (g *fakeGateway) lookup(hash string) (fakeKey, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	key, ok := g.keys[hash]
	if !ok {
		return fakeKey{}, false
	}
	return *key, true
}
