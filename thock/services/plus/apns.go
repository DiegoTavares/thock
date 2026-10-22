package main

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"
)

// APNs with a token-based key (a .p8 from the Apple developer account):
// every request carries an ES256 JWT naming the key and the team, and the
// topic is the app's bundle id. The pushes are silent: they wake the phone
// to sync and show nothing.

const (
	apnsProductionURL = "https://api.push.apple.com"
	apnsSandboxURL    = "https://api.sandbox.push.apple.com"
	// The app's bundle id (thock/ios, PRODUCT_BUNDLE_IDENTIFIER).
	apnsDefaultTopic  = "com.thethock.ios"
	apnsDefaultTeamID = "KY338LSPPB"
	// APNs refuses a provider token older than an hour and one replaced more
	// often than every twenty minutes, so one is reused for this long.
	apnsTokenLifetime = 40 * time.Minute
)

// errPushTokenGone means APNs will never deliver to this device token again
// (the app was removed, or the token is not one of this app's), so the
// server forgets it until the phone registers a new one.
var errPushTokenGone = errors.New("the device token is no longer valid")

type apnsConfig struct {
	// The .p8 file's contents, PEM-encoded PKCS #8.
	KeyPEM string
	KeyID  string
	TeamID string
	Topic  string
	// apnsProductionURL or apnsSandboxURL; a test server in tests.
	BaseURL string
}

// apnsConfigFromEnv reads APNS_KEY, APNS_KEY_ID, APNS_TEAM_ID, APNS_TOPIC and
// APNS_ENVIRONMENT. It returns nil when APNS_KEY is unset, which keeps the
// logging pusher.
func apnsConfigFromEnv(getenv func(string) string) (*apnsConfig, error) {
	keyPEM := getenv("APNS_KEY")
	if keyPEM == "" {
		return nil, nil
	}
	config := &apnsConfig{
		KeyPEM: keyPEM,
		KeyID:  strings.TrimSpace(getenv("APNS_KEY_ID")),
		TeamID: strings.TrimSpace(getenv("APNS_TEAM_ID")),
		Topic:  strings.TrimSpace(getenv("APNS_TOPIC")),
	}
	if config.KeyID == "" {
		return nil, errors.New("APNS_KEY is set but APNS_KEY_ID is not")
	}
	if config.TeamID == "" {
		config.TeamID = apnsDefaultTeamID
	}
	if config.Topic == "" {
		config.Topic = apnsDefaultTopic
	}
	// TestFlight and App Store builds register production tokens; only a
	// build run from Xcode registers sandbox ones.
	switch environment := getenv("APNS_ENVIRONMENT"); environment {
	case "", "production":
		config.BaseURL = apnsProductionURL
	case "sandbox", "development":
		config.BaseURL = apnsSandboxURL
	default:
		return nil, fmt.Errorf("APNS_ENVIRONMENT=%q isn't supported; use production or sandbox", environment)
	}
	return config, nil
}

type apnsPusher struct {
	config apnsConfig
	key    *ecdsa.PrivateKey
	client *http.Client
	now    func() time.Time

	mu          sync.Mutex
	bearer      string
	bearerIssue time.Time
}

func newAPNSPusher(config apnsConfig, client *http.Client) (*apnsPusher, error) {
	key, err := parseAPNSKey(config.KeyPEM)
	if err != nil {
		return nil, err
	}
	if client == nil {
		// The default transport negotiates HTTP/2 over TLS, which APNs requires.
		client = &http.Client{Transport: http.DefaultTransport.(*http.Transport).Clone()}
	}
	return &apnsPusher{
		config: config,
		key:    key,
		client: client,
		now:    time.Now,
	}, nil
}

func parseAPNSKey(keyPEM string) (*ecdsa.PrivateKey, error) {
	block, _ := pem.Decode([]byte(keyPEM))
	if block == nil {
		return nil, errors.New("APNS_KEY isn't a PEM key (paste the whole .p8 file)")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, fmt.Errorf("APNS_KEY: %w", err)
	}
	key, ok := parsed.(*ecdsa.PrivateKey)
	if !ok || key.Curve != elliptic.P256() {
		return nil, errors.New("APNS_KEY must be a P-256 key, as Apple issues them")
	}
	return key, nil
}

// providerToken returns the cached JWT, minting a new one when it has aged
// past apnsTokenLifetime or when refresh is set (APNs said it expired).
func (p *apnsPusher) providerToken(refresh bool) (string, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	now := p.now()
	if !refresh && p.bearer != "" && now.Sub(p.bearerIssue) < apnsTokenLifetime {
		return p.bearer, nil
	}
	header, err := json.Marshal(map[string]string{"alg": "ES256", "kid": p.config.KeyID})
	if err != nil {
		return "", err
	}
	claims, err := json.Marshal(map[string]any{"iss": p.config.TeamID, "iat": now.Unix()})
	if err != nil {
		return "", err
	}
	encoding := base64.RawURLEncoding
	signingInput := encoding.EncodeToString(header) + "." + encoding.EncodeToString(claims)
	digest := sha256.Sum256([]byte(signingInput))
	r, s, err := ecdsa.Sign(rand.Reader, p.key, digest[:])
	if err != nil {
		return "", fmt.Errorf("signing the APNs provider token: %w", err)
	}
	// JWS wants the raw 64-byte r || s, not the ASN.1 form.
	signature := make([]byte, 64)
	r.FillBytes(signature[:32])
	s.FillBytes(signature[32:])
	p.bearer = signingInput + "." + encoding.EncodeToString(signature)
	p.bearerIssue = now
	return p.bearer, nil
}

func (p *apnsPusher) push(ctx context.Context, apnsToken string, payload []byte) error {
	if _, err := hex.DecodeString(apnsToken); err != nil || apnsToken == "" {
		return fmt.Errorf("%w: not a hex token", errPushTokenGone)
	}
	reason, err := p.send(ctx, apnsToken, payload, false)
	if err == nil && reason == "ExpiredProviderToken" {
		reason, err = p.send(ctx, apnsToken, payload, true)
	}
	if err != nil {
		return err
	}
	switch reason {
	case "":
		return nil
	case "Unregistered", "BadDeviceToken":
		return fmt.Errorf("%w: %s", errPushTokenGone, reason)
	default:
		return fmt.Errorf("APNs refused the push: %s", reason)
	}
}

// send makes one request and returns APNs's reason when it refused it, or
// an empty reason when it accepted it.
func (p *apnsPusher) send(ctx context.Context, apnsToken string, payload []byte, refreshToken bool) (string, error) {
	bearer, err := p.providerToken(refreshToken)
	if err != nil {
		return "", err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, p.config.BaseURL+"/3/device/"+apnsToken, bytes.NewReader(payload))
	if err != nil {
		return "", err
	}
	request.Header.Set("Authorization", "bearer "+bearer)
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("apns-topic", p.config.Topic)
	request.Header.Set("apns-push-type", "background")
	// Apple requires priority 5 for a background push; 10 is refused.
	request.Header.Set("apns-priority", "5")
	response, err := p.client.Do(request)
	if err != nil {
		return "", fmt.Errorf("reaching APNs: %w", err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<16))
	if err != nil {
		return "", fmt.Errorf("reading the APNs response: %w", err)
	}
	if response.StatusCode == http.StatusOK {
		return "", nil
	}
	// 410 means the token is gone whatever the body says.
	if response.StatusCode == http.StatusGone {
		return "Unregistered", nil
	}
	var refusal struct {
		Reason string `json:"reason"`
	}
	if err := json.Unmarshal(body, &refusal); err != nil || refusal.Reason == "" {
		return fmt.Sprintf("status %d", response.StatusCode), nil
	}
	return refusal.Reason, nil
}
