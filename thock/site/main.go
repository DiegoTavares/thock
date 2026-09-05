// The Thock website (spec: thock/specs/v21-site-hosting-and-download-gate.md).
//
// A static file server and nothing more: everything under public/ is compiled
// into the binary and served as-is, so Cloud Run's Go buildpack can build it
// from source with no Dockerfile — the same shape as the release index in
// thock/services/releases. There is deliberately no logic here; the download
// gate lives entirely in the page (download.html + gate.json).
package main

import (
	"embed"
	"io/fs"
	"log"
	"net/http"
	"os"
	"strings"
)

//go:embed public
var publicFiles embed.FS

// The canonical host. Requests for www.<host> are redirected here so the site
// has one address.
const canonicalHost = "thethock.com"

func handler(files fs.FS) http.Handler {
	fileServer := http.FileServer(http.FS(files))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.EqualFold(r.Host, "www."+canonicalHost) {
			target := "https://" + canonicalHost + r.URL.RequestURI()
			http.Redirect(w, r, target, http.StatusMovedPermanently)
			return
		}
		if r.URL.Path == "/health" {
			w.Header().Set("Content-Type", "text/plain; charset=utf-8")
			if _, err := w.Write([]byte("ok")); err != nil {
				log.Printf("health: %v", err)
			}
			return
		}
		// Clean URLs: /download serves download.html. Anything else is exactly a
		// file under public/ or a 404.
		if r.URL.Path == "/download" {
			r.URL.Path = "/download.html"
		}
		w.Header().Set("Cache-Control", "public, max-age=300")
		w.Header().Set("X-Content-Type-Options", "nosniff")
		fileServer.ServeHTTP(w, r)
	})
}

func main() {
	files, err := fs.Sub(publicFiles, "public")
	if err != nil {
		log.Fatalf("embedded public/ directory: %v", err)
	}
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	log.Printf("thock site listening on :%s", port)
	if err := http.ListenAndServe(":"+port, handler(files)); err != nil {
		log.Fatal(err)
	}
}
