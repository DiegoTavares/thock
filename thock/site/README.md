# Thock website

A single-file static site (`index.html`) with no build step and no dependencies beyond Google Fonts.
Deploy by pointing any static host (Cloudflare Pages, Netlify, GitHub Pages, an S3 bucket) at this
directory.

## Waitlist

The two waitlist forms POST `{ "email": "..." }` as JSON to the endpoint named in the
`WAITLIST_ENDPOINT` constant at the top of the inline script. It ships empty, and the form tells
visitors signups aren't open yet until it's set. Any of these work:

- **Formspree**: create a form, set the endpoint to `https://formspree.io/f/<id>`.
- **Buttondown**: `https://api.buttondown.email/v1/subscribers` behind a tiny proxy (the API key
  can't live in the page).
- **Cloudflare Worker + KV**: a dozen lines; keeps the list in your own account.

## Design

Direction: "Low Light". Warm charcoal ground (`#1A1918`), amber accent (`#E2A554`),
Schibsted Grotesk for text, Geist Mono for keys/paths/demos. Deliberately dark-only for now.

The page is keyboard-navigable (`j`/`k` between sections, `gg`/`G`, `w` for waitlist, `?` for the
keymap sheet), matching the product's keyboard-first principle.

Copy rules: descriptive voice, no selling; vault compatibility stays implicit ("point Thock at the
vault you already keep"); the word "Obsidian" does not appear.
