package main

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"
)

type fakeCloud struct {
	serviceByName   map[string]service
	bucketByName    map[string]bucket
	policies        map[string]policy
	failService     string
	writes          int
	serviceListFail bool
}

func newFakeCloud() *fakeCloud {
	return &fakeCloud{serviceByName: map[string]service{}, bucketByName: map[string]bucket{}, policies: map[string]policy{}}
}

func (f *fakeCloud) addService(name, ingress string, labels map[string]string) {
	f.serviceByName[name] = service{Name: name, Ingress: ingress, Labels: labels}
}

func (f *fakeCloud) addBucket(name string, labels map[string]string, bindings ...binding) {
	f.bucketByName[name] = bucket{Name: name, Labels: labels}
	f.policies[name] = policy{Version: 1, Etag: "CAY=", Bindings: bindings}
}

func (f *fakeCloud) services(_ context.Context, _ string) ([]service, error) {
	if f.serviceListFail {
		return nil, errors.New("run API down")
	}
	var list []service
	for _, s := range f.serviceByName {
		list = append(list, s)
	}
	slices.SortFunc(list, func(a, b service) int { return strings.Compare(a.Name, b.Name) })
	return list, nil
}

func (f *fakeCloud) updateService(_ context.Context, s service) error {
	if s.Name == f.failService {
		return errors.New("permission denied")
	}
	f.writes++
	f.serviceByName[s.Name] = s
	return nil
}

func (f *fakeCloud) buckets(_ context.Context, _ string) ([]bucket, error) {
	var list []bucket
	for _, b := range f.bucketByName {
		list = append(list, b)
	}
	slices.SortFunc(list, func(a, b bucket) int { return strings.Compare(a.Name, b.Name) })
	return list, nil
}

func (f *fakeCloud) bucketPolicy(_ context.Context, name string) (policy, error) {
	return f.policies[name], nil
}

func (f *fakeCloud) setBucketPolicy(_ context.Context, name string, p policy) error {
	f.writes++
	f.policies[name] = p
	return nil
}

func (f *fakeCloud) setBucketLabels(_ context.Context, name string, labels map[string]*string) error {
	f.writes++
	b := f.bucketByName[name]
	b.Labels = withLabels(b.Labels, nil)
	for key, value := range labels {
		if value == nil {
			delete(b.Labels, key)
		} else {
			b.Labels[key] = *value
		}
	}
	f.bucketByName[name] = b
	return nil
}

func (f *fakeCloud) members(bucketName, role string) []string {
	for _, b := range f.policies[bucketName].Bindings {
		if b.Role == role {
			return b.Members
		}
	}
	return nil
}

const (
	viewer = "roles/storage.objectViewer"
	admin  = "roles/storage.objectAdmin"
	reader = "serviceAccount:releases-api@p.iam.gserviceaccount.com"
)

func releasesFixture() *fakeCloud {
	f := newFakeCloud()
	f.addService("site", ingressAll, nil)
	f.addService("api", ingressAll, map[string]string{"team": "thock"})
	f.addBucket("releases", nil,
		binding{Role: viewer, Members: []string{"allUsers", reader}},
		binding{Role: admin, Members: []string{"serviceAccount:publisher@p.iam.gserviceaccount.com"}})
	f.addBucket("blobs", nil, binding{Role: admin, Members: []string{"serviceAccount:plus@p.iam.gserviceaccount.com"}})
	return f
}

func TestTripRestrictsServicesAndPublicBuckets(t *testing.T) {
	f := releasesFixture()
	g := &guard{cloud: f}

	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}

	for _, name := range []string{"site", "api"} {
		s := f.serviceByName[name]
		if s.Ingress != ingressInternal {
			t.Errorf("%s ingress = %s, want internal", name, s.Ingress)
		}
		if s.Labels[labelGuard] != valueTripped || s.Labels[labelIngress] != "ingress_traffic_all" {
			t.Errorf("%s labels = %v", name, s.Labels)
		}
	}
	if f.serviceByName["api"].Labels["team"] != "thock" {
		t.Error("existing service labels were dropped")
	}
	if got := f.members("releases", viewer); !slices.Equal(got, []string{reader}) {
		t.Errorf("releases viewers = %v, want only the service account", got)
	}
	if got := f.members("releases", admin); len(got) != 1 {
		t.Errorf("unrelated binding changed: %v", got)
	}
	labels := f.bucketByName["releases"].Labels
	if labels[labelGuard] != valueTripped || labels[labelPublicPrefix+"0"] != "allusers_objectviewer" {
		t.Errorf("releases labels = %v", labels)
	}
	if len(f.bucketByName["blobs"].Labels) != 0 {
		t.Errorf("private bucket was labelled: %v", f.bucketByName["blobs"].Labels)
	}
}

func TestTripDropsBindingsLeftEmpty(t *testing.T) {
	f := newFakeCloud()
	f.addBucket("site-assets", nil, binding{Role: viewer, Members: []string{"allUsers", "allAuthenticatedUsers"}})
	g := &guard{cloud: f}

	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if bindings := f.policies["site-assets"].Bindings; len(bindings) != 0 {
		t.Errorf("bindings = %v, want none", bindings)
	}
	labels := f.bucketByName["site-assets"].Labels
	if labels[labelPublicPrefix+"0"] != "allusers_objectviewer" || labels[labelPublicPrefix+"1"] != "allauthenticatedusers_objectviewer" {
		t.Errorf("labels = %v", labels)
	}
}

func TestTripSkipsExemptInternalAndRestoredThisPeriod(t *testing.T) {
	f := newFakeCloud()
	f.addService("guard", ingressAll, map[string]string{labelGuard: valueExempt})
	f.addService("worker", ingressInternal, nil)
	f.addService("restored-now", ingressAll, map[string]string{labelRestored: "2026-10"})
	f.addService("restored-before", ingressAll, map[string]string{labelRestored: "2026-09"})
	f.addBucket("exempt", map[string]string{labelGuard: valueExempt}, binding{Role: viewer, Members: []string{"allUsers"}})
	g := &guard{cloud: f}

	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if f.serviceByName["guard"].Ingress != ingressAll || f.serviceByName["restored-now"].Ingress != ingressAll {
		t.Error("an exempt or restored-this-period service was restricted")
	}
	if f.serviceByName["worker"].Labels[labelGuard] == valueTripped {
		t.Error("an already-internal service was labelled tripped; restore would open it up")
	}
	if f.serviceByName["restored-before"].Ingress != ingressInternal {
		t.Error("a service restored in an earlier period was not restricted")
	}
	if got := f.members("exempt", viewer); !slices.Equal(got, []string{"allUsers"}) {
		t.Errorf("exempt bucket viewers = %v", got)
	}
}

func TestTripIsIdempotent(t *testing.T) {
	f := releasesFixture()
	g := &guard{cloud: f}
	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	writes := f.writes
	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if f.writes != writes {
		t.Errorf("second trip made %d writes, want none", f.writes-writes)
	}
}

func TestTripContinuesPastFailures(t *testing.T) {
	f := releasesFixture()
	f.failService = "api"
	g := &guard{cloud: f}

	err := g.trip(context.Background(), "p", "2026-10")
	if err == nil || !strings.Contains(err.Error(), "restrict_service api") {
		t.Fatalf("err = %v, want the api failure", err)
	}
	if f.serviceByName["site"].Ingress != ingressInternal {
		t.Error("site was not restricted after api failed")
	}
	if got := f.members("releases", viewer); slices.Contains(got, "allUsers") {
		t.Error("the bucket was not restricted after a service failed")
	}
}

func TestTripStillRestrictsBucketsWhenServicesCannotBeListed(t *testing.T) {
	f := releasesFixture()
	f.serviceListFail = true
	g := &guard{cloud: f}

	if err := g.trip(context.Background(), "p", "2026-10"); err == nil {
		t.Fatal("want the listing error")
	}
	if got := f.members("releases", viewer); slices.Contains(got, "allUsers") {
		t.Error("the bucket was not restricted")
	}
}

func TestDryRunChangesNothing(t *testing.T) {
	f := releasesFixture()
	g := &guard{cloud: f, dryRun: true}
	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if f.writes != 0 {
		t.Errorf("dry run made %d writes", f.writes)
	}
}

func TestRestoreUndoesTrip(t *testing.T) {
	f := releasesFixture()
	f.addService("lb-only", "INGRESS_TRAFFIC_INTERNAL_LOAD_BALANCER", nil)
	g := &guard{cloud: f}
	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if err := g.restore(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}

	if f.serviceByName["site"].Ingress != ingressAll {
		t.Errorf("site ingress = %s", f.serviceByName["site"].Ingress)
	}
	if f.serviceByName["lb-only"].Ingress != "INGRESS_TRAFFIC_INTERNAL_LOAD_BALANCER" {
		t.Errorf("lb-only ingress = %s, want its original", f.serviceByName["lb-only"].Ingress)
	}
	api := f.serviceByName["api"].Labels
	if api["team"] != "thock" || api[labelRestored] != "2026-10" || api[labelGuard] != "" || api[labelIngress] != "" {
		t.Errorf("api labels = %v", api)
	}
	if got := f.members("releases", viewer); !slices.Equal(got, []string{reader, "allUsers"}) {
		t.Errorf("releases viewers = %v", got)
	}
	labels := f.bucketByName["releases"].Labels
	if labels[labelGuard] != "" || labels[labelPublicPrefix+"0"] != "" || labels[labelRestored] != "2026-10" {
		t.Errorf("releases labels = %v", labels)
	}

	// The budget keeps reporting the overspend; a restore has to stick.
	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if f.serviceByName["site"].Ingress != ingressAll || !slices.Contains(f.members("releases", viewer), "allUsers") {
		t.Error("a trip in the same period undid the restore")
	}
}

func TestRestoreRecreatesDroppedBinding(t *testing.T) {
	f := newFakeCloud()
	f.addBucket("site-assets", nil, binding{Role: viewer, Members: []string{"allUsers"}})
	g := &guard{cloud: f}
	if err := g.trip(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if err := g.restore(context.Background(), "p", "2026-10"); err != nil {
		t.Fatal(err)
	}
	if got := f.members("site-assets", viewer); !slices.Equal(got, []string{"allUsers"}) {
		t.Errorf("viewers = %v", got)
	}
}

func TestRestoreReportsLabelsItCannotDecode(t *testing.T) {
	f := newFakeCloud()
	f.addBucket("odd", map[string]string{labelGuard: valueTripped, labelPublicPrefix + "0": "allusers_customrole"})
	g := &guard{cloud: f}

	err := g.restore(context.Background(), "p", "2026-10")
	if err == nil || !strings.Contains(err.Error(), "re-add it by hand") {
		t.Fatalf("err = %v", err)
	}
	if f.bucketByName["odd"].Labels[labelGuard] != "" {
		t.Error("the bucket was left labelled tripped")
	}
}

func TestPublicBindingLabelsRoundTrip(t *testing.T) {
	for role := range storageRoles {
		canonical := storageRoles[role]
		for member := range publicMembers {
			values := encodePublicBindings([]binding{{Role: canonical, Members: []string{member}}}, "b")
			if len(values) != 1 {
				t.Fatalf("%s %s: no label", member, canonical)
			}
			if len(values[0]) > 63 {
				t.Errorf("label %q is longer than GCP allows", values[0])
			}
			gotMember, gotRole, ok := decodePublicBinding(values[0])
			if !ok || gotMember != member || gotRole != canonical {
				t.Errorf("%s %s round-tripped to %s %s %t", member, canonical, gotMember, gotRole, ok)
			}
		}
	}
	if values := encodePublicBindings([]binding{{Role: "projects/p/roles/custom", Members: []string{"allUsers"}}}, "b"); len(values) != 0 {
		t.Errorf("custom role encoded as %v", values)
	}
}
