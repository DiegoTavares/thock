package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"errors"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

type apnsRequest struct {
	Proto   int
	Path    string
	Header  http.Header
	Payload []byte
}

// fakeAPNs is an HTTP/2 TLS server that records each request and answers
// with whatever respond returns for it.
type fakeAPNs struct {
	server  *httptest.Server
	mu      sync.Mutex
	seen    []apnsRequest
	respond func(index int, request apnsRequest) (int, string)
}

func newFakeAPNs(t *testing.T, respond func(index int, request apnsRequest) (int, string)) *fakeAPNs {
	t.Helper()
	fake := &fakeAPNs{respond: respond}
	fake.server = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		payload, err := io.ReadAll(r.Body)
		if err != nil {
			t.Errorf("reading the push body: %v", err)
		}
		request := apnsRequest{Proto: r.ProtoMajor, Path: r.URL.Path, Header: r.Header.Clone(), Payload: payload}
		fake.mu.Lock()
		index := len(fake.seen)
		fake.seen = append(fake.seen, request)
		fake.mu.Unlock()
		status, reason := http.StatusOK, ""
		if fake.respond != nil {
			status, reason = fake.respond(index, request)
		}
		w.Header().Set("apns-id", "00000000-0000-0000-0000-000000000000")
		w.WriteHeader(status)
		if reason != "" {
			if err := json.NewEncoder(w).Encode(map[string]string{"reason": reason}); err != nil {
				t.Errorf("answering the push: %v", err)
			}
		}
	}))
	fake.server.EnableHTTP2 = true
	fake.server.StartTLS()
	t.Cleanup(fake.server.Close)
	return fake
}

func (f *fakeAPNs) requests() []apnsRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]apnsRequest(nil), f.seen...)
}

func testAPNSKey(t *testing.T) (*ecdsa.PrivateKey, string) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	der, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		t.Fatal(err)
	}
	return key, string(pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der}))
}

func newTestAPNSPusher(t *testing.T, fake *fakeAPNs) (*apnsPusher, *ecdsa.PrivateKey) {
	t.Helper()
	key, keyPEM := testAPNSKey(t)
	pusher, err := newAPNSPusher(apnsConfig{
		KeyPEM:  keyPEM,
		KeyID:   "ABC123DEFG",
		TeamID:  apnsDefaultTeamID,
		Topic:   apnsDefaultTopic,
		BaseURL: fake.server.URL,
	}, fake.server.Client())
	if err != nil {
		t.Fatal(err)
	}
	return pusher, key
}

// verifyProviderToken checks the bearer's ES256 signature against the key
// and returns its header and claims.
func verifyProviderToken(t *testing.T, authorization string, key *ecdsa.PrivateKey) (map[string]any, map[string]any) {
	t.Helper()
	token, ok := strings.CutPrefix(authorization, "bearer ")
	if !ok {
		t.Fatalf("authorization should be a bearer token: %q", authorization)
	}
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		t.Fatalf("a JWT has three parts: %q", token)
	}
	decode := func(part string) []byte {
		data, err := base64.RawURLEncoding.DecodeString(part)
		if err != nil {
			t.Fatalf("JWT part isn't base64url: %v", err)
		}
		return data
	}
	signature := decode(parts[2])
	if len(signature) != 64 {
		t.Fatalf("an ES256 signature is 64 bytes, got %d", len(signature))
	}
	digest := sha256.Sum256([]byte(parts[0] + "." + parts[1]))
	r, s := new(big.Int).SetBytes(signature[:32]), new(big.Int).SetBytes(signature[32:])
	if !ecdsa.Verify(&key.PublicKey, digest[:], r, s) {
		t.Fatal("the provider token's signature doesn't verify")
	}
	var header, claims map[string]any
	if err := json.Unmarshal(decode(parts[0]), &header); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(decode(parts[1]), &claims); err != nil {
		t.Fatal(err)
	}
	return header, claims
}

func TestAPNSSendsASilentPushOverHTTP2(t *testing.T) {
	fake := newFakeAPNs(t, nil)
	pusher, key := newTestAPNSPusher(t, fake)
	clock := time.Date(2026, 10, 21, 14, 0, 0, 0, time.UTC)
	pusher.now = func() time.Time { return clock }
	payload := []byte(`{"aps":{"content-available":1},"thock":{"vault_id":"v1","latest_version":3,"acked_through_seq":1}}`)
	if err := pusher.push(context.Background(), "a1b2c3", payload); err != nil {
		t.Fatalf("push: %v", err)
	}
	requests := fake.requests()
	if len(requests) != 1 {
		t.Fatalf("one request: %d", len(requests))
	}
	request := requests[0]
	if request.Proto != 2 {
		t.Fatalf("APNs needs HTTP/2, got HTTP/%d", request.Proto)
	}
	if request.Path != "/3/device/a1b2c3" {
		t.Fatalf("path: %s", request.Path)
	}
	for name, want := range map[string]string{
		"apns-topic":     "com.thethock.ios",
		"apns-push-type": "background",
		"apns-priority":  "5",
		"Content-Type":   "application/json",
	} {
		if got := request.Header.Get(name); got != want {
			t.Fatalf("%s: %q, want %q", name, got, want)
		}
	}
	if string(request.Payload) != string(payload) {
		t.Fatalf("payload: %s", request.Payload)
	}
	header, claims := verifyProviderToken(t, request.Header.Get("Authorization"), key)
	if header["alg"] != "ES256" || header["kid"] != "ABC123DEFG" {
		t.Fatalf("JWT header: %v", header)
	}
	if claims["iss"] != "KY338LSPPB" || claims["iat"].(float64) != float64(clock.Unix()) {
		t.Fatalf("JWT claims: %v", claims)
	}
}

func TestAPNSReusesTheProviderTokenUntilItAges(t *testing.T) {
	fake := newFakeAPNs(t, nil)
	pusher, _ := newTestAPNSPusher(t, fake)
	clock := time.Date(2026, 10, 21, 14, 0, 0, 0, time.UTC)
	pusher.now = func() time.Time { return clock }
	for _, step := range []time.Duration{0, 10 * time.Minute, 35 * time.Minute} {
		clock = clock.Add(step)
		if err := pusher.push(context.Background(), "aa", []byte(`{}`)); err != nil {
			t.Fatal(err)
		}
	}
	requests := fake.requests()
	first, second, third := requests[0].Header.Get("Authorization"), requests[1].Header.Get("Authorization"), requests[2].Header.Get("Authorization")
	if first != second {
		t.Fatal("a token ten minutes old should be reused")
	}
	if third == second {
		t.Fatal("a token forty-five minutes old should be replaced")
	}
}

func TestAPNSRetriesOnceWithAFreshTokenWhenItExpired(t *testing.T) {
	fake := newFakeAPNs(t, func(index int, _ apnsRequest) (int, string) {
		if index == 0 {
			return http.StatusForbidden, "ExpiredProviderToken"
		}
		return http.StatusOK, ""
	})
	pusher, _ := newTestAPNSPusher(t, fake)
	clock := time.Date(2026, 10, 21, 14, 0, 0, 0, time.UTC)
	pusher.now = func() time.Time { clock = clock.Add(time.Second); return clock }
	if err := pusher.push(context.Background(), "aa", []byte(`{}`)); err != nil {
		t.Fatalf("the retry should succeed: %v", err)
	}
	requests := fake.requests()
	if len(requests) != 2 || requests[0].Header.Get("Authorization") == requests[1].Header.Get("Authorization") {
		t.Fatalf("one retry with a new token: %d requests", len(requests))
	}
}

func TestAPNSRefusalsThatForgetTheToken(t *testing.T) {
	cases := []struct {
		status int
		reason string
		gone   bool
	}{
		{http.StatusGone, "Unregistered", true},
		{http.StatusGone, "", true},
		{http.StatusBadRequest, "BadDeviceToken", true},
		{http.StatusBadRequest, "DeviceTokenNotForTopic", false},
		{http.StatusForbidden, "InvalidProviderToken", false},
		{http.StatusTooManyRequests, "TooManyRequests", false},
		{http.StatusInternalServerError, "", false},
	}
	for _, c := range cases {
		fake := newFakeAPNs(t, func(int, apnsRequest) (int, string) { return c.status, c.reason })
		pusher, _ := newTestAPNSPusher(t, fake)
		err := pusher.push(context.Background(), "aa", []byte(`{}`))
		if err == nil {
			t.Fatalf("%d %s should fail", c.status, c.reason)
		}
		if errors.Is(err, errPushTokenGone) != c.gone {
			t.Fatalf("%d %s: gone should be %v, got %v", c.status, c.reason, c.gone, err)
		}
	}
}

func TestAPNSRefusesATokenThatIsNotHex(t *testing.T) {
	fake := newFakeAPNs(t, nil)
	pusher, _ := newTestAPNSPusher(t, fake)
	for _, token := range []string{"", "../../admin", "zz"} {
		if err := pusher.push(context.Background(), token, []byte(`{}`)); !errors.Is(err, errPushTokenGone) {
			t.Fatalf("%q should be refused as gone: %v", token, err)
		}
	}
	if len(fake.requests()) != 0 {
		t.Fatal("nothing should reach APNs")
	}
}

func TestAPNSConfigFromEnv(t *testing.T) {
	_, keyPEM := testAPNSKey(t)
	env := func(values map[string]string) func(string) string {
		return func(name string) string { return values[name] }
	}
	config, err := apnsConfigFromEnv(env(nil))
	if err != nil || config != nil {
		t.Fatalf("no key keeps the logging pusher: %v %v", config, err)
	}
	if _, err := apnsConfigFromEnv(env(map[string]string{"APNS_KEY": keyPEM})); err == nil {
		t.Fatal("a key without its id should be refused")
	}
	config, err = apnsConfigFromEnv(env(map[string]string{"APNS_KEY": keyPEM, "APNS_KEY_ID": "ABC123DEFG"}))
	if err != nil {
		t.Fatal(err)
	}
	if config.TeamID != "KY338LSPPB" || config.Topic != "com.thethock.ios" || config.BaseURL != apnsProductionURL {
		t.Fatalf("defaults: %+v", config)
	}
	config, err = apnsConfigFromEnv(env(map[string]string{"APNS_KEY": keyPEM, "APNS_KEY_ID": "ABC123DEFG", "APNS_ENVIRONMENT": "sandbox"}))
	if err != nil || config.BaseURL != apnsSandboxURL {
		t.Fatalf("sandbox: %+v %v", config, err)
	}
	if _, err := apnsConfigFromEnv(env(map[string]string{"APNS_KEY": keyPEM, "APNS_KEY_ID": "ABC123DEFG", "APNS_ENVIRONMENT": "staging"})); err == nil {
		t.Fatal("an unknown environment should be refused")
	}
	if _, err := newAPNSPusher(apnsConfig{KeyPEM: "not a key", KeyID: "x"}, nil); err == nil {
		t.Fatal("a key that isn't PEM should be refused")
	}
}

// The phone registers a token through PATCH /v1/vault/devices/me, a desk
// change pushes to it, and APNs answering 410 makes the server forget it.
func TestPushToARegisteredTokenAndForgetItWhenGone(t *testing.T) {
	h := newSyncHarness(t)
	gone := false
	var goneMu sync.Mutex
	fake := newFakeAPNs(t, func(int, apnsRequest) (int, string) {
		goneMu.Lock()
		defer goneMu.Unlock()
		if gone {
			return http.StatusGone, "Unregistered"
		}
		return http.StatusOK, ""
	})
	pusher, _ := newTestAPNSPusher(t, fake)
	h.server.pusher = pusher

	status, body := h.call("PATCH", "/v1/vault/devices/me", h.phone, map[string]any{"apns_token": "bb22"})
	if status != 200 {
		t.Fatalf("register a token: %d %v", status, body)
	}
	h.mustUpload("a.md", 0, "a")
	waitFor(t, func() bool { return len(fake.requests()) == 1 })
	request := fake.requests()[0]
	if request.Path != "/3/device/bb22" {
		t.Fatalf("pushed to %s", request.Path)
	}
	var payload map[string]any
	if err := json.Unmarshal(request.Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload["thock"].(map[string]any)["vault_id"] != h.vault {
		t.Fatalf("payload: %s", request.Payload)
	}
	if phone := h.phoneDevice(); phone.APNSToken != "bb22" {
		t.Fatalf("an accepted push keeps the token: %q", phone.APNSToken)
	}

	goneMu.Lock()
	gone = true
	goneMu.Unlock()
	h.clock = h.clock.Add(time.Minute)
	h.mustUpload("b.md", 0, "b")
	waitFor(t, func() bool { return h.phoneDevice().APNSToken == "" })
	if len(fake.requests()) != 2 {
		t.Fatalf("two pushes: %d", len(fake.requests()))
	}
}

func TestForgettingATokenKeepsANewerOne(t *testing.T) {
	h := newSyncHarness(t)
	status, body := h.call("PATCH", "/v1/vault/devices/me", h.phone, map[string]any{"apns_token": "cc33"})
	if status != 200 {
		t.Fatalf("register a token: %d %v", status, body)
	}
	phone := h.phoneDevice()
	if err := h.server.store.forgetAPNSToken(context.Background(), phone.ID, "aa11"); err != nil {
		t.Fatal(err)
	}
	if phone := h.phoneDevice(); phone.APNSToken != "cc33" {
		t.Fatalf("a stale failure cleared the new token: %q", phone.APNSToken)
	}
}

func (h *syncHarness) phoneDevice() device {
	h.t.Helper()
	phone, err := h.server.store.phoneDevice(context.Background(), h.vault)
	if err != nil {
		h.t.Fatal(err)
	}
	return phone
}

func waitFor(t *testing.T, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			t.Fatal("timed out waiting")
		}
		time.Sleep(10 * time.Millisecond)
	}
}
