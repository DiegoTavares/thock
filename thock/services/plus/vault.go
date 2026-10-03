package main

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"regexp"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"golang.org/x/text/unicode/norm"
)

// Vault sync routes (contract: thock/specs/v34-vault-sync-api.md §6). The
// desk authenticates with its Plus credential, the phone with the credential
// minted at pairing; every handler receives the resolved principal.

const (
	maxPlaintextBytes   = 2 * 1024 * 1024
	envelopeOverhead    = 32
	maxBlobBytes        = maxPlaintextBytes + envelopeOverhead
	maxWriteBodyBytes   = 3 * 1024 * 1024
	maxPathBytes        = 1024
	signedURLLifetime   = 15 * time.Minute
	pairingCodeLifetime = 10 * time.Minute
	defaultFilesPage    = 500
	maxFilesPage        = 2000
	defaultWritesPage   = 200
	maxWritesPage       = 1000
	tombstoneRetention  = 30 * 24 * time.Hour
)

var (
	syncExtensions   = map[string]bool{"md": true, "txt": true, "toml": true, "json": true, "csv": true}
	excludedPrefixes = []string{".thock/history/", ".thock/cache/", ".thock/sync/", ".git/"}
	hex32            = regexp.MustCompile(`^[0-9a-f]{32}$`)
	hex64            = regexp.MustCompile(`^[0-9a-f]{64}$`)
	uuidLower        = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)
	// Contract §4.3: no I, O, 0 or 1.
	pairingAlphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
)

// validateSyncPath is contract §4.1. The server validates on every route that
// names a path so neither client has to trust the other's.
func validateSyncPath(path string) error {
	if path == "" {
		return errors.New("the path is empty")
	}
	if len(path) > maxPathBytes {
		return errors.New("the path is longer than 1024 bytes")
	}
	if !utf8.ValidString(path) || !norm.NFC.IsNormalString(path) {
		return errors.New("the path isn't normalized UTF-8")
	}
	if strings.HasPrefix(path, "/") || strings.HasSuffix(path, "/") {
		return errors.New("the path can't start or end with a slash")
	}
	for _, segment := range strings.Split(path, "/") {
		if segment == "" || segment == "." || segment == ".." {
			return errors.New("the path has an empty, '.' or '..' segment")
		}
	}
	dot := strings.LastIndexByte(path, '.')
	slash := strings.LastIndexByte(path, '/')
	if dot <= slash || !syncExtensions[strings.ToLower(path[dot+1:])] {
		return errors.New("only .md, .txt, .toml, .json and .csv files sync")
	}
	for _, prefix := range excludedPrefixes {
		if strings.HasPrefix(path, prefix) {
			return fmt.Errorf("files under %s never sync", prefix)
		}
	}
	return nil
}

type principal struct {
	user   user
	vault  *vault
	role   deviceRole
	device device
	// A desk whose Plus access was turned off. Only routes with
	// routeAccess.revokedDesk let it through.
	revoked bool
}

// Which roles a route accepts and what a lapsed vault may still do there.
type routeAccess struct {
	roles       []deviceRole
	needVault   bool
	lapsedDesk  bool
	lapsedPhone bool
	// A disconnected desk may still delete its vault copy instead of waiting
	// for the sweeper.
	revokedDesk bool
}

var (
	deskOnly         = routeAccess{roles: []deviceRole{roleDesk}, needVault: true}
	phoneOnly        = routeAccess{roles: []deviceRole{rolePhone}, needVault: true}
	bothRead         = routeAccess{roles: []deviceRole{roleDesk, rolePhone}, needVault: true, lapsedPhone: true}
	vaultStatusRoute = routeAccess{roles: []deviceRole{roleDesk, rolePhone}, needVault: true, lapsedDesk: true, lapsedPhone: true}
	vaultDeleteRoute = routeAccess{roles: []deviceRole{roleDesk}, needVault: true, lapsedDesk: true, revokedDesk: true}
	vaultCreateRoute = routeAccess{roles: []deviceRole{roleDesk}}
)

func (s *server) withVault(access routeAccess, next func(http.ResponseWriter, *http.Request, principal)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		credential, ok := bearer(r)
		if !ok {
			writeErrorCode(w, http.StatusUnauthorized, "unauthorized", "This request needs your Thock Plus credential.")
			return
		}
		p, err := s.resolvePrincipal(r.Context(), credential)
		var refusal *refusalError
		if errors.As(err, &refusal) {
			writeErrorCode(w, refusal.status, refusal.code, refusal.message)
			return
		}
		if err != nil {
			logf("error: resolving a credential: %v", err)
			writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't check your connection right now. Try again.")
			return
		}
		if p.revoked && !access.revokedDesk {
			writeRevoked(w)
			return
		}
		allowed := false
		for _, role := range access.roles {
			allowed = allowed || role == p.role
		}
		if !allowed {
			writeErrorCode(w, http.StatusForbidden, "role_forbidden", "This request is for the other device.")
			return
		}
		if access.needVault && p.vault == nil {
			writeErrorCode(w, http.StatusNotFound, "vault_missing", "There is no vault to sync yet. Turn on phone sync at your computer first.")
			return
		}
		lapsed := p.vault != nil && (p.vault.LapsedAt != nil || p.user.Status == userRevoked)
		if lapsed && ((p.role == roleDesk && !access.lapsedDesk) || (p.role == rolePhone && !access.lapsedPhone)) {
			writeErrorCode(w, http.StatusForbidden, "plus_lapsed", "Your Thock Plus subscription ended, so the phone can only look. Renew to keep writing from it.")
			return
		}
		if p.vault != nil {
			if err := s.store.touchDevice(r.Context(), p.vault.ID, p.role, s.now()); err != nil {
				logf("warning: recording a device's last call: %v", err)
			}
		}
		next(w, r, p)
	}
}

type refusalError struct {
	status  int
	code    string
	message string
}

func (e *refusalError) Error() string { return e.message }

func refuse(status int, code, message string) error {
	return &refusalError{status: status, code: code, message: message}
}

func writeRevoked(w http.ResponseWriter) {
	writeErrorCode(w, http.StatusForbidden, "revoked", "Your Thock Plus access was turned off. Your own agent still works from the Agent panel.")
}

// resolvePrincipal maps a credential to who is calling. A revoked desk
// resolves with principal.revoked set; callers must refuse it unless the
// route allows it.
func (s *server) resolvePrincipal(ctx context.Context, credential string) (principal, error) {
	hash := hashCredential(credential)
	if strings.HasPrefix(credential, "tpp_") {
		d, err := s.store.deviceByCredential(ctx, hash)
		if errors.Is(err, errNotFound) {
			return principal{}, refuse(http.StatusUnauthorized, "unauthorized", "This phone isn't connected anymore. Connect it again from your computer.")
		}
		if err != nil {
			return principal{}, err
		}
		v, err := s.store.vaultByID(ctx, d.VaultID)
		if err != nil {
			return principal{}, err
		}
		u, err := s.store.userByID(ctx, v.UserID)
		if err != nil {
			return principal{}, err
		}
		return principal{user: u, vault: &v, role: rolePhone, device: d}, nil
	}
	u, err := s.store.userByCredential(ctx, hash)
	if errors.Is(err, errNotFound) {
		return principal{}, refuse(http.StatusUnauthorized, "unauthorized", "That Thock Plus connection is no longer valid. Connect again with an invite code.")
	}
	if err != nil {
		return principal{}, err
	}
	p := principal{user: u, role: roleDesk, revoked: u.Status == userRevoked}
	v, err := s.store.vaultByUser(ctx, u.ID)
	if err == nil {
		p.vault = &v
	} else if !errors.Is(err, errNotFound) {
		return principal{}, err
	}
	return p, nil
}

// --- the vault object ---

type vaultResponse struct {
	VaultID       string           `json:"vault_id"`
	Status        string           `json:"status"`
	KeyCheck      string           `json:"key_check"`
	CreatedAt     time.Time        `json:"created_at"`
	LapsedAt      *time.Time       `json:"lapsed_at"`
	QuotaBytes    int64            `json:"quota_bytes"`
	UsedBytes     int64            `json:"used_bytes"`
	FileCount     int64            `json:"file_count"`
	LatestVersion int64            `json:"latest_version"`
	Writes        writesSummary    `json:"writes"`
	Devices       []deviceResponse `json:"devices"`
}

type writesSummary struct {
	Pending         int64 `json:"pending"`
	LatestSeq       int64 `json:"latest_seq"`
	AckedThroughSeq int64 `json:"acked_through_seq"`
	AckedAtVersion  int64 `json:"acked_at_version"`
}

type deviceResponse struct {
	DeviceID   string     `json:"device_id"`
	Role       deviceRole `json:"role"`
	Name       string     `json:"name"`
	PairedAt   time.Time  `json:"paired_at"`
	LastSeenAt time.Time  `json:"last_seen_at"`
}

func toDeviceResponse(d device) deviceResponse {
	return deviceResponse{DeviceID: d.ID, Role: d.Role, Name: d.Name, PairedAt: d.PairedAt, LastSeenAt: d.LastSeenAt}
}

func (s *server) vaultResponse(ctx context.Context, v vault, quota int64) (vaultResponse, error) {
	counts, err := s.store.vaultCounts(ctx, v.ID)
	if err != nil {
		return vaultResponse{}, err
	}
	devices, err := s.store.listDevices(ctx, v.ID)
	if err != nil {
		return vaultResponse{}, err
	}
	status := "active"
	if v.LapsedAt != nil {
		status = "lapsed"
	}
	response := vaultResponse{
		VaultID:       v.ID,
		Status:        status,
		KeyCheck:      v.KeyCheck,
		CreatedAt:     v.CreatedAt,
		LapsedAt:      v.LapsedAt,
		QuotaBytes:    quota,
		UsedBytes:     v.BytesUsed,
		FileCount:     counts.Files,
		LatestVersion: v.LatestVersion,
		Writes: writesSummary{
			Pending:         counts.PendingWrites,
			LatestSeq:       v.LatestSeq,
			AckedThroughSeq: v.AckedThroughSeq,
			AckedAtVersion:  v.AckedAtVersion,
		},
		Devices: []deviceResponse{},
	}
	for _, d := range devices {
		response.Devices = append(response.Devices, toDeviceResponse(d))
	}
	return response, nil
}

// quotaFor reads the plan's vault quota; a plan without one has no vault.
func (s *server) quotaFor(ctx context.Context, u user) (int64, error) {
	p, _, err := s.store.plan(ctx, u.Plan)
	if err != nil {
		return 0, err
	}
	return p.Limits.VaultQuotaBytes, nil
}

func hardCap(quota int64) int64 {
	return quota + quota/10
}

func (s *server) writeVault(w http.ResponseWriter, r *http.Request, p principal, status int) {
	quota, err := s.quotaFor(r.Context(), p.user)
	if err != nil {
		logf("error: reading plan %s: %v", p.user.Plan, err)
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Your plan isn't configured right now. Try again later.")
		return
	}
	v, err := s.store.vaultByID(r.Context(), p.vault.ID)
	if err != nil {
		logf("error: re-reading vault %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the vault. Try again.")
		return
	}
	response, err := s.vaultResponse(r.Context(), v, quota)
	if err != nil {
		logf("error: building the vault response for %s: %v", v.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the vault. Try again.")
		return
	}
	writeJSON(w, status, response)
}

// --- §6.1 vault ---

type vaultCreateRequest struct {
	DeviceName string `json:"device_name"`
	KeyCheck   string `json:"key_check"`
}

func (s *server) handleVaultCreate(w http.ResponseWriter, r *http.Request, p principal) {
	var request vaultCreateRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	if !hex32.MatchString(request.KeyCheck) {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "key_check must be 32 hex characters.")
		return
	}
	quota, err := s.quotaFor(r.Context(), p.user)
	if err != nil {
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Your plan isn't configured right now. Try again later.")
		return
	}
	if quota == 0 {
		writeErrorCode(w, http.StatusForbidden, "plan_excludes_sync", "Your Thock Plus plan doesn't include phone sync.")
		return
	}
	v, err := s.store.ensureVault(r.Context(), p.user.ID, request.KeyCheck, strings.TrimSpace(request.DeviceName), s.now())
	if errors.Is(err, errKeyMismatch) {
		writeErrorCode(w, http.StatusConflict, "key_mismatch", "This computer holds a different key than the one your vault was set up with. Reset phone sync to start over.")
		return
	}
	if err != nil {
		logf("error: ensuring a vault for %s: %v", p.user.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't set up the vault. Try again.")
		return
	}
	p.vault = &v
	s.writeVault(w, r, p, http.StatusOK)
}

func (s *server) handleVaultGet(w http.ResponseWriter, r *http.Request, p principal) {
	s.writeVault(w, r, p, http.StatusOK)
}

func (s *server) handleVaultDelete(w http.ResponseWriter, r *http.Request, p principal) {
	blobs, err := s.store.deleteVault(r.Context(), p.vault.ID)
	if err != nil && !errors.Is(err, errNotFound) {
		logf("error: deleting vault %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't delete the vault copy. Try again.")
		return
	}
	s.dropPhoneKey(p.user.ID)
	s.feed.publish(p.vault.ID, feedEvent{Kind: "vault", Data: map[string]any{"status": "deleted"}})
	s.deleteBlobs(p.vault.ID, blobs)
	w.WriteHeader(http.StatusNoContent)
}

type vaultResetRequest struct {
	KeyCheck string `json:"key_check"`
}

func (s *server) handleVaultReset(w http.ResponseWriter, r *http.Request, p principal) {
	var request vaultResetRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	if !hex32.MatchString(request.KeyCheck) {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "key_check must be 32 hex characters.")
		return
	}
	phone, phoneErr := s.store.phoneDevice(r.Context(), p.vault.ID)
	blobs, err := s.store.resetVault(r.Context(), p.vault.ID, request.KeyCheck)
	if err != nil {
		logf("error: resetting vault %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't reset phone sync. Try again.")
		return
	}
	s.dropPhoneKey(p.user.ID)
	if phoneErr == nil {
		s.feed.publish(p.vault.ID, feedEvent{Kind: "device", Data: map[string]any{"device_id": phone.ID, "revoked": true}})
	}
	s.deleteBlobs(p.vault.ID, blobs)
	s.writeVault(w, r, p, http.StatusOK)
}

// deleteBlobs is best effort: a blob that outlives its row costs storage,
// not correctness, and the sweeper can't see it, so failures are logged.
func (s *server) deleteBlobs(vaultID string, blobs []string) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	for _, id := range blobs {
		if err := s.blobs.delete(ctx, vaultID, id); err != nil {
			logf("warning: deleting blob %s/%s: %v", vaultID, id, err)
		}
	}
}

// --- §6.2 pairing and devices ---

func pairingCode() (string, error) {
	buffer := make([]byte, 8)
	if _, err := rand.Read(buffer); err != nil {
		return "", err
	}
	var code strings.Builder
	for i, b := range buffer {
		if i == 4 {
			code.WriteByte('-')
		}
		code.WriteByte(pairingAlphabet[int(b)%len(pairingAlphabet)])
	}
	return code.String(), nil
}

func normalizePairingCode(code string) string {
	code = strings.ToUpper(strings.TrimSpace(code))
	return strings.NewReplacer("-", "", " ", "").Replace(code)
}

func (s *server) handlePairingCreate(w http.ResponseWriter, r *http.Request, p principal) {
	code, err := pairingCode()
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't mint a pairing code. Try again.")
		return
	}
	now := s.now()
	expires := now.Add(pairingCodeLifetime)
	if err := s.store.createPairing(r.Context(), p.vault.ID, hashCredential(normalizePairingCode(code)), now, expires); err != nil {
		logf("error: saving a pairing code: %v", err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't save the pairing code. Try again.")
		return
	}
	writeJSON(w, http.StatusCreated, map[string]any{"code": code, "expires_at": expires})
}

type pairRequest struct {
	Code       string  `json:"code"`
	DeviceName string  `json:"device_name"`
	Platform   string  `json:"platform"`
	APNSToken  *string `json:"apns_token"`
}

func (s *server) handlePair(w http.ResponseWriter, r *http.Request) {
	var request pairRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	code := normalizePairingCode(request.Code)
	if len(code) != 8 {
		writeErrorCode(w, http.StatusNotFound, "pairing_invalid", "That code didn't work. Show a fresh one at your computer and scan again.")
		return
	}
	deviceID, err := randomToken(8)
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't connect the phone. Try again.")
		return
	}
	secret, err := randomToken(24)
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't connect the phone. Try again.")
		return
	}
	credential := "tpp_" + secret
	d := device{
		ID:             deviceID,
		Role:           rolePhone,
		Name:           strings.TrimSpace(request.DeviceName),
		CredentialHash: hashCredential(credential),
		PairedAt:       s.now(),
	}
	if request.APNSToken != nil {
		d.APNSToken = strings.TrimSpace(*request.APNSToken)
	}
	v, err := s.store.redeemPairing(r.Context(), hashCredential(code), d)
	switch {
	case errors.Is(err, errNotFound):
		writeErrorCode(w, http.StatusNotFound, "pairing_invalid", "That code didn't work. Show a fresh one at your computer and scan again.")
		return
	case errors.Is(err, errExpired):
		writeErrorCode(w, http.StatusGone, "pairing_expired", "That code has expired. Show a fresh one at your computer and scan again.")
		return
	case err != nil:
		logf("error: redeeming a pairing code: %v", err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't connect the phone. Try again.")
		return
	}
	// Whatever phone was paired before is gone now, and its key goes with it
	// before the new phone holds a credential to ask for one.
	s.dropPhoneKey(v.UserID)
	u, err := s.store.userByID(r.Context(), v.UserID)
	if err != nil {
		logf("error: reading the vault owner: %v", err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't connect the phone. Try again.")
		return
	}
	quota, err := s.quotaFor(r.Context(), u)
	if err != nil {
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "The plan isn't configured right now. Try again later.")
		return
	}
	response, err := s.vaultResponse(r.Context(), v, quota)
	if err != nil {
		logf("error: building the vault response: %v", err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't connect the phone. Try again.")
		return
	}
	s.feed.publish(v.ID, feedEvent{Kind: "device", From: rolePhone, Data: map[string]any{"device_id": d.ID, "revoked": false}})
	writeJSON(w, http.StatusCreated, map[string]any{
		"credential": credential,
		"device":     map[string]any{"device_id": d.ID, "role": rolePhone, "name": d.Name},
		"vault":      response,
	})
}

func (s *server) handleDevicesList(w http.ResponseWriter, r *http.Request, p principal) {
	devices, err := s.store.listDevices(r.Context(), p.vault.ID)
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the devices. Try again.")
		return
	}
	response := []deviceResponse{}
	for _, d := range devices {
		response = append(response, toDeviceResponse(d))
	}
	writeJSON(w, http.StatusOK, map[string]any{"devices": response})
}

type devicePatchRequest struct {
	DeviceName *string `json:"device_name"`
	// A JSON null clears the token, so the field distinguishes absent
	// from null through the raw message.
	APNSToken json.RawMessage `json:"apns_token"`
}

func (s *server) handleDevicePatch(w http.ResponseWriter, r *http.Request, p principal) {
	var request devicePatchRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	patch := devicePatch{}
	if request.DeviceName != nil {
		name := strings.TrimSpace(*request.DeviceName)
		patch.Name = &name
	}
	if len(request.APNSToken) > 0 {
		var token *string
		if err := json.Unmarshal(request.APNSToken, &token); err != nil {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "apns_token must be a string or null.")
			return
		}
		value := ""
		if token != nil {
			value = strings.TrimSpace(*token)
		}
		patch.APNSToken = &value
	}
	if err := s.store.patchDevice(r.Context(), p.device.ID, patch); err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't update the phone. Try again.")
		return
	}
	d, err := s.store.deviceByID(r.Context(), p.vault.ID, p.device.ID)
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't re-read the phone. Try again.")
		return
	}
	writeJSON(w, http.StatusOK, toDeviceResponse(d))
}

func (s *server) handleDeviceRevoke(w http.ResponseWriter, r *http.Request, p principal) {
	id := r.PathValue("device_id")
	d, err := s.store.deviceByID(r.Context(), p.vault.ID, id)
	if errors.Is(err, errNotFound) {
		writeErrorCode(w, http.StatusNotFound, "not_found", "There is no such device.")
		return
	}
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the device. Try again.")
		return
	}
	if d.Role == roleDesk {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The computer can't be disconnected from itself. Turn phone sync off instead.")
		return
	}
	if err := s.store.deleteDevice(r.Context(), p.vault.ID, id); err != nil && !errors.Is(err, errNotFound) {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't disconnect the phone. Try again.")
		return
	}
	s.dropPhoneKey(p.user.ID)
	s.feed.publish(p.vault.ID, feedEvent{Kind: "device", From: roleDesk, Data: map[string]any{"device_id": id, "revoked": true}})
	w.WriteHeader(http.StatusNoContent)
}

// --- the agent grant (thock/specs/v35-phone-ask.md §5.1) ---

// What the phone's Ask loop runs on: the balance, and the phone's own key.
// Never the desk key.
type agentGrantResponse struct {
	Status         string            `json:"status"`
	AllowanceUnits int64             `json:"allowance_units"`
	UsedUnits      int64             `json:"used_units"`
	RemainingUnits int64             `json:"remaining_units"`
	WarnAtPercent  int               `json:"warn_at_percent"`
	CycleEndsAt    time.Time         `json:"cycle_ends_at"`
	Gateway        agentGrantGateway `json:"gateway"`
}

type agentGrantGateway struct {
	Provider string     `json:"provider"`
	BaseURL  string     `json:"base_url"`
	APIKey   string     `json:"api_key"`
	Models   modelTiers `json:"models"`
}

func (s *server) handleAgentGrant(w http.ResponseWriter, r *http.Request, p principal) {
	credential, _ := bearer(r)
	s.syncMu.Lock()
	defer s.syncMu.Unlock()
	// The phone may have been disconnected, or the vault lapsed, between the
	// check that let this request in and the lock. Those paths revoke the
	// phone key under the same lock, so asking again here is what stops a
	// request already in flight from minting a key nobody will revoke.
	current, err := s.resolvePrincipal(r.Context(), credential)
	var refusal *refusalError
	if errors.As(err, &refusal) {
		writeErrorCode(w, refusal.status, refusal.code, refusal.message)
		return
	}
	if err != nil {
		logf("error: re-checking a phone credential for its grant: %v", err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't check your connection right now. Try again.")
		return
	}
	if current.vault == nil || current.vault.LapsedAt != nil || current.user.Status == userRevoked {
		writeErrorCode(w, http.StatusForbidden, "plus_lapsed", "Your Thock Plus subscription ended, so the phone can only look. Renew to ask from it again.")
		return
	}
	u := current.user
	if u.PhoneGateway.Hash == "" {
		u, err = s.mintPhoneKey(r.Context(), u)
		if errors.Is(err, errPlanMissing) {
			writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Your plan isn't configured right now. Try again later.")
			return
		}
		if err != nil {
			logf("error: minting a phone key for %s: %v", p.user.ID, err)
			writeErrorCode(w, http.StatusBadGateway, "upstream", "Couldn't set up your agent's access right now. Try again in a minute.")
			return
		}
	}
	entitlement, err := s.allowanceLoop(r.Context(), u)
	if errors.Is(err, errPlanMissing) {
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Your plan isn't configured right now. Try again later.")
		return
	}
	if err != nil {
		logf("error: the phone's grant for %s: %v", u.ID, err)
		writeErrorCode(w, http.StatusBadGateway, "upstream", "Couldn't read your balance right now. Try again in a minute.")
		return
	}
	writeJSON(w, http.StatusOK, agentGrantResponse{
		Status:         entitlement.Status,
		AllowanceUnits: entitlement.AllowanceUnits,
		UsedUnits:      entitlement.UsedUnits,
		RemainingUnits: entitlement.RemainingUnits,
		WarnAtPercent:  entitlement.WarnAtPercent,
		CycleEndsAt:    entitlement.CycleEndsAt,
		Gateway: agentGrantGateway{
			Provider: "openrouter",
			BaseURL:  gatewayBaseURL,
			APIKey:   u.PhoneGateway.Secret,
			Models:   entitlement.Gateway.Models,
		},
	})
}

// --- §6.3 files ---

type fileResponse struct {
	Path              string     `json:"path"`
	Version           int64      `json:"version"`
	Deleted           bool       `json:"deleted"`
	BlobID            string     `json:"blob_id,omitempty"`
	SizeBytes         int64      `json:"size_bytes,omitempty"`
	ContentHash       string     `json:"content_hash,omitempty"`
	UpdatedBy         string     `json:"updated_by"`
	UpdatedAt         time.Time  `json:"updated_at"`
	DownloadURL       string     `json:"download_url,omitempty"`
	DownloadExpiresAt *time.Time `json:"download_expires_at,omitempty"`
}

func (s *server) handleFilesList(w http.ResponseWriter, r *http.Request, p principal) {
	since := int64(-1)
	if raw := r.URL.Query().Get("since"); raw != "" {
		parsed, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || parsed < 0 {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "since must be a version number.")
			return
		}
		since = parsed
	}
	limit, ok := pageLimit(w, r, defaultFilesPage, maxFilesPage)
	if !ok {
		return
	}
	if since >= 0 && since < p.vault.TombstoneHorizon {
		writeErrorCode(w, http.StatusGone, "cursor_expired", "That position is too old to continue from. Fetch the whole list again.")
		return
	}
	rows, err := s.store.listFiles(r.Context(), p.vault.ID, since, limit)
	if err != nil {
		logf("error: listing files for %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the file list. Try again.")
		return
	}
	hasMore := len(rows) > limit
	if hasMore {
		rows = rows[:limit]
	}
	expires := s.now().Add(signedURLLifetime)
	files := make([]fileResponse, 0, len(rows))
	nextSince := since
	if since < 0 {
		nextSince = 0
	}
	for _, row := range rows {
		item := fileResponse{Path: row.Path, Version: row.Version, Deleted: row.Deleted, UpdatedBy: row.UpdatedBy, UpdatedAt: row.UpdatedAt}
		if !row.Deleted {
			item.BlobID = row.BlobID
			item.SizeBytes = row.SizeBytes
			item.ContentHash = row.ContentHash
			url, err := s.blobs.downloadURL(r.Context(), p.vault.ID, row.BlobID, expires)
			if err != nil {
				logf("error: signing a download for %s/%s: %v", p.vault.ID, row.Path, err)
				writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Couldn't prepare the downloads. Try again.")
				return
			}
			item.DownloadURL = url
			item.DownloadExpiresAt = &expires
		}
		files = append(files, item)
		nextSince = row.Version
	}
	if !hasMore && nextSince < p.vault.LatestVersion {
		// Every row above the cursor is in this page, so whatever versions
		// remain belong to pruned tombstones, rewritten paths or (on a full
		// pull) tombstones left out on purpose: the cursor may jump to the end.
		nextSince = p.vault.LatestVersion
	}
	writeJSON(w, http.StatusOK, map[string]any{"files": files, "next_since": nextSince, "has_more": hasMore})
}

func pageLimit(w http.ResponseWriter, r *http.Request, fallback, ceiling int) (int, bool) {
	raw := r.URL.Query().Get("limit")
	if raw == "" {
		return fallback, true
	}
	limit, err := strconv.Atoi(raw)
	if err != nil || limit <= 0 {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "limit must be a positive number.")
		return 0, false
	}
	return min(limit, ceiling), true
}

type beginUploadRequest struct {
	ExpectedVersion int64  `json:"expected_version"`
	BlobID          string `json:"blob_id"`
	SizeBytes       int64  `json:"size_bytes"`
	ContentHash     string `json:"content_hash"`
}

type commitRequest struct {
	ExpectedVersion int64  `json:"expected_version"`
	BlobID          string `json:"blob_id"`
}

// handleFilePost is begin-upload, or commit when the wildcard ends in
// /commit (no syncable path can end that way, see validateSyncPath).
func (s *server) handleFilePost(w http.ResponseWriter, r *http.Request, p principal) {
	path := r.PathValue("path")
	if trimmed, isCommit := strings.CutSuffix(path, "/commit"); isCommit {
		s.handleFileCommit(w, r, p, trimmed)
		return
	}
	if err := validateSyncPath(path); err != nil {
		writeErrorCode(w, http.StatusUnprocessableEntity, "path_not_allowed", "That file can't sync: "+err.Error()+".")
		return
	}
	var request beginUploadRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	if !hex32.MatchString(request.BlobID) || !hex64.MatchString(request.ContentHash) || request.ExpectedVersion < 0 || request.SizeBytes <= 0 {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "blob_id, content_hash, size_bytes and expected_version are required and must be well formed.")
		return
	}
	if request.SizeBytes > maxBlobBytes {
		writeErrorCode(w, http.StatusRequestEntityTooLarge, "too_large", "Files over 2 MB don't sync.")
		return
	}
	quota, err := s.quotaFor(r.Context(), p.user)
	if err != nil {
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Your plan isn't configured right now. Try again later.")
		return
	}
	now := s.now()
	pending := pendingUpload{
		VaultID: p.vault.ID, BlobID: request.BlobID, Path: path, ExpectedVersion: request.ExpectedVersion,
		SizeBytes: request.SizeBytes, ContentHash: request.ContentHash, CreatedAt: now,
	}
	err = s.store.beginUpload(r.Context(), pending, hardCap(quota))
	if s.writeVersionError(w, err, "Couldn't start the upload. Try again.") {
		return
	}
	upload, err := s.blobs.uploadURL(r.Context(), p.vault.ID, request.BlobID, request.SizeBytes, now.Add(signedURLLifetime))
	if err != nil {
		logf("error: signing an upload: %v", err)
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Couldn't prepare the upload. Try again.")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"upload": upload})
}

// writeVersionError answers the store errors begin, commit and delete share.
// Returns true when it wrote a response.
func (s *server) writeVersionError(w http.ResponseWriter, err error, internal string) bool {
	var stale staleVersionError
	switch {
	case err == nil:
		return false
	case errors.As(err, &stale):
		current := map[string]any{"version": stale.current.Version, "deleted": stale.current.Deleted}
		if !stale.current.Deleted && stale.current.Version > 0 {
			current["blob_id"] = stale.current.BlobID
			current["content_hash"] = stale.current.ContentHash
		}
		writeJSON(w, http.StatusConflict, map[string]any{
			"error":   "That file changed since this computer last saw it.",
			"code":    "stale_version",
			"current": current,
		})
	case errors.Is(err, errQuota):
		writeErrorCode(w, http.StatusInsufficientStorage, "quota_exceeded", "Your vault is over its storage allowance. Make room before more can sync.")
	case errors.Is(err, errBlobMissing):
		writeErrorCode(w, http.StatusUnprocessableEntity, "blob_missing", "The upload didn't arrive, or its size differs from what was announced. Start it again.")
	case errors.Is(err, errNotFound):
		writeErrorCode(w, http.StatusNotFound, "not_found", "There is no such file.")
	default:
		logf("error: %s: %v", internal, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", internal)
	}
	return true
}

func (s *server) handleFileCommit(w http.ResponseWriter, r *http.Request, p principal, path string) {
	if err := validateSyncPath(path); err != nil {
		writeErrorCode(w, http.StatusUnprocessableEntity, "path_not_allowed", "That file can't sync: "+err.Error()+".")
		return
	}
	var request commitRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	if !hex32.MatchString(request.BlobID) || request.ExpectedVersion < 0 {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "blob_id and expected_version are required and must be well formed.")
		return
	}
	pending, err := s.store.pendingUpload(r.Context(), p.vault.ID, request.BlobID)
	if errors.Is(err, errNotFound) {
		// Perhaps it already committed: the store answers idempotently.
		current, currentErr := s.store.fileByPath(r.Context(), p.vault.ID, path)
		if currentErr == nil && !current.Deleted && current.BlobID == request.BlobID {
			writeJSON(w, http.StatusOK, map[string]any{"version": current.Version, "updated_at": current.UpdatedAt})
			return
		}
		writeErrorCode(w, http.StatusUnprocessableEntity, "blob_missing", "The upload didn't arrive, or its size differs from what was announced. Start it again.")
		return
	}
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't check the upload. Try again.")
		return
	}
	size, err := s.blobs.stat(r.Context(), p.vault.ID, request.BlobID)
	if errors.Is(err, errNotFound) || (err == nil && size != pending.SizeBytes) {
		writeErrorCode(w, http.StatusUnprocessableEntity, "blob_missing", "The upload didn't arrive, or its size differs from what was announced. Start it again.")
		return
	}
	if err != nil {
		logf("error: checking blob %s/%s: %v", p.vault.ID, request.BlobID, err)
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Couldn't check the upload. Try again.")
		return
	}
	quota, err := s.quotaFor(r.Context(), p.user)
	if err != nil {
		writeErrorCode(w, http.StatusServiceUnavailable, "unavailable", "Your plan isn't configured right now. Try again later.")
		return
	}
	result, err := s.store.commitUpload(r.Context(), p.vault.ID, path, request.BlobID, request.ExpectedVersion, hardCap(quota), s.now())
	if s.writeVersionError(w, err, "Couldn't publish the upload. Try again.") {
		return
	}
	if !result.AlreadyCommitted {
		s.feed.publish(p.vault.ID, feedEvent{Kind: "file", From: roleDesk, Data: map[string]any{"path": path, "version": result.Version, "deleted": false}})
		if result.ReplacedBlob != "" {
			s.deleteBlobs(p.vault.ID, []string{result.ReplacedBlob})
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"version": result.Version, "updated_at": result.UpdatedAt})
}

type deleteFileRequest struct {
	ExpectedVersion int64 `json:"expected_version"`
}

func (s *server) handleFileDelete(w http.ResponseWriter, r *http.Request, p principal) {
	path := r.PathValue("path")
	if err := validateSyncPath(path); err != nil {
		writeErrorCode(w, http.StatusUnprocessableEntity, "path_not_allowed", "That file can't sync: "+err.Error()+".")
		return
	}
	var request deleteFileRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	result, err := s.store.tombstoneFile(r.Context(), p.vault.ID, path, request.ExpectedVersion, s.now())
	if s.writeVersionError(w, err, "Couldn't record the deletion. Try again.") {
		return
	}
	if !result.AlreadyDeleted {
		s.feed.publish(p.vault.ID, feedEvent{Kind: "file", From: roleDesk, Data: map[string]any{"path": path, "version": result.Version, "deleted": true}})
		if result.FreedBlob != "" {
			s.deleteBlobs(p.vault.ID, []string{result.FreedBlob})
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"version": result.Version})
}

// --- §6.4 writes ---

type writeRequest struct {
	ClientID    string `json:"client_id"`
	Path        string `json:"path"`
	BaseVersion int64  `json:"base_version"`
	Payload     string `json:"payload"`
}

func (s *server) handleWriteCreate(w http.ResponseWriter, r *http.Request, p principal) {
	var request writeRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxWriteBodyBytes)).Decode(&request); err != nil {
		var tooLarge *http.MaxBytesError
		if errors.As(err, &tooLarge) {
			writeErrorCode(w, http.StatusRequestEntityTooLarge, "too_large", "That write is too large to send.")
			return
		}
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	if !uuidLower.MatchString(request.ClientID) {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "client_id must be a lower-case UUID.")
		return
	}
	if request.BaseVersion < 0 {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "base_version can't be negative.")
		return
	}
	if err := validateSyncPath(request.Path); err != nil {
		writeErrorCode(w, http.StatusUnprocessableEntity, "path_not_allowed", "That file can't sync: "+err.Error()+".")
		return
	}
	payload, err := base64.StdEncoding.DecodeString(request.Payload)
	if err != nil || len(payload) == 0 {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "payload must be base64.")
		return
	}
	if len(payload) > maxBlobBytes {
		writeErrorCode(w, http.StatusRequestEntityTooLarge, "too_large", "That write is too large to send.")
		return
	}
	now := s.now()
	seq, created, err := s.store.enqueueWrite(r.Context(), p.vault.ID, writeRow{
		ClientID: request.ClientID, Path: request.Path, BaseVersion: request.BaseVersion, Payload: payload, CreatedAt: now,
	})
	if err != nil {
		logf("error: queuing a write for %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't save that change. It stays on your phone and will be sent again.")
		return
	}
	status := http.StatusOK
	if created {
		status = http.StatusCreated
		s.feed.publish(p.vault.ID, feedEvent{Kind: "write", From: rolePhone, Data: map[string]any{"seq": seq, "path": request.Path}})
	}
	writeJSON(w, status, map[string]any{"seq": seq, "created_at": now})
}

type writeResponse struct {
	Seq         int64     `json:"seq"`
	ClientID    string    `json:"client_id"`
	Path        string    `json:"path"`
	BaseVersion int64     `json:"base_version"`
	Payload     string    `json:"payload"`
	CreatedAt   time.Time `json:"created_at"`
}

func (s *server) handleWritesList(w http.ResponseWriter, r *http.Request, p principal) {
	after := p.vault.AckedThroughSeq
	if raw := r.URL.Query().Get("after"); raw != "" {
		parsed, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || parsed < 0 {
			writeErrorCode(w, http.StatusBadRequest, "bad_request", "after must be a seq number.")
			return
		}
		after = parsed
	}
	limit, ok := pageLimit(w, r, defaultWritesPage, maxWritesPage)
	if !ok {
		return
	}
	rows, err := s.store.listWrites(r.Context(), p.vault.ID, after, limit)
	if err != nil {
		logf("error: listing writes for %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the changes from your phone. Try again.")
		return
	}
	hasMore := len(rows) > limit
	if hasMore {
		rows = rows[:limit]
	}
	writes := make([]writeResponse, 0, len(rows))
	for _, row := range rows {
		writes = append(writes, writeResponse{
			Seq: row.Seq, ClientID: row.ClientID, Path: row.Path, BaseVersion: row.BaseVersion,
			Payload: base64.StdEncoding.EncodeToString(row.Payload), CreatedAt: row.CreatedAt,
		})
	}
	writeJSON(w, http.StatusOK, map[string]any{"writes": writes, "has_more": hasMore})
}

type ackRequest struct {
	ThroughSeq int64 `json:"through_seq"`
}

func (s *server) handleWritesAck(w http.ResponseWriter, r *http.Request, p principal) {
	var request ackRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	if request.ThroughSeq < 0 {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "through_seq can't be negative.")
		return
	}
	result, err := s.store.ackWrites(r.Context(), p.vault.ID, request.ThroughSeq, s.now())
	if errors.Is(err, errNotFound) {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "through_seq is beyond the last change the phone sent.")
		return
	}
	if err != nil {
		logf("error: acking writes for %s: %v", p.vault.ID, err)
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't record the changes as applied. Try again.")
		return
	}
	if result.Advanced {
		s.feed.publish(p.vault.ID, feedEvent{Kind: "ack", From: roleDesk, Data: map[string]any{"through_seq": result.ThroughSeq, "at_version": result.AtVersion}})
	}
	writeJSON(w, http.StatusOK, map[string]any{"through_seq": result.ThroughSeq, "at_version": result.AtVersion})
}

// --- §6.5 feed and push ---

func (s *server) handleFeed(w http.ResponseWriter, r *http.Request, p principal) {
	credential, _ := bearer(r)
	vaultID := p.vault.ID
	s.feed.serve(w, r, vaultID, p.role, func(ctx context.Context) bool {
		current, err := s.resolvePrincipal(ctx, credential)
		var refusal *refusalError
		if errors.As(err, &refusal) {
			return false
		}
		if err != nil {
			// A database hiccup is not a revocation; the next ping asks again.
			logf("warning: re-checking a feed credential: %v", err)
			return true
		}
		return !current.revoked && current.vault != nil && current.vault.ID == vaultID && current.role == p.role
	})
}

// sendPush is what the coalescer calls once per burst: one silent push with
// the counters the phone needs to decide what to pull.
func (s *server) sendPush(vaultID string) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if s.feed.phoneListening(vaultID) {
		return
	}
	v, err := s.store.vaultByID(ctx, vaultID)
	if err != nil {
		return
	}
	phone, err := s.store.phoneDevice(ctx, vaultID)
	if err != nil || phone.APNSToken == "" {
		return
	}
	payload, err := json.Marshal(map[string]any{
		"aps":   map[string]any{"content-available": 1},
		"thock": map[string]any{"vault_id": v.ID, "latest_version": v.LatestVersion, "acked_through_seq": v.AckedThroughSeq},
	})
	if err != nil {
		return
	}
	if err := s.pusher.push(ctx, phone.APNSToken, payload); err != nil {
		logf("warning: pushing to the phone of vault %s: %v", vaultID, err)
	}
}

// --- §6.6 entitlement ---

type entitlementVault struct {
	QuotaBytes int64      `json:"quota_bytes"`
	UsedBytes  int64      `json:"used_bytes"`
	Status     string     `json:"status"`
	LapsedAt   *time.Time `json:"lapsed_at"`
}

func (s *server) entitlementVault(ctx context.Context, u user, quota int64) *entitlementVault {
	v, err := s.store.vaultByUser(ctx, u.ID)
	if err != nil {
		if !errors.Is(err, errNotFound) {
			logf("warning: reading the vault for %s: %v", u.ID, err)
		}
		return nil
	}
	status := "active"
	if v.LapsedAt != nil {
		status = "lapsed"
	}
	return &entitlementVault{QuotaBytes: quota, UsedBytes: v.BytesUsed, Status: status, LapsedAt: v.LapsedAt}
}

// --- admin ---

type lapseRequest struct {
	Lapsed bool `json:"lapsed"`
}

// handleAdminLapse sets or clears a vault's lapse by hand: what a billing
// driver does on cancel and renew, reachable for tests and support.
func (s *server) handleAdminLapse(w http.ResponseWriter, r *http.Request) {
	var request lapseRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<16)).Decode(&request); err != nil {
		writeErrorCode(w, http.StatusBadRequest, "bad_request", "The request wasn't readable.")
		return
	}
	v, err := s.store.vaultByUser(r.Context(), r.PathValue("id"))
	if errors.Is(err, errNotFound) {
		writeErrorCode(w, http.StatusNotFound, "not_found", "That user has no vault.")
		return
	}
	if err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't read the vault: "+err.Error())
		return
	}
	var at *time.Time
	if request.Lapsed {
		now := s.now()
		at = &now
	}
	if err := s.store.setVaultLapsed(r.Context(), v.ID, at); err != nil {
		writeErrorCode(w, http.StatusInternalServerError, "internal", "Couldn't update the vault: "+err.Error())
		return
	}
	if request.Lapsed {
		// The phone keeps its credential through a lapse but may no longer
		// ask; a renewal's first grant mints a fresh key.
		s.dropPhoneKey(v.UserID)
	}
	if request.Lapsed && v.LapsedAt == nil {
		s.feed.publish(v.ID, feedEvent{Kind: "vault", Data: map[string]any{"status": "lapsed"}})
	}
	w.WriteHeader(http.StatusNoContent)
}

// runSweeper prunes aged rows on an interval until ctx ends.
func (s *server) runSweeper(ctx context.Context, every time.Duration) {
	ticker := time.NewTicker(every)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.sweepOnce(ctx)
		}
	}
}

func (s *server) sweepOnce(ctx context.Context) sweepReport {
	report, err := s.store.sweep(ctx, s.now())
	if err != nil {
		logf("error: sweeping: %v", err)
		return report
	}
	for _, ref := range report.BlobsToDelete {
		if err := s.blobs.delete(ctx, ref.VaultID, ref.BlobID); err != nil {
			logf("warning: deleting swept blob %s/%s: %v", ref.VaultID, ref.BlobID, err)
		}
	}
	if report.LapsedVaults+report.StaleUploads+report.Tombstones+report.AckedWrites > 0 {
		logf("swept %d lapsed vaults, %d stale uploads, %d tombstones, %d acked writes", report.LapsedVaults, report.StaleUploads, report.Tombstones, report.AckedWrites)
	}
	return report
}
