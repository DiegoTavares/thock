package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"sort"
	"sync"
	"time"
)

// Plans are configuration, never code (spec decision 14): everything that
// prices or sizes a tier lives in this file so it can change without a
// deploy. The catalog re-reads the file whenever its mtime moves, so an
// operator edit lands on the next request.
type plansConfig struct {
	Version int `json:"version"`
	// How many normalized units one dollar of gateway spend costs. 100 makes a
	// unit a cent, which is what the spec's "cost-in-cents based" means.
	UnitsPerDollar float64         `json:"units_per_dollar"`
	Plans          map[string]plan `json:"plans"`
}

type plan struct {
	ID             string     `json:"id"`
	Name           string     `json:"name"`
	AllowanceUnits int64      `json:"allowance_units"`
	CycleDays      int        `json:"cycle_days"`
	Models         modelTiers `json:"models"`
	Limits         planLimits `json:"limits"`
}

// The abstract tiers the app and the skills speak (Default/Fast) mapped to
// OpenRouter model slugs. Users never see the right-hand side.
type modelTiers struct {
	Default string `json:"default"`
	Fast    string `json:"fast"`
}

type planLimits struct {
	WarnAtPercent      int `json:"warn_at_percent"`
	MaxTurnsPerSession int `json:"max_turns_per_session"`
}

func (p plan) allowanceDollars(units float64) float64 {
	if units <= 0 {
		return 0
	}
	return float64(p.AllowanceUnits) / units
}

func (p plan) cycleLength() time.Duration {
	days := p.CycleDays
	if days <= 0 {
		days = 30
	}
	return time.Duration(days) * 24 * time.Hour
}

func parsePlans(raw []byte) (plansConfig, error) {
	var config plansConfig
	if err := json.Unmarshal(raw, &config); err != nil {
		return plansConfig{}, fmt.Errorf("plans file is not valid JSON: %w", err)
	}
	if config.UnitsPerDollar <= 0 {
		config.UnitsPerDollar = 100
	}
	if len(config.Plans) == 0 {
		return plansConfig{}, errors.New("plans file defines no plans")
	}
	for id, plan := range config.Plans {
		if plan.ID == "" {
			plan.ID = id
		}
		if plan.ID != id {
			return plansConfig{}, fmt.Errorf("plan %q declares a different id %q", id, plan.ID)
		}
		if plan.Name == "" {
			plan.Name = id
		}
		if plan.AllowanceUnits < 0 {
			return plansConfig{}, fmt.Errorf("plan %q has a negative allowance", id)
		}
		if plan.Models.Default == "" {
			return plansConfig{}, fmt.Errorf("plan %q has no default model", id)
		}
		if plan.Models.Fast == "" {
			plan.Models.Fast = plan.Models.Default
		}
		if plan.Limits.WarnAtPercent <= 0 || plan.Limits.WarnAtPercent > 100 {
			plan.Limits.WarnAtPercent = 80
		}
		config.Plans[id] = plan
	}
	return config, nil
}

type planCatalog struct {
	path string

	mu       sync.Mutex
	config   plansConfig
	modTime  time.Time
	loadedAt time.Time
}

func loadPlanCatalog(path string) (*planCatalog, error) {
	catalog := &planCatalog{path: path}
	if err := catalog.reload(); err != nil {
		return nil, err
	}
	return catalog, nil
}

// reload re-reads the file unconditionally. A file that fails to parse keeps
// the last good config in place: a typo must never take the service down.
func (c *planCatalog) reload() error {
	raw, err := os.ReadFile(c.path)
	if err != nil {
		return fmt.Errorf("reading plans: %w", err)
	}
	info, err := os.Stat(c.path)
	if err != nil {
		return fmt.Errorf("reading plans: %w", err)
	}
	config, err := parsePlans(raw)
	if err != nil {
		return err
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.config = config
	c.modTime = info.ModTime()
	c.loadedAt = time.Now()
	return nil
}

// current returns the live config, picking up an edited file on the way.
func (c *planCatalog) current() plansConfig {
	c.mu.Lock()
	modTime := c.modTime
	c.mu.Unlock()
	if info, err := os.Stat(c.path); err == nil && !info.ModTime().Equal(modTime) {
		if err := c.reload(); err != nil {
			logf("warning: keeping the previous plans, the edited file didn't load: %v", err)
		}
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.config
}

func (c *planCatalog) plan(id string) (plan, plansConfig, bool) {
	config := c.current()
	plan, ok := config.Plans[id]
	return plan, config, ok
}

func (c plansConfig) sortedPlans() []plan {
	plans := make([]plan, 0, len(c.Plans))
	for _, plan := range c.Plans {
		plans = append(plans, plan)
	}
	sort.Slice(plans, func(i, j int) bool { return plans[i].ID < plans[j].ID })
	return plans
}
