package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"slices"
	"strings"
	"sync"
)

// Labels the guard reads and writes on the resources it touches. The guard
// keeps no state of its own: what it changed, and what restoring puts back,
// lives on the resources.
const (
	// "tripped" on a resource the guard restricted; "exempt", set by hand,
	// keeps the guard away from a resource (its own service, for one).
	labelGuard = "budget-guard"
	// The Cloud Run ingress before the trip, lowercased.
	labelIngress = "budget-guard-ingress"
	// A bucket's public bindings before the trip, one label per binding:
	// budget-guard-public-0 = "allusers_objectviewer".
	labelPublicPrefix = "budget-guard-public-"
	// The billing period ("2026-10") a restore happened in. The budget keeps
	// reporting the same overspend for the rest of the period, and a restore
	// has to stick until the next one.
	labelRestored = "budget-guard-restored"

	valueTripped = "tripped"
	valueExempt  = "exempt"

	ingressAll      = "INGRESS_TRAFFIC_ALL"
	ingressInternal = "INGRESS_TRAFFIC_INTERNAL_ONLY"
)

var publicMembers = map[string]string{
	"allUsers":              "allusers",
	"allAuthenticatedUsers": "allauthenticatedusers",
}

// Label values can't hold capitals, slashes or dots, so a role is stored by
// the lowercase name of a predefined Storage role and mapped back on restore.
var storageRoles = map[string]string{}

func init() {
	for _, role := range []string{
		"admin", "objectAdmin", "objectCreator", "objectUser", "objectViewer",
		"legacyBucketOwner", "legacyBucketReader", "legacyBucketWriter",
		"legacyObjectOwner", "legacyObjectReader",
	} {
		storageRoles[strings.ToLower(role)] = "roles/storage." + role
	}
}

type service struct {
	// Full resource name: projects/p/locations/l/services/s.
	Name    string            `json:"name"`
	Ingress string            `json:"ingress"`
	Labels  map[string]string `json:"labels"`
}

type bucket struct {
	Name   string
	Labels map[string]string
}

type binding struct {
	Role      string   `json:"role"`
	Members   []string `json:"members"`
	Condition any      `json:"condition,omitempty"`
}

type policy struct {
	Version  int       `json:"version,omitempty"`
	Etag     string    `json:"etag,omitempty"`
	Bindings []binding `json:"bindings"`
}

// cloud is the slice of the Cloud Run and Cloud Storage APIs the guard uses.
type cloud interface {
	services(ctx context.Context, project string) ([]service, error)
	// updateService writes the service's ingress and labels as given.
	updateService(ctx context.Context, s service) error
	buckets(ctx context.Context, project string) ([]bucket, error)
	bucketPolicy(ctx context.Context, name string) (policy, error)
	setBucketPolicy(ctx context.Context, name string, p policy) error
	// setBucketLabels sets the given labels; a nil value removes the label.
	setBucketLabels(ctx context.Context, name string, labels map[string]*string) error
}

type guard struct {
	cloud  cloud
	dryRun bool

	// Pub/Sub can deliver notifications for one budget concurrently; trips
	// run one at a time so they don't race each other's read-modify-writes.
	mu sync.Mutex
}

// trip takes a project's public surface offline: every Cloud Run service goes
// to internal-only ingress and every bucket loses its public bindings. It
// skips resources labelled exempt, resources already restricted, and
// resources restored by hand during period. Every resource is attempted; the
// failures come back joined.
func (g *guard) trip(ctx context.Context, project, period string) error {
	g.mu.Lock()
	defer g.mu.Unlock()

	var errs []error
	services, err := g.cloud.services(ctx, project)
	if err != nil {
		errs = append(errs, fmt.Errorf("listing services in %s: %w", project, err))
	}
	for _, s := range services {
		if skipTrip(s.Labels, period) || s.Ingress == ingressInternal {
			continue
		}
		restricted := s
		restricted.Labels = withLabels(s.Labels, map[string]string{
			labelGuard:   valueTripped,
			labelIngress: strings.ToLower(s.Ingress),
		})
		restricted.Ingress = ingressInternal
		g.act("restrict_service", s.Name, func() error { return g.cloud.updateService(ctx, restricted) }, &errs)
	}

	buckets, err := g.cloud.buckets(ctx, project)
	if err != nil {
		errs = append(errs, fmt.Errorf("listing buckets in %s: %w", project, err))
	}
	for _, b := range buckets {
		if skipTrip(b.Labels, period) {
			continue
		}
		current, err := g.cloud.bucketPolicy(ctx, b.Name)
		if err != nil {
			errs = append(errs, fmt.Errorf("reading the policy of %s: %w", b.Name, err))
			continue
		}
		private, removed := withoutPublicMembers(current)
		if len(removed) == 0 {
			continue
		}
		// Record what's being removed before removing it, so a failure
		// in between leaves something restore can still act on.
		labels := map[string]*string{labelGuard: ptr(valueTripped)}
		for i, value := range encodePublicBindings(removed, b.Name) {
			labels[fmt.Sprintf("%s%d", labelPublicPrefix, i)] = ptr(value)
		}
		g.act("restrict_bucket", b.Name, func() error {
			if err := g.cloud.setBucketLabels(ctx, b.Name, labels); err != nil {
				return err
			}
			return g.cloud.setBucketPolicy(ctx, b.Name, private)
		}, &errs)
	}
	return errors.Join(errs...)
}

// restore undoes a trip on the resources labelled tripped, and marks them so
// further notifications in period leave them alone.
func (g *guard) restore(ctx context.Context, project, period string) error {
	g.mu.Lock()
	defer g.mu.Unlock()

	var errs []error
	services, err := g.cloud.services(ctx, project)
	if err != nil {
		errs = append(errs, fmt.Errorf("listing services in %s: %w", project, err))
	}
	for _, s := range services {
		if s.Labels[labelGuard] != valueTripped {
			continue
		}
		restored := s
		restored.Ingress = strings.ToUpper(s.Labels[labelIngress])
		if restored.Ingress == "" {
			restored.Ingress = ingressAll
		}
		restored.Labels = withLabels(s.Labels, map[string]string{labelRestored: period})
		delete(restored.Labels, labelGuard)
		delete(restored.Labels, labelIngress)
		g.act("restore_service", s.Name, func() error { return g.cloud.updateService(ctx, restored) }, &errs)
	}

	buckets, err := g.cloud.buckets(ctx, project)
	if err != nil {
		errs = append(errs, fmt.Errorf("listing buckets in %s: %w", project, err))
	}
	for _, b := range buckets {
		if b.Labels[labelGuard] != valueTripped {
			continue
		}
		current, err := g.cloud.bucketPolicy(ctx, b.Name)
		if err != nil {
			errs = append(errs, fmt.Errorf("reading the policy of %s: %w", b.Name, err))
			continue
		}
		labels := map[string]*string{labelGuard: nil, labelRestored: ptr(period)}
		for key, value := range b.Labels {
			if !strings.HasPrefix(key, labelPublicPrefix) {
				continue
			}
			labels[key] = nil
			member, role, ok := decodePublicBinding(value)
			if !ok {
				errs = append(errs, fmt.Errorf("%s: label %s=%s is not a binding restore knows; re-add it by hand", b.Name, key, value))
				continue
			}
			current = withMember(current, role, member)
		}
		g.act("restore_bucket", b.Name, func() error {
			if err := g.cloud.setBucketPolicy(ctx, b.Name, current); err != nil {
				return err
			}
			return g.cloud.setBucketLabels(ctx, b.Name, labels)
		}, &errs)
	}
	return errors.Join(errs...)
}

// act runs one change, or only logs it in a dry run. The log line is what the
// alert policy matches, so it is written for the dry run too.
func (g *guard) act(action, resource string, change func() error, errs *[]error) {
	if g.dryRun {
		logEvent("WARNING", map[string]any{"action": action, "resource": resource, "dry_run": true})
		return
	}
	if err := change(); err != nil {
		logEvent("ERROR", map[string]any{"action": action, "resource": resource, "error": err.Error()})
		*errs = append(*errs, fmt.Errorf("%s %s: %w", action, resource, err))
		return
	}
	logEvent("WARNING", map[string]any{"action": action, "resource": resource})
}

func skipTrip(labels map[string]string, period string) bool {
	return labels[labelGuard] == valueExempt || (period != "" && labels[labelRestored] == period)
}

func withLabels(labels map[string]string, extra map[string]string) map[string]string {
	merged := make(map[string]string, len(labels)+len(extra))
	for key, value := range labels {
		merged[key] = value
	}
	for key, value := range extra {
		merged[key] = value
	}
	return merged
}

// withoutPublicMembers returns p without allUsers and allAuthenticatedUsers,
// and the bindings that held them.
func withoutPublicMembers(p policy) (policy, []binding) {
	var removed []binding
	private := p
	private.Bindings = nil
	for _, b := range p.Bindings {
		kept := b
		kept.Members = nil
		for _, member := range b.Members {
			if _, public := publicMembers[member]; public {
				removed = append(removed, binding{Role: b.Role, Members: []string{member}})
			} else {
				kept.Members = append(kept.Members, member)
			}
		}
		if len(kept.Members) > 0 {
			private.Bindings = append(private.Bindings, kept)
		}
	}
	return private, removed
}

// withMember adds member to the unconditional binding for role.
func withMember(p policy, role, member string) policy {
	updated := p
	updated.Bindings = slices.Clone(p.Bindings)
	for i, b := range updated.Bindings {
		if b.Role != role || b.Condition != nil {
			continue
		}
		if !slices.Contains(b.Members, member) {
			updated.Bindings[i].Members = append(slices.Clone(b.Members), member)
		}
		return updated
	}
	updated.Bindings = append(updated.Bindings, binding{Role: role, Members: []string{member}})
	return updated
}

// encodePublicBindings turns removed bindings into label values. A role
// that isn't a predefined Storage role can't be stored in a label; it is
// still removed — stopping the spend matters more — and logged so it can be
// re-added by hand.
func encodePublicBindings(removed []binding, bucketName string) []string {
	var values []string
	for _, b := range removed {
		short, known := strings.CutPrefix(b.Role, "roles/storage.")
		if _, ok := storageRoles[strings.ToLower(short)]; !known || !ok {
			logEvent("ERROR", map[string]any{
				"resource": bucketName,
				"error":    fmt.Sprintf("removing %s from %s, which restore cannot put back", b.Members[0], b.Role),
			})
			continue
		}
		values = append(values, publicMembers[b.Members[0]]+"_"+strings.ToLower(short))
	}
	return values
}

func decodePublicBinding(value string) (member, role string, ok bool) {
	shortMember, shortRole, found := strings.Cut(value, "_")
	if !found {
		return "", "", false
	}
	for name, short := range publicMembers {
		if short == shortMember {
			member = name
		}
	}
	role, known := storageRoles[shortRole]
	return member, role, member != "" && known
}

// logEvent writes one structured line for Cloud Logging. The alert policy
// matches jsonPayload.event="budget_guard".
func logEvent(severity string, fields map[string]any) {
	fields["severity"] = severity
	fields["event"] = "budget_guard"
	line, err := json.Marshal(fields)
	if err != nil {
		log.Printf("budget_guard %s: %v (marshal: %v)", severity, fields, err)
		return
	}
	log.Print(string(line))
}

func ptr(value string) *string {
	return &value
}
