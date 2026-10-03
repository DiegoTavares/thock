package main

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// The backend's own record of users, entitlements, and usage: the source of
// truth that a billing driver (Polar, in Stage 2) grants into, never the
// other way around. Postgres, with the schema in migrations/ applied at
// startup; every write that touches more than one row runs in a transaction.
type userStatus string

const (
	userActive  userStatus = "active"
	userRevoked userStatus = "revoked"
)

type user struct {
	ID             string     `json:"id"`
	Plan           string     `json:"plan"`
	Device         string     `json:"device"`
	InviteCode     string     `json:"invite_code"`
	CredentialHash string     `json:"credential_hash"`
	Status         userStatus `json:"status"`
	CreatedAt      time.Time  `json:"created_at"`
	CycleStartedAt time.Time  `json:"cycle_started_at"`

	Gateway gatewayKey `json:"gateway"`
	// Gateway usage (dollars) when the current cycle started; usage in this
	// cycle is whatever the gateway reports above it.
	UsageBaselineUSD float64 `json:"usage_baseline_usd"`
	// Units burned this cycle, as of the last gateway sync.
	UsedUnits int64 `json:"used_units"`
	// Units granted on top of the plan allowance this cycle (admin top-ups,
	// or taken away when negative). Cleared at rollover.
	AdjustUnits int64     `json:"adjust_units"`
	LastSyncAt  time.Time `json:"last_sync_at"`
	// The gateway key is disabled while true; flipped back when a top-up or
	// a new cycle brings the balance above zero.
	Exhausted bool `json:"exhausted"`

	// The phone's own key (V35 §5.1), minted on its first grant and revoked
	// with the phone. An empty hash means there is none. Its usage above its
	// own baseline counts against the same allowance as the desk key's.
	PhoneGateway          gatewayKey `json:"phone_gateway"`
	PhoneUsageBaselineUSD float64    `json:"phone_usage_baseline_usd"`
}

type gatewayKey struct {
	Hash string `json:"hash"`
	// The provisioned key itself. Extractable by design (spec decision 12):
	// the budget cap bounds the damage, not secrecy of this row.
	Secret   string  `json:"secret"`
	LimitUSD float64 `json:"limit_usd"`
}

type invite struct {
	Code      string    `json:"code"`
	Plan      string    `json:"plan"`
	MaxUses   int       `json:"max_uses"`
	Uses      int       `json:"uses"`
	Note      string    `json:"note"`
	CreatedAt time.Time `json:"created_at"`
}

type ledgerEntry struct {
	At     time.Time `json:"at"`
	UserID string    `json:"user_id"`
	Units  int64     `json:"units"`
	// "sync" (gateway usage landed), "adjust" (admin), "reset" (admin or
	// cycle rollover), "revoke", "connect".
	Source string `json:"source"`
	Note   string `json:"note,omitempty"`
}

var (
	errNotFound        = errors.New("not found")
	errInviteExhausted = errors.New("invite exhausted")
)

type store struct {
	pool *pgxpool.Pool
}

// openStore connects and brings the schema up to date before anything else
// runs against it.
func openStore(ctx context.Context, databaseURL string) (*store, error) {
	pool, err := openPool(ctx, databaseURL)
	if err != nil {
		return nil, err
	}
	applied, err := migrate(ctx, pool)
	if err != nil {
		pool.Close()
		return nil, fmt.Errorf("migrating: %w", err)
	}
	for _, name := range applied {
		logf("applied migration %s", name)
	}
	return &store{pool: pool}, nil
}

func (s *store) close() {
	s.pool.Close()
}

// --- plans and settings ---

type settings struct {
	UnitsPerDollar float64 `json:"units_per_dollar"`
}

func (s *store) settings(ctx context.Context) (settings, error) {
	var result settings
	err := s.pool.QueryRow(ctx, "select units_per_dollar from settings").Scan(&result.UnitsPerDollar)
	if err != nil {
		return settings{}, fmt.Errorf("reading settings: %w", err)
	}
	return result, nil
}

func (s *store) updateSettings(ctx context.Context, unitsPerDollar float64) error {
	_, err := s.pool.Exec(ctx, "update settings set units_per_dollar = $1", unitsPerDollar)
	return err
}

const planColumns = "id, name, allowance_units, cycle_days, default_model, fast_model, warn_at_percent, max_turns_per_session, vault_quota_bytes"

func scanPlan(row pgx.Row) (plan, error) {
	var p plan
	err := row.Scan(&p.ID, &p.Name, &p.AllowanceUnits, &p.CycleDays, &p.Models.Default, &p.Models.Fast,
		&p.Limits.WarnAtPercent, &p.Limits.MaxTurnsPerSession, &p.Limits.VaultQuotaBytes)
	if errors.Is(err, pgx.ErrNoRows) {
		return plan{}, errNotFound
	}
	return p, err
}

// plan returns a plan together with the settings it is priced under; the
// two always travel together in the allowance loop.
func (s *store) plan(ctx context.Context, id string) (plan, settings, error) {
	p, err := scanPlan(s.pool.QueryRow(ctx, "select "+planColumns+" from plans where id = $1", id))
	if err != nil {
		return plan{}, settings{}, err
	}
	config, err := s.settings(ctx)
	if err != nil {
		return plan{}, settings{}, err
	}
	return p, config, nil
}

func (s *store) listPlans(ctx context.Context) ([]plan, error) {
	rows, err := s.pool.Query(ctx, "select "+planColumns+" from plans order by id")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	plans := []plan{}
	for rows.Next() {
		p, err := scanPlan(rows)
		if err != nil {
			return nil, err
		}
		plans = append(plans, p)
	}
	return plans, rows.Err()
}

func (s *store) upsertPlan(ctx context.Context, p plan) error {
	_, err := s.pool.Exec(ctx, `insert into plans (`+planColumns+`)
		values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
		on conflict (id) do update set
			name = excluded.name,
			allowance_units = excluded.allowance_units,
			cycle_days = excluded.cycle_days,
			default_model = excluded.default_model,
			fast_model = excluded.fast_model,
			warn_at_percent = excluded.warn_at_percent,
			max_turns_per_session = excluded.max_turns_per_session,
			vault_quota_bytes = excluded.vault_quota_bytes,
			updated_at = now()`,
		p.ID, p.Name, p.AllowanceUnits, p.CycleDays, p.Models.Default, p.Models.Fast,
		p.Limits.WarnAtPercent, p.Limits.MaxTurnsPerSession, p.Limits.VaultQuotaBytes)
	return err
}

// --- invites ---

const inviteColumns = "code, plan_id, max_uses, uses, note, created_at"

func scanInvite(row pgx.Row) (invite, error) {
	var i invite
	err := row.Scan(&i.Code, &i.Plan, &i.MaxUses, &i.Uses, &i.Note, &i.CreatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return invite{}, errNotFound
	}
	i.CreatedAt = i.CreatedAt.UTC()
	return i, err
}

func (s *store) createInvite(ctx context.Context, i invite) error {
	_, err := s.pool.Exec(ctx, "insert into invites ("+inviteColumns+") values ($1, $2, $3, $4, $5, $6)",
		i.Code, i.Plan, i.MaxUses, i.Uses, i.Note, i.CreatedAt)
	return err
}

func (s *store) inviteByCode(ctx context.Context, code string) (invite, error) {
	return scanInvite(s.pool.QueryRow(ctx, "select "+inviteColumns+" from invites where code = $1", code))
}

func (s *store) listInvites(ctx context.Context) ([]invite, error) {
	rows, err := s.pool.Query(ctx, "select "+inviteColumns+" from invites order by created_at, code")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	invites := []invite{}
	for rows.Next() {
		i, err := scanInvite(rows)
		if err != nil {
			return nil, err
		}
		invites = append(invites, i)
	}
	return invites, rows.Err()
}

// --- users ---

const userColumns = `id, plan_id, device, coalesce(invite_code, ''), credential_hash, status, created_at, cycle_started_at,
	gateway_key_hash, gateway_key_secret, gateway_limit_usd, usage_baseline_usd, used_units, adjust_units, last_sync_at, exhausted,
	phone_gateway_key_hash, phone_gateway_key_secret, phone_gateway_limit_usd, phone_usage_baseline_usd`

func scanUser(row pgx.Row) (user, error) {
	var u user
	err := row.Scan(&u.ID, &u.Plan, &u.Device, &u.InviteCode, &u.CredentialHash, &u.Status, &u.CreatedAt, &u.CycleStartedAt,
		&u.Gateway.Hash, &u.Gateway.Secret, &u.Gateway.LimitUSD, &u.UsageBaselineUSD, &u.UsedUnits, &u.AdjustUnits,
		&u.LastSyncAt, &u.Exhausted,
		&u.PhoneGateway.Hash, &u.PhoneGateway.Secret, &u.PhoneGateway.LimitUSD, &u.PhoneUsageBaselineUSD)
	if errors.Is(err, pgx.ErrNoRows) {
		return user{}, errNotFound
	}
	u.CreatedAt = u.CreatedAt.UTC()
	u.CycleStartedAt = u.CycleStartedAt.UTC()
	u.LastSyncAt = u.LastSyncAt.UTC()
	return u, err
}

// createUser spends one use of the invite and records the new user, all or
// nothing: the invite row is locked so two connects racing on the last use
// can't both win.
func (s *store) createUser(ctx context.Context, u user, entry ledgerEntry) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		var maxUses, uses int
		err := tx.QueryRow(ctx, "select max_uses, uses from invites where code = $1 for update", u.InviteCode).Scan(&maxUses, &uses)
		if errors.Is(err, pgx.ErrNoRows) {
			return errNotFound
		}
		if err != nil {
			return err
		}
		if maxUses > 0 && uses >= maxUses {
			return errInviteExhausted
		}
		if _, err := tx.Exec(ctx, "update invites set uses = uses + 1 where code = $1", u.InviteCode); err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `insert into users (id, plan_id, device, invite_code, credential_hash, status, created_at, cycle_started_at,
			gateway_key_hash, gateway_key_secret, gateway_limit_usd, usage_baseline_usd, used_units, adjust_units, last_sync_at, exhausted,
			phone_gateway_key_hash, phone_gateway_key_secret, phone_gateway_limit_usd, phone_usage_baseline_usd)
			values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)`,
			u.ID, u.Plan, u.Device, u.InviteCode, u.CredentialHash, string(u.Status), u.CreatedAt, u.CycleStartedAt,
			u.Gateway.Hash, u.Gateway.Secret, u.Gateway.LimitUSD, u.UsageBaselineUSD, u.UsedUnits, u.AdjustUnits,
			u.LastSyncAt, u.Exhausted,
			u.PhoneGateway.Hash, u.PhoneGateway.Secret, u.PhoneGateway.LimitUSD, u.PhoneUsageBaselineUSD)
		if err != nil {
			return err
		}
		return insertLedger(ctx, tx, entry)
	})
}

func (s *store) userByCredential(ctx context.Context, hash string) (user, error) {
	return scanUser(s.pool.QueryRow(ctx, "select "+userColumns+" from users where credential_hash = $1", hash))
}

func (s *store) userByID(ctx context.Context, id string) (user, error) {
	return scanUser(s.pool.QueryRow(ctx, "select "+userColumns+" from users where id = $1", id))
}

func (s *store) listUsers(ctx context.Context) ([]user, error) {
	rows, err := s.pool.Query(ctx, "select "+userColumns+" from users order by created_at, id")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	users := []user{}
	for rows.Next() {
		u, err := scanUser(rows)
		if err != nil {
			return nil, err
		}
		users = append(users, u)
	}
	return users, rows.Err()
}

// saveAllowance persists the outcome of one pass of the allowance loop (cycle,
// usage, gateway cap and state) together with the ledger lines explaining it.
func (s *store) saveAllowance(ctx context.Context, u user, entries []ledgerEntry) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		tag, err := tx.Exec(ctx, `update users set
			cycle_started_at = $2, usage_baseline_usd = $3, used_units = $4, adjust_units = $5,
			last_sync_at = $6, exhausted = $7, gateway_key_hash = $8, gateway_key_secret = $9, gateway_limit_usd = $10,
			phone_gateway_key_hash = $11, phone_gateway_key_secret = $12, phone_gateway_limit_usd = $13, phone_usage_baseline_usd = $14
			where id = $1`,
			u.ID, u.CycleStartedAt, u.UsageBaselineUSD, u.UsedUnits, u.AdjustUnits,
			u.LastSyncAt, u.Exhausted, u.Gateway.Hash, u.Gateway.Secret, u.Gateway.LimitUSD,
			u.PhoneGateway.Hash, u.PhoneGateway.Secret, u.PhoneGateway.LimitUSD, u.PhoneUsageBaselineUSD)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return errNotFound
		}
		for _, entry := range entries {
			if err := insertLedger(ctx, tx, entry); err != nil {
				return err
			}
		}
		return nil
	})
}

// touchSync moves the sync clock forward without touching anything else.
func (s *store) touchSync(ctx context.Context, id string, at time.Time) error {
	_, err := s.pool.Exec(ctx, "update users set last_sync_at = $2 where id = $1 and last_sync_at < $2", id, at)
	return err
}

// setPhoneKey records a freshly minted phone key, which has spent nothing
// yet. errNotFound when the user is gone or already holds a phone key: the
// caller's key lost and must be revoked.
func (s *store) setPhoneKey(ctx context.Context, id string, key gatewayKey) error {
	tag, err := s.pool.Exec(ctx, `update users set
		phone_gateway_key_hash = $2, phone_gateway_key_secret = $3, phone_gateway_limit_usd = $4, phone_usage_baseline_usd = 0
		where id = $1 and phone_gateway_key_hash = ''`,
		id, key.Hash, key.Secret, key.LimitUSD)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return errNotFound
	}
	return nil
}

// clearPhoneKey forgets the phone key with this hash; a different key that
// took its place in the meantime is left alone.
func (s *store) clearPhoneKey(ctx context.Context, id, hash string, carriedUSD float64) error {
	// The departing key's spend moves onto the desk key's side of the sum by
	// lowering that baseline, which a new cycle resets like any other. The
	// last sync is forgotten so the very next pass recounts: a balance cached
	// from before would let a fresh phone key start with the old one's
	// allowance.
	_, err := s.pool.Exec(ctx, `update users set
		phone_gateway_key_hash = '', phone_gateway_key_secret = '', phone_gateway_limit_usd = 0, phone_usage_baseline_usd = 0,
		usage_baseline_usd = usage_baseline_usd - $3, last_sync_at = 'epoch'
		where id = $1 and phone_gateway_key_hash = $2`,
		id, hash, carriedUSD)
	return err
}

func (s *store) revokeUser(ctx context.Context, id string, at time.Time, note string) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		tag, err := tx.Exec(ctx, `update users set status = $2, gateway_key_secret = '', phone_gateway_key_secret = '', exhausted = true where id = $1`,
			id, string(userRevoked))
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return errNotFound
		}
		// The vault benefit ends with the access (V34 §12); the phone keeps
		// reading for the grace window and the sweeper deletes afterwards.
		if _, err := tx.Exec(ctx, "update vaults set lapsed_at = $2 where user_id = $1 and lapsed_at is null", id, at); err != nil {
			return err
		}
		return insertLedger(ctx, tx, ledgerEntry{At: at, UserID: id, Source: "revoke", Note: note})
	})
}

type allowanceChange struct {
	Plan string
	// Start a fresh cycle at `At` from these gateway baselines, one per key.
	Reset            bool
	BaselineUSD      float64
	PhoneBaselineUSD float64
	AdjustUnits      int64
	Note             string
	At               time.Time
}

// applyAllowance records an admin change and clears the sync clock so the
// next entitlement read re-syncs and re-enforces at the gateway.
func (s *store) applyAllowance(ctx context.Context, id string, change allowanceChange) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		if change.Plan != "" {
			if _, err := tx.Exec(ctx, "update users set plan_id = $2 where id = $1", id, change.Plan); err != nil {
				return err
			}
		}
		if change.Reset {
			_, err := tx.Exec(ctx, `update users set cycle_started_at = $2, usage_baseline_usd = $3, phone_usage_baseline_usd = $4, used_units = 0, adjust_units = 0 where id = $1`,
				id, change.At, change.BaselineUSD, change.PhoneBaselineUSD)
			if err != nil {
				return err
			}
			if err := insertLedger(ctx, tx, ledgerEntry{At: change.At, UserID: id, Source: "reset", Note: change.Note}); err != nil {
				return err
			}
		}
		if change.AdjustUnits != 0 {
			if _, err := tx.Exec(ctx, "update users set adjust_units = adjust_units + $2 where id = $1", id, change.AdjustUnits); err != nil {
				return err
			}
			if err := insertLedger(ctx, tx, ledgerEntry{At: change.At, UserID: id, Units: -change.AdjustUnits, Source: "adjust", Note: change.Note}); err != nil {
				return err
			}
		}
		tag, err := tx.Exec(ctx, "update users set last_sync_at = $2 where id = $1", id, time.Time{}.UTC())
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return errNotFound
		}
		return nil
	})
}

func insertLedger(ctx context.Context, tx pgx.Tx, entry ledgerEntry) error {
	_, err := tx.Exec(ctx, "insert into ledger (at, user_id, units, source, note) values ($1, $2, $3, $4, $5)",
		entry.At, entry.UserID, entry.Units, entry.Source, entry.Note)
	return err
}

func (s *store) ledgerFor(ctx context.Context, userID string) ([]ledgerEntry, error) {
	rows, err := s.pool.Query(ctx, "select at, user_id, units, source, note from ledger where user_id = $1 order by at, id", userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	entries := []ledgerEntry{}
	for rows.Next() {
		var entry ledgerEntry
		if err := rows.Scan(&entry.At, &entry.UserID, &entry.Units, &entry.Source, &entry.Note); err != nil {
			return nil, err
		}
		entry.At = entry.At.UTC()
		entries = append(entries, entry)
	}
	return entries, rows.Err()
}
