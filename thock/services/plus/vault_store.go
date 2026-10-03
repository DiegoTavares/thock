package main

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

// The vault sync rows (migrations/0002_vault_sync.sql). Every change that
// assigns a version or a seq runs inside withVaultLock so publish order is
// numeric order, which is what makes a single version a safe pull cursor.

type deviceRole string

const (
	roleDesk  deviceRole = "desk"
	rolePhone deviceRole = "phone"
)

type vault struct {
	ID               string
	UserID           string
	KeyCheck         string
	CreatedAt        time.Time
	LatestVersion    int64
	LatestSeq        int64
	AckedThroughSeq  int64
	AckedAtVersion   int64
	BytesUsed        int64
	TombstoneHorizon int64
	LapsedAt         *time.Time
}

type device struct {
	ID             string
	VaultID        string
	Role           deviceRole
	Name           string
	CredentialHash string
	APNSToken      string
	PairedAt       time.Time
	LastSeenAt     time.Time
}

type fileRow struct {
	Path        string
	Version     int64
	Deleted     bool
	BlobID      string
	SizeBytes   int64
	ContentHash string
	UpdatedBy   string
	UpdatedAt   time.Time
}

type pendingUpload struct {
	VaultID         string
	BlobID          string
	Path            string
	ExpectedVersion int64
	SizeBytes       int64
	ContentHash     string
	CreatedAt       time.Time
}

type writeRow struct {
	Seq         int64
	ClientID    string
	Path        string
	BaseVersion int64
	Payload     []byte
	CreatedAt   time.Time
	AckedAt     *time.Time
}

var (
	errStaleVersion = errors.New("stale version")
	errBlobMissing  = errors.New("blob missing")
	errQuota        = errors.New("quota exceeded")
	errExpired      = errors.New("expired")
)

// --- vaults ---

const vaultColumns = `id, user_id, key_check, created_at, latest_version, latest_seq, acked_through_seq, acked_at_version,
	bytes_used, tombstone_horizon, lapsed_at`

func scanVault(row pgx.Row) (vault, error) {
	var v vault
	err := row.Scan(&v.ID, &v.UserID, &v.KeyCheck, &v.CreatedAt, &v.LatestVersion, &v.LatestSeq, &v.AckedThroughSeq,
		&v.AckedAtVersion, &v.BytesUsed, &v.TombstoneHorizon, &v.LapsedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return vault{}, errNotFound
	}
	v.CreatedAt = v.CreatedAt.UTC()
	if v.LapsedAt != nil {
		at := v.LapsedAt.UTC()
		v.LapsedAt = &at
	}
	return v, err
}

func (s *store) vaultByUser(ctx context.Context, userID string) (vault, error) {
	return scanVault(s.pool.QueryRow(ctx, "select "+vaultColumns+" from vaults where user_id = $1", userID))
}

func (s *store) vaultByID(ctx context.Context, id string) (vault, error) {
	return scanVault(s.pool.QueryRow(ctx, "select "+vaultColumns+" from vaults where id = $1", id))
}

// withVaultLock runs fn with the vault row locked for the transaction. fn
// receives the row as it is at lock time; whatever it changes it writes back
// itself through tx.
func (s *store) withVaultLock(ctx context.Context, vaultID string, fn func(tx pgx.Tx, v vault) error) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		v, err := scanVault(tx.QueryRow(ctx, "select "+vaultColumns+" from vaults where id = $1 for update", vaultID))
		if err != nil {
			return err
		}
		return fn(tx, v)
	})
}

// ensureVault creates the vault and its desk device on first call. On later
// calls the key check must match unless the vault holds no files, in which
// case the new check is adopted (contract §6.1). The desk device's name is
// refreshed either way.
func (s *store) ensureVault(ctx context.Context, userID, keyCheck, deviceName string, now time.Time) (vault, error) {
	var result vault
	err := pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		v, err := scanVault(tx.QueryRow(ctx, "select "+vaultColumns+" from vaults where user_id = $1 for update", userID))
		if errors.Is(err, errNotFound) {
			id, err := randomToken(8)
			if err != nil {
				return err
			}
			v = vault{ID: id, UserID: userID, KeyCheck: keyCheck, CreatedAt: now}
			_, err = tx.Exec(ctx, `insert into vaults (id, user_id, key_check, created_at) values ($1, $2, $3, $4)`,
				v.ID, v.UserID, v.KeyCheck, v.CreatedAt)
			if err != nil {
				return err
			}
			deviceID, err := randomToken(8)
			if err != nil {
				return err
			}
			_, err = tx.Exec(ctx, `insert into devices (id, vault_id, role, name, paired_at, last_seen_at) values ($1, $2, $3, $4, $5, $5)`,
				deviceID, v.ID, string(roleDesk), deviceName, now)
			if err != nil {
				return err
			}
			result = v
			return nil
		}
		if err != nil {
			return err
		}
		if v.KeyCheck != keyCheck {
			var liveFiles int
			if err := tx.QueryRow(ctx, "select count(*) from files where vault_id = $1 and not deleted", v.ID).Scan(&liveFiles); err != nil {
				return err
			}
			if liveFiles > 0 {
				return errKeyMismatch
			}
			if _, err := tx.Exec(ctx, "update vaults set key_check = $2 where id = $1", v.ID, keyCheck); err != nil {
				return err
			}
			v.KeyCheck = keyCheck
		}
		_, err = tx.Exec(ctx, `insert into devices (id, vault_id, role, name, paired_at, last_seen_at)
			values ($1, $2, $3, $4, $5, $5)
			on conflict (vault_id, role) do update set name = excluded.name, last_seen_at = excluded.last_seen_at`,
			mustRandomToken(8), v.ID, string(roleDesk), deviceName, now)
		if err != nil {
			return err
		}
		result = v
		return nil
	})
	return result, err
}

var errKeyMismatch = errors.New("key mismatch")

func mustRandomToken(bytes int) string {
	token, err := randomToken(bytes)
	if err != nil {
		// crypto/rand only fails when the OS entropy source is broken, in
		// which case nothing else in this process works either.
		panic(err)
	}
	return token
}

// deleteVault removes the vault and everything under it (cascade) and
// returns the blob ids the caller must delete from the store.
func (s *store) deleteVault(ctx context.Context, vaultID string) ([]string, error) {
	var blobs []string
	err := pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		var err error
		blobs, err = allBlobIDs(ctx, tx, vaultID)
		if err != nil {
			return err
		}
		tag, err := tx.Exec(ctx, "delete from vaults where id = $1", vaultID)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return errNotFound
		}
		return nil
	})
	return blobs, err
}

type blobRef struct {
	VaultID string
	BlobID  string
}

func allBlobIDs(ctx context.Context, tx pgx.Tx, vaultID string) ([]string, error) {
	rows, err := tx.Query(ctx, `select blob_id from files where vault_id = $1 and blob_id is not null
		union all select blob_id from pending_uploads where vault_id = $1`, vaultID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var blobs []string
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		blobs = append(blobs, id)
	}
	return blobs, rows.Err()
}

// resetVault wipes files, pending uploads and writes, revokes the phone, and
// records the new key check. Counters keep going up; the tombstone horizon
// moves to the latest version so a stale cursor is told to pull afresh.
func (s *store) resetVault(ctx context.Context, vaultID, keyCheck string) ([]string, error) {
	var blobs []string
	err := s.withVaultLock(ctx, vaultID, func(tx pgx.Tx, v vault) error {
		var err error
		blobs, err = allBlobIDs(ctx, tx, vaultID)
		if err != nil {
			return err
		}
		for _, statement := range []string{
			"delete from files where vault_id = $1",
			"delete from pending_uploads where vault_id = $1",
			"delete from writes where vault_id = $1",
			"delete from pairings where vault_id = $1",
			"delete from devices where vault_id = $1 and role = 'phone'",
		} {
			if _, err := tx.Exec(ctx, statement, vaultID); err != nil {
				return err
			}
		}
		_, err = tx.Exec(ctx, `update vaults set key_check = $2, bytes_used = 0, tombstone_horizon = latest_version where id = $1`,
			vaultID, keyCheck)
		return err
	})
	return blobs, err
}

func (s *store) setVaultLapsed(ctx context.Context, vaultID string, at *time.Time) error {
	tag, err := s.pool.Exec(ctx, "update vaults set lapsed_at = $2 where id = $1", vaultID, at)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return errNotFound
	}
	return nil
}

type vaultCounts struct {
	Files         int64
	PendingWrites int64
}

func (s *store) vaultCounts(ctx context.Context, vaultID string) (vaultCounts, error) {
	var counts vaultCounts
	err := s.pool.QueryRow(ctx, `select
		(select count(*) from files where vault_id = $1 and not deleted),
		(select count(*) from writes where vault_id = $1 and acked_at is null)`, vaultID).
		Scan(&counts.Files, &counts.PendingWrites)
	return counts, err
}

// --- devices ---

const deviceColumns = "id, vault_id, role, name, coalesce(credential_hash, ''), coalesce(apns_token, ''), paired_at, last_seen_at"

func scanDevice(row pgx.Row) (device, error) {
	var d device
	err := row.Scan(&d.ID, &d.VaultID, &d.Role, &d.Name, &d.CredentialHash, &d.APNSToken, &d.PairedAt, &d.LastSeenAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return device{}, errNotFound
	}
	d.PairedAt = d.PairedAt.UTC()
	d.LastSeenAt = d.LastSeenAt.UTC()
	return d, err
}

func (s *store) deviceByCredential(ctx context.Context, hash string) (device, error) {
	return scanDevice(s.pool.QueryRow(ctx, "select "+deviceColumns+" from devices where credential_hash = $1", hash))
}

func (s *store) deviceByID(ctx context.Context, vaultID, id string) (device, error) {
	return scanDevice(s.pool.QueryRow(ctx, "select "+deviceColumns+" from devices where vault_id = $1 and id = $2", vaultID, id))
}

func (s *store) phoneDevice(ctx context.Context, vaultID string) (device, error) {
	return scanDevice(s.pool.QueryRow(ctx, "select "+deviceColumns+" from devices where vault_id = $1 and role = 'phone'", vaultID))
}

func (s *store) listDevices(ctx context.Context, vaultID string) ([]device, error) {
	rows, err := s.pool.Query(ctx, "select "+deviceColumns+" from devices where vault_id = $1 order by role, paired_at", vaultID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	devices := []device{}
	for rows.Next() {
		d, err := scanDevice(rows)
		if err != nil {
			return nil, err
		}
		devices = append(devices, d)
	}
	return devices, rows.Err()
}

func (s *store) touchDevice(ctx context.Context, vaultID string, role deviceRole, at time.Time) error {
	_, err := s.pool.Exec(ctx, "update devices set last_seen_at = $3 where vault_id = $1 and role = $2 and last_seen_at < $3",
		vaultID, string(role), at)
	return err
}

type devicePatch struct {
	Name      *string
	APNSToken *string
}

func (s *store) patchDevice(ctx context.Context, id string, patch devicePatch) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		if patch.Name != nil {
			if _, err := tx.Exec(ctx, "update devices set name = $2 where id = $1", id, *patch.Name); err != nil {
				return err
			}
		}
		if patch.APNSToken != nil {
			if _, err := tx.Exec(ctx, "update devices set apns_token = nullif($2, '') where id = $1", id, *patch.APNSToken); err != nil {
				return err
			}
		}
		return nil
	})
}

func (s *store) deleteDevice(ctx context.Context, vaultID, id string) error {
	tag, err := s.pool.Exec(ctx, "delete from devices where vault_id = $1 and id = $2 and role = 'phone'", vaultID, id)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return errNotFound
	}
	return nil
}

// --- pairings ---

// createPairing stores the code's hash and drops any earlier unused code:
// only the newest one on the desk's screen works.
func (s *store) createPairing(ctx context.Context, vaultID, codeHash string, now, expires time.Time) error {
	return pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		if _, err := tx.Exec(ctx, "delete from pairings where vault_id = $1 and used_at is null", vaultID); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, "insert into pairings (vault_id, code_hash, expires_at, created_at) values ($1, $2, $3, $4)",
			vaultID, codeHash, expires, now)
		return err
	})
}

// redeemPairing spends the code and replaces the phone device in one
// transaction. errNotFound: unknown or already used. errExpired: too late.
func (s *store) redeemPairing(ctx context.Context, codeHash string, d device) (vault, error) {
	var result vault
	err := pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		var vaultID string
		var expires time.Time
		var used *time.Time
		err := tx.QueryRow(ctx, "select vault_id, expires_at, used_at from pairings where code_hash = $1 order by created_at desc limit 1 for update", codeHash).
			Scan(&vaultID, &expires, &used)
		if errors.Is(err, pgx.ErrNoRows) || (err == nil && used != nil) {
			return errNotFound
		}
		if err != nil {
			return err
		}
		if !d.PairedAt.Before(expires) {
			return errExpired
		}
		if _, err := tx.Exec(ctx, "update pairings set used_at = $3 where vault_id = $1 and code_hash = $2", vaultID, codeHash, d.PairedAt); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, "delete from devices where vault_id = $1 and role = 'phone'", vaultID); err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `insert into devices (id, vault_id, role, name, credential_hash, apns_token, paired_at, last_seen_at)
			values ($1, $2, 'phone', $3, $4, nullif($5, ''), $6, $6)`,
			d.ID, vaultID, d.Name, d.CredentialHash, d.APNSToken, d.PairedAt)
		if err != nil {
			return err
		}
		result, err = scanVault(tx.QueryRow(ctx, "select "+vaultColumns+" from vaults where id = $1", vaultID))
		return err
	})
	return result, err
}

// --- files ---

const fileColumns = "path, version, deleted, coalesce(blob_id, ''), size_bytes, coalesce(content_hash, ''), updated_by, updated_at"

func scanFile(row pgx.Row) (fileRow, error) {
	var f fileRow
	err := row.Scan(&f.Path, &f.Version, &f.Deleted, &f.BlobID, &f.SizeBytes, &f.ContentHash, &f.UpdatedBy, &f.UpdatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return fileRow{}, errNotFound
	}
	f.UpdatedAt = f.UpdatedAt.UTC()
	return f, err
}

func (s *store) fileByPath(ctx context.Context, vaultID, path string) (fileRow, error) {
	return scanFile(s.pool.QueryRow(ctx, "select "+fileColumns+" from files where vault_id = $1 and path = $2", vaultID, path))
}

// listFiles returns rows above since in version order, limit+1 of them so
// the caller can tell whether more follow. since < 0 means the full pull:
// live rows only.
func (s *store) listFiles(ctx context.Context, vaultID string, since int64, limit int) ([]fileRow, error) {
	var rows pgx.Rows
	var err error
	if since < 0 {
		rows, err = s.pool.Query(ctx, "select "+fileColumns+" from files where vault_id = $1 and not deleted order by version limit $2",
			vaultID, limit+1)
	} else {
		rows, err = s.pool.Query(ctx, "select "+fileColumns+" from files where vault_id = $1 and version > $2 order by version limit $3",
			vaultID, since, limit+1)
	}
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	files := []fileRow{}
	for rows.Next() {
		f, err := scanFile(rows)
		if err != nil {
			return nil, err
		}
		files = append(files, f)
	}
	return files, rows.Err()
}

// currentVersion is what expected_version is compared against: the row's
// version, tombstone or not, or 0 for a path the vault never had.
func currentVersion(ctx context.Context, tx pgx.Tx, vaultID, path string) (fileRow, error) {
	f, err := scanFile(tx.QueryRow(ctx, "select "+fileColumns+" from files where vault_id = $1 and path = $2", vaultID, path))
	if errors.Is(err, errNotFound) {
		return fileRow{Path: path}, nil
	}
	return f, err
}

type staleVersionError struct {
	current fileRow
}

func (e staleVersionError) Error() string { return errStaleVersion.Error() }
func (e staleVersionError) Unwrap() error { return errStaleVersion }

// beginUpload checks the version and the quota and records the pending
// upload. The hard cap is quota plus ten percent (contract §3.5).
func (s *store) beginUpload(ctx context.Context, p pendingUpload, hardCap int64) error {
	return s.withVaultLock(ctx, p.VaultID, func(tx pgx.Tx, v vault) error {
		current, err := currentVersion(ctx, tx, p.VaultID, p.Path)
		if err != nil {
			return err
		}
		if current.Version != p.ExpectedVersion {
			return staleVersionError{current: current}
		}
		live := current.SizeBytes
		if current.Deleted {
			live = 0
		}
		if v.BytesUsed-live+p.SizeBytes > hardCap {
			return errQuota
		}
		_, err = tx.Exec(ctx, `insert into pending_uploads (vault_id, blob_id, path, expected_version, size_bytes, content_hash, created_at)
			values ($1, $2, $3, $4, $5, $6, $7)
			on conflict (vault_id, blob_id) do update set path = excluded.path, expected_version = excluded.expected_version,
				size_bytes = excluded.size_bytes, content_hash = excluded.content_hash, created_at = excluded.created_at`,
			p.VaultID, p.BlobID, p.Path, p.ExpectedVersion, p.SizeBytes, p.ContentHash, p.CreatedAt)
		return err
	})
}

func (s *store) pendingUpload(ctx context.Context, vaultID, blobID string) (pendingUpload, error) {
	var p pendingUpload
	err := s.pool.QueryRow(ctx, `select vault_id, blob_id, path, expected_version, size_bytes, content_hash, created_at
		from pending_uploads where vault_id = $1 and blob_id = $2`, vaultID, blobID).
		Scan(&p.VaultID, &p.BlobID, &p.Path, &p.ExpectedVersion, &p.SizeBytes, &p.ContentHash, &p.CreatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return pendingUpload{}, errNotFound
	}
	return p, err
}

type commitResult struct {
	Version      int64
	UpdatedAt    time.Time
	ReplacedBlob string
	// True when the blob was already the file's current version: nothing
	// changed and no event should fire.
	AlreadyCommitted bool
}

// commitUpload publishes a verified blob as the path's new version. The
// caller has checked that the object exists with the declared size.
func (s *store) commitUpload(ctx context.Context, vaultID, path, blobID string, expectedVersion int64, hardCap int64, now time.Time) (commitResult, error) {
	var result commitResult
	err := s.withVaultLock(ctx, vaultID, func(tx pgx.Tx, v vault) error {
		current, err := currentVersion(ctx, tx, vaultID, path)
		if err != nil {
			return err
		}
		if !current.Deleted && current.BlobID == blobID && current.Version > 0 {
			result = commitResult{Version: current.Version, UpdatedAt: current.UpdatedAt, AlreadyCommitted: true}
			return nil
		}
		// Locked, so the sweeper's delete of stale uploads waits for this
		// commit and then skips the row, rather than removing a blob this
		// commit has just published.
		var p pendingUpload
		err = tx.QueryRow(ctx, `select size_bytes, content_hash from pending_uploads where vault_id = $1 and blob_id = $2 and path = $3 for update`,
			vaultID, blobID, path).Scan(&p.SizeBytes, &p.ContentHash)
		if errors.Is(err, pgx.ErrNoRows) {
			return errBlobMissing
		}
		if err != nil {
			return err
		}
		if current.Version != expectedVersion {
			return staleVersionError{current: current}
		}
		live := current.SizeBytes
		if current.Deleted {
			live = 0
		}
		if v.BytesUsed-live+p.SizeBytes > hardCap {
			return errQuota
		}
		version := v.LatestVersion + 1
		_, err = tx.Exec(ctx, `insert into files (vault_id, path, version, deleted, blob_id, size_bytes, content_hash, updated_by, updated_at)
			values ($1, $2, $3, false, $4, $5, $6, 'desk', $7)
			on conflict (vault_id, path) do update set version = excluded.version, deleted = false, blob_id = excluded.blob_id,
				size_bytes = excluded.size_bytes, content_hash = excluded.content_hash, updated_by = 'desk', updated_at = excluded.updated_at`,
			vaultID, path, version, blobID, p.SizeBytes, p.ContentHash, now)
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, "delete from pending_uploads where vault_id = $1 and blob_id = $2", vaultID, blobID); err != nil {
			return err
		}
		_, err = tx.Exec(ctx, "update vaults set latest_version = $2, bytes_used = bytes_used - $3 + $4 where id = $1",
			vaultID, version, live, p.SizeBytes)
		if err != nil {
			return err
		}
		result = commitResult{Version: version, UpdatedAt: now}
		if !current.Deleted {
			result.ReplacedBlob = current.BlobID
		}
		return nil
	})
	return result, err
}

type tombstoneResult struct {
	Version   int64
	FreedBlob string
	// Already a tombstone: nothing changed.
	AlreadyDeleted bool
}

func (s *store) tombstoneFile(ctx context.Context, vaultID, path string, expectedVersion int64, now time.Time) (tombstoneResult, error) {
	var result tombstoneResult
	err := s.withVaultLock(ctx, vaultID, func(tx pgx.Tx, v vault) error {
		current, err := currentVersion(ctx, tx, vaultID, path)
		if err != nil {
			return err
		}
		if current.Deleted {
			result = tombstoneResult{Version: current.Version, AlreadyDeleted: true}
			return nil
		}
		if current.Version != expectedVersion {
			return staleVersionError{current: current}
		}
		if current.Version == 0 {
			return errNotFound
		}
		version := v.LatestVersion + 1
		_, err = tx.Exec(ctx, `update files set version = $3, deleted = true, blob_id = null, size_bytes = 0, content_hash = null,
			updated_by = 'desk', updated_at = $4 where vault_id = $1 and path = $2`, vaultID, path, version, now)
		if err != nil {
			return err
		}
		_, err = tx.Exec(ctx, "update vaults set latest_version = $2, bytes_used = bytes_used - $3 where id = $1",
			vaultID, version, current.SizeBytes)
		if err != nil {
			return err
		}
		result = tombstoneResult{Version: version, FreedBlob: current.BlobID}
		return nil
	})
	return result, err
}

// --- writes ---

// enqueueWrite assigns the next seq, or returns the existing row's seq when
// the client id was seen before (retries are free). The bool is true when
// the write is new.
func (s *store) enqueueWrite(ctx context.Context, vaultID string, w writeRow) (int64, bool, error) {
	var seq int64
	created := false
	err := s.withVaultLock(ctx, vaultID, func(tx pgx.Tx, v vault) error {
		err := tx.QueryRow(ctx, "select seq from writes where vault_id = $1 and client_id = $2", vaultID, w.ClientID).Scan(&seq)
		if err == nil {
			return nil
		}
		if !errors.Is(err, pgx.ErrNoRows) {
			return err
		}
		seq = v.LatestSeq + 1
		_, err = tx.Exec(ctx, `insert into writes (vault_id, seq, client_id, path, base_version, payload, size_bytes, created_at)
			values ($1, $2, $3, $4, $5, $6, $7, $8)`,
			vaultID, seq, w.ClientID, w.Path, w.BaseVersion, w.Payload, len(w.Payload), w.CreatedAt)
		if err != nil {
			var pgErr *pgconn.PgError
			if errors.As(err, &pgErr) && pgErr.Code == "23505" {
				return fmt.Errorf("write %s raced its own retry", w.ClientID)
			}
			return err
		}
		if _, err := tx.Exec(ctx, "update vaults set latest_seq = $2 where id = $1", vaultID, seq); err != nil {
			return err
		}
		created = true
		return nil
	})
	return seq, created, err
}

// listWrites returns unacked writes above after, limit+1 of them.
func (s *store) listWrites(ctx context.Context, vaultID string, after int64, limit int) ([]writeRow, error) {
	rows, err := s.pool.Query(ctx, `select seq, client_id, path, base_version, payload, created_at from writes
		where vault_id = $1 and seq > $2 and acked_at is null order by seq limit $3`, vaultID, after, limit+1)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	writes := []writeRow{}
	for rows.Next() {
		var w writeRow
		if err := rows.Scan(&w.Seq, &w.ClientID, &w.Path, &w.BaseVersion, &w.Payload, &w.CreatedAt); err != nil {
			return nil, err
		}
		w.CreatedAt = w.CreatedAt.UTC()
		writes = append(writes, w)
	}
	return writes, rows.Err()
}

type ackResult struct {
	ThroughSeq int64
	AtVersion  int64
	// False when through was at or below the recorded ack, so nothing moved.
	Advanced bool
}

// ackWrites drops the payloads through the given seq and records where the
// vault's version counter stood at that instant, which is what lets the
// phone prune its own queue (contract §8.4).
func (s *store) ackWrites(ctx context.Context, vaultID string, through int64, now time.Time) (ackResult, error) {
	var result ackResult
	err := s.withVaultLock(ctx, vaultID, func(tx pgx.Tx, v vault) error {
		if through > v.LatestSeq {
			return errNotFound
		}
		if through <= v.AckedThroughSeq {
			result = ackResult{ThroughSeq: v.AckedThroughSeq, AtVersion: v.AckedAtVersion}
			return nil
		}
		_, err := tx.Exec(ctx, "update writes set acked_at = $3, payload = '' where vault_id = $1 and seq <= $2 and acked_at is null",
			vaultID, through, now)
		if err != nil {
			return err
		}
		_, err = tx.Exec(ctx, "update vaults set acked_through_seq = $2, acked_at_version = latest_version where id = $1", vaultID, through)
		if err != nil {
			return err
		}
		result = ackResult{ThroughSeq: through, AtVersion: v.LatestVersion, Advanced: true}
		return nil
	})
	return result, err
}

// --- sweeping ---

type sweepReport struct {
	LapsedVaults  int
	StaleUploads  int
	Tombstones    int
	AckedWrites   int
	Pairings      int
	BlobsToDelete []blobRef
}

// sweep prunes what has aged out: vaults 30 days past their lapse, uploads
// begun over an hour ago and never committed, tombstones older than 30 days
// (moving the horizon so stale cursors learn to pull afresh), acked writes
// older than a week, and spent or expired pairing codes.
func (s *store) sweep(ctx context.Context, now time.Time) (sweepReport, error) {
	var report sweepReport
	err := pgx.BeginFunc(ctx, s.pool, func(tx pgx.Tx) error {
		lapsedBefore := now.Add(-30 * 24 * time.Hour)
		rows, err := tx.Query(ctx, "select id from vaults where lapsed_at is not null and lapsed_at <= $1", lapsedBefore)
		if err != nil {
			return err
		}
		var lapsed []string
		for rows.Next() {
			var id string
			if err := rows.Scan(&id); err != nil {
				rows.Close()
				return err
			}
			lapsed = append(lapsed, id)
		}
		rows.Close()
		if err := rows.Err(); err != nil {
			return err
		}
		for _, id := range lapsed {
			blobs, err := allBlobIDs(ctx, tx, id)
			if err != nil {
				return err
			}
			for _, blob := range blobs {
				report.BlobsToDelete = append(report.BlobsToDelete, blobRef{VaultID: id, BlobID: blob})
			}
			if _, err := tx.Exec(ctx, "delete from vaults where id = $1", id); err != nil {
				return err
			}
			report.LapsedVaults++
		}

		stale, err := tx.Query(ctx, "delete from pending_uploads where created_at <= $1 returning vault_id, blob_id", now.Add(-time.Hour))
		if err != nil {
			return err
		}
		for stale.Next() {
			var ref blobRef
			if err := stale.Scan(&ref.VaultID, &ref.BlobID); err != nil {
				stale.Close()
				return err
			}
			report.BlobsToDelete = append(report.BlobsToDelete, ref)
			report.StaleUploads++
		}
		stale.Close()
		if err := stale.Err(); err != nil {
			return err
		}

		// Horizon first, then the rows: a vault whose oldest surviving
		// tombstone is above the horizon still answers cursors correctly.
		_, err = tx.Exec(ctx, `update vaults v set tombstone_horizon = greatest(v.tombstone_horizon, pruned.max_version)
			from (select vault_id, max(version) as max_version from files where deleted and updated_at <= $1 group by vault_id) pruned
			where pruned.vault_id = v.id`, lapsedBefore)
		if err != nil {
			return err
		}
		tag, err := tx.Exec(ctx, "delete from files where deleted and updated_at <= $1", lapsedBefore)
		if err != nil {
			return err
		}
		report.Tombstones = int(tag.RowsAffected())

		tag, err = tx.Exec(ctx, "delete from writes where acked_at is not null and acked_at <= $1", now.Add(-7*24*time.Hour))
		if err != nil {
			return err
		}
		report.AckedWrites = int(tag.RowsAffected())

		tag, err = tx.Exec(ctx, "delete from pairings where used_at is not null or expires_at <= $1", now)
		if err != nil {
			return err
		}
		report.Pairings = int(tag.RowsAffected())
		return nil
	})
	return report, err
}
