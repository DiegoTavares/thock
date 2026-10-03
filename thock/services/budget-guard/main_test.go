package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
)

func TestParseBudgets(t *testing.T) {
	budgets, err := parseBudgets(" b1=thock-505921 , b2=reader@3.5 ")
	if err != nil {
		t.Fatal(err)
	}
	if budgets["b1"] != (budgetTarget{project: "thock-505921", ratio: defaultRatio}) {
		t.Errorf("b1 = %+v", budgets["b1"])
	}
	if budgets["b2"] != (budgetTarget{project: "reader", ratio: 3.5}) {
		t.Errorf("b2 = %+v", budgets["b2"])
	}

	for _, bad := range []string{"", "b1", "=p", "b1=", "b1=p@x", "b1=p@0.5"} {
		if _, err := parseBudgets(bad); err == nil {
			t.Errorf("parseBudgets(%q) accepted", bad)
		}
	}
}

func TestBillingPeriod(t *testing.T) {
	if got := billingPeriod("2026-10-01T07:00:00Z"); got != "2026-10" {
		t.Errorf("got %q", got)
	}
	if got := billingPeriod("not a time"); got != "" {
		t.Errorf("got %q", got)
	}
}

// The shape Cloud Billing publishes (schema version 1.0).
const notificationFixture = `{
  "budgetDisplayName": "thock project CAD 20 monthly",
  "alertThresholdExceeded": 2.0,
  "costAmount": %COST%,
  "costIntervalStart": "2026-10-01T07:00:00Z",
  "budgetAmount": 20.0,
  "budgetAmountType": "SPECIFIED_AMOUNT",
  "currencyCode": "CAD"
}`

func push(t *testing.T, s *server, budgetID, cost string) {
	t.Helper()
	data := strings.Replace(notificationFixture, "%COST%", cost, 1)
	envelope := map[string]any{
		"message": map[string]any{
			"data":       base64.StdEncoding.EncodeToString([]byte(data)),
			"attributes": map[string]string{"budgetId": budgetID, "billingAccountId": "01C412", "schemaVersion": "1.0"},
			"messageId":  "1",
		},
		"subscription": "projects/p/subscriptions/budget-guard",
	}
	body, err := json.Marshal(envelope)
	if err != nil {
		t.Fatal(err)
	}
	pushRaw(t, s, string(body))
}

func pushRaw(t *testing.T, s *server, body string) {
	t.Helper()
	recorder := httptest.NewRecorder()
	s.routes().ServeHTTP(recorder, httptest.NewRequest(http.MethodPost, "/pubsub", strings.NewReader(body)))
	if recorder.Code != http.StatusNoContent {
		t.Fatalf("status = %d, want 204 so Pub/Sub acks", recorder.Code)
	}
}

func newTestServer() (*server, *fakeCloud) {
	f := releasesFixture()
	return &server{guard: &guard{cloud: f}, budgets: map[string]budgetTarget{"thock": {project: "p", ratio: 2}}}, f
}

func TestPushBelowRatioDoesNothing(t *testing.T) {
	s, f := newTestServer()
	push(t, s, "thock", "39.99")
	if f.writes != 0 {
		t.Errorf("%d writes below the ratio", f.writes)
	}
}

func TestPushAtRatioTrips(t *testing.T) {
	s, f := newTestServer()
	push(t, s, "thock", "40.00")
	if f.serviceByName["site"].Ingress != ingressInternal {
		t.Error("site not restricted at 2.0x")
	}
}

func TestPushIgnoresUnknownBudgetsAndBadInput(t *testing.T) {
	s, f := newTestServer()
	push(t, s, "someone-else", "1000")
	pushRaw(t, s, "not json")
	pushRaw(t, s, `{"message":{"data":"!!","attributes":{"budgetId":"thock"}}}`)
	pushRaw(t, s, `{"message":{"data":"`+base64.StdEncoding.EncodeToString([]byte("{"))+`","attributes":{"budgetId":"thock"}}}`)
	if f.writes != 0 {
		t.Errorf("%d writes", f.writes)
	}
}

func TestPushIgnoresZeroBudget(t *testing.T) {
	s, f := newTestServer()
	data := strings.Replace(strings.Replace(notificationFixture, "%COST%", "5", 1), `"budgetAmount": 20.0`, `"budgetAmount": 0`, 1)
	pushRaw(t, s, `{"message":{"data":"`+base64.StdEncoding.EncodeToString([]byte(data))+`","attributes":{"budgetId":"thock"}}}`)
	if f.writes != 0 {
		t.Errorf("%d writes", f.writes)
	}
}

func TestPushRejectsGet(t *testing.T) {
	s, _ := newTestServer()
	recorder := httptest.NewRecorder()
	s.routes().ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/pubsub", nil))
	if recorder.Code != http.StatusMethodNotAllowed {
		t.Errorf("status = %d", recorder.Code)
	}
}

type recordedRequest struct {
	method, path, query, body string
}

// The REST client against a stand-in for the Google APIs: paths, the update
// mask, pagination, and label removal as JSON null.
func TestRestCloudRequests(t *testing.T) {
	var requests []recordedRequest
	api := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		requests = append(requests, recordedRequest{r.Method, r.URL.Path, r.URL.RawQuery, string(body)})
		switch {
		case r.URL.Path == "/run/projects/p/locations/-/services" && r.URL.Query().Get("pageToken") == "":
			io.WriteString(w, `{"services":[{"name":"projects/p/locations/us-central1/services/site","ingress":"INGRESS_TRAFFIC_ALL","labels":{"a":"b"}}],"nextPageToken":"next"}`)
		case r.URL.Path == "/run/projects/p/locations/-/services":
			io.WriteString(w, `{"services":[{"name":"projects/p/locations/us-east4/services/sync","ingress":"INGRESS_TRAFFIC_INTERNAL_ONLY"}]}`)
		case r.URL.Path == "/storage/b" && r.Method == http.MethodGet:
			io.WriteString(w, `{"items":[{"name":"releases"}]}`)
		case r.URL.Path == "/storage/b/releases/iam" && r.Method == http.MethodGet:
			io.WriteString(w, `{"version":1,"etag":"CAY=","bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}`)
		case r.URL.Path == "/storage/b/does-not-exist/iam":
			w.WriteHeader(http.StatusNotFound)
			io.WriteString(w, `{"error":{"message":"No such bucket"}}`)
		default:
			io.WriteString(w, `{}`)
		}
	}))
	defer api.Close()
	c := &restCloud{client: api.Client(), runBase: api.URL + "/run", storageBase: api.URL + "/storage"}
	ctx := context.Background()

	services, err := c.services(ctx, "p")
	if err != nil {
		t.Fatal(err)
	}
	if len(services) != 2 || services[0].Labels["a"] != "b" || services[1].Ingress != ingressInternal {
		t.Errorf("services = %+v", services)
	}

	requests = nil
	if err := c.updateService(ctx, service{Name: "projects/p/locations/us-central1/services/site", Ingress: ingressInternal, Labels: map[string]string{labelGuard: valueTripped}}); err != nil {
		t.Fatal(err)
	}
	update := requests[0]
	if update.method != http.MethodPatch || update.path != "/run/projects/p/locations/us-central1/services/site" || update.query != "updateMask=ingress%2Clabels" {
		t.Errorf("update = %+v", update)
	}
	if !strings.Contains(update.body, `"ingress":"INGRESS_TRAFFIC_INTERNAL_ONLY"`) || strings.Contains(update.body, "template") {
		t.Errorf("update body = %s", update.body)
	}

	buckets, err := c.buckets(ctx, "p")
	if err != nil || len(buckets) != 1 || buckets[0].Name != "releases" {
		t.Fatalf("buckets = %+v, %v", buckets, err)
	}
	p, err := c.bucketPolicy(ctx, "releases")
	if err != nil || p.Etag != "CAY=" || !slices.Equal(p.Bindings[0].Members, []string{"allUsers"}) {
		t.Fatalf("policy = %+v, %v", p, err)
	}
	if _, err := c.bucketPolicy(ctx, "does-not-exist"); err == nil || !strings.Contains(err.Error(), "No such bucket") {
		t.Errorf("err = %v, want the API's message", err)
	}

	requests = nil
	if err := c.setBucketPolicy(ctx, "releases", policy{Version: 1, Etag: "CAY="}); err != nil {
		t.Fatal(err)
	}
	if requests[0].method != http.MethodPut || !strings.Contains(requests[0].body, `"etag":"CAY="`) || !strings.Contains(requests[0].body, `"bindings":[]`) {
		t.Errorf("set policy = %+v", requests[0])
	}

	requests = nil
	if err := c.setBucketLabels(ctx, "releases", map[string]*string{labelGuard: nil, labelRestored: ptr("2026-10")}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(requests[0].body, `"budget-guard":null`) || !strings.Contains(requests[0].body, `"budget-guard-restored":"2026-10"`) {
		t.Errorf("labels body = %s", requests[0].body)
	}
}
