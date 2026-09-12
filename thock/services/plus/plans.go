package main

import (
	"errors"
	"fmt"
	"time"
)

// Plans are configuration, never code (spec decision 14): everything that
// prices or sizes a tier is a row in the plans table, changed through the
// admin API (or any SQL client) without a deploy. Every entitlement read
// looks the plan up fresh, so an edit lands on the next request.
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

func (p plan) allowanceDollars(unitsPerDollar float64) float64 {
	if unitsPerDollar <= 0 {
		return 0
	}
	return float64(p.AllowanceUnits) / unitsPerDollar
}

func (p plan) cycleLength() time.Duration {
	return time.Duration(p.CycleDays) * 24 * time.Hour
}

// normalize fills the defaults an operator may leave out and rejects what
// can't be defaulted. The id comes from the URL, so a body that names a
// different one is a mistake worth refusing.
func (p plan) normalize(id string) (plan, error) {
	if id == "" {
		return plan{}, errors.New("a plan needs an id")
	}
	if p.ID == "" {
		p.ID = id
	}
	if p.ID != id {
		return plan{}, fmt.Errorf("the body declares plan %q but the URL names %q", p.ID, id)
	}
	if p.Name == "" {
		p.Name = id
	}
	if p.AllowanceUnits < 0 {
		return plan{}, errors.New("the allowance can't be negative")
	}
	if p.CycleDays <= 0 {
		p.CycleDays = 30
	}
	if p.Models.Default == "" {
		return plan{}, errors.New("a plan needs a default model")
	}
	if p.Models.Fast == "" {
		p.Models.Fast = p.Models.Default
	}
	if p.Limits.WarnAtPercent <= 0 || p.Limits.WarnAtPercent > 100 {
		p.Limits.WarnAtPercent = 80
	}
	if p.Limits.MaxTurnsPerSession < 0 {
		return plan{}, errors.New("max_turns_per_session can't be negative")
	}
	return p, nil
}
