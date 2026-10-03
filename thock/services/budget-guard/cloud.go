package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
)

const (
	runAPI     = "https://run.googleapis.com/v2"
	storageAPI = "https://storage.googleapis.com/storage/v1"
)

// restCloud talks to the Cloud Run Admin v2 and Cloud Storage JSON APIs over
// an HTTP client that already carries Google credentials.
type restCloud struct {
	client      *http.Client
	runBase     string
	storageBase string
}

func newRestCloud(client *http.Client) *restCloud {
	return &restCloud{client: client, runBase: runAPI, storageBase: storageAPI}
}

func (c *restCloud) services(ctx context.Context, project string) ([]service, error) {
	var services []service
	pageToken := ""
	for {
		query := url.Values{"pageSize": {"100"}}
		if pageToken != "" {
			query.Set("pageToken", pageToken)
		}
		var page struct {
			Services      []service `json:"services"`
			NextPageToken string    `json:"nextPageToken"`
		}
		endpoint := fmt.Sprintf("%s/projects/%s/locations/-/services?%s", c.runBase, url.PathEscape(project), query.Encode())
		if err := c.call(ctx, http.MethodGet, endpoint, nil, &page); err != nil {
			return nil, err
		}
		services = append(services, page.Services...)
		if page.NextPageToken == "" {
			return services, nil
		}
		pageToken = page.NextPageToken
	}
}

// updateService patches only ingress and labels. The update mask keeps the
// revision template untouched, so no new revision is rolled out.
func (c *restCloud) updateService(ctx context.Context, s service) error {
	body := map[string]any{"ingress": s.Ingress, "labels": s.Labels}
	query := url.Values{"updateMask": {"ingress,labels"}}
	return c.call(ctx, http.MethodPatch, fmt.Sprintf("%s/%s?%s", c.runBase, s.Name, query.Encode()), body, nil)
}

func (c *restCloud) buckets(ctx context.Context, project string) ([]bucket, error) {
	var buckets []bucket
	pageToken := ""
	for {
		query := url.Values{"project": {project}, "fields": {"items(name,labels),nextPageToken"}}
		if pageToken != "" {
			query.Set("pageToken", pageToken)
		}
		var page struct {
			Items []struct {
				Name   string            `json:"name"`
				Labels map[string]string `json:"labels"`
			} `json:"items"`
			NextPageToken string `json:"nextPageToken"`
		}
		if err := c.call(ctx, http.MethodGet, c.storageBase+"/b?"+query.Encode(), nil, &page); err != nil {
			return nil, err
		}
		for _, item := range page.Items {
			buckets = append(buckets, bucket{Name: item.Name, Labels: item.Labels})
		}
		if page.NextPageToken == "" {
			return buckets, nil
		}
		pageToken = page.NextPageToken
	}
}

func (c *restCloud) bucketPolicy(ctx context.Context, name string) (policy, error) {
	var p policy
	endpoint := fmt.Sprintf("%s/b/%s/iam?optionsRequestedPolicyVersion=3", c.storageBase, url.PathEscape(name))
	err := c.call(ctx, http.MethodGet, endpoint, nil, &p)
	return p, err
}

// setBucketPolicy writes p with its etag, so a concurrent change fails the
// write instead of being overwritten.
func (c *restCloud) setBucketPolicy(ctx context.Context, name string, p policy) error {
	if p.Bindings == nil {
		p.Bindings = []binding{}
	}
	return c.call(ctx, http.MethodPut, fmt.Sprintf("%s/b/%s/iam", c.storageBase, url.PathEscape(name)), p, nil)
}

func (c *restCloud) setBucketLabels(ctx context.Context, name string, labels map[string]*string) error {
	body := map[string]any{"labels": labels}
	return c.call(ctx, http.MethodPatch, fmt.Sprintf("%s/b/%s?fields=name", c.storageBase, url.PathEscape(name)), body, nil)
}

func (c *restCloud) call(ctx context.Context, method, endpoint string, body, out any) error {
	var reader io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return err
		}
		reader = bytes.NewReader(encoded)
	}
	request, err := http.NewRequestWithContext(ctx, method, endpoint, reader)
	if err != nil {
		return err
	}
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	response, err := c.client.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	payload, err := io.ReadAll(io.LimitReader(response.Body, 8<<20))
	if err != nil {
		return err
	}
	if response.StatusCode/100 != 2 {
		return fmt.Errorf("%s %s: %s: %s", method, endpoint, response.Status, bytes.TrimSpace(payload))
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(payload, out)
}
