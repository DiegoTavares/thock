// Who this dashboard is for. The Set Profile ritual writes this from
// `profile.md`; edit it by hand any time.
//
//   code   true: always show the pull-request panel, tiles and timeline bar
//          false: hide them, and show carried-over goals and personal items
//                 in their place
//          "auto" (the default): show them only in weeks that carry PRs/MRs
//   focus  the areas from profile.md's "What you track", in order. Each one
//          keeps the same colour from week to week.
window.PROFILE = {
  code: "auto",
  focus: []
};

// Weekly review data — one entry per week, newest last.
// The Week Review skill appends a new object to this array each week.
//
// Schema (per week):
// {
//   id: "2026_29_Jul",       // week id = weekly filename stem
//   week: 29,                // the WW number
//   label: "Week 29",
//   range: "Jul 13 – Jul 19, 2026",
//   status: "reviewed",      // "reviewed", or "in-progress" if the week isn't over
//   goals:     [ { text: "…", done: true } ],   // from # Week Goals
//   tentative: [ { text: "…", done: false } ],  // from # Tentative (or [])
//   personal:  [ { text: "…", done: false } ],  // from # Personal (or [])
//   highlights: [ "…" ],     // 2–3 entries, or [] for an in-progress week
//   projects: [
//     { name: "Scheduler", goal: true, tasks: [ "task one" ] }  // omit `goal` when not a week goal
//   ],
//   prs: {
//     created:  [ { ref: "webapp#2425", title: "…", status: "open", src: "github" } ],
//     reviewed: [ { ref: "platform-api!18", title: "…", src: "gitlab" } ]
//   }
// }
window.WEEKS = [];

// Repo short-name → full path, so refs in the feed become clickable links.
// Add a row the first time a repository shows up; unmapped refs still render
// as plain text. `host` is your GitLab instance (self-hosted or gitlab.com).
window.REPOS = {
  github: {
    // webapp: "your-org/webapp"
  },
  gitlab: {
    host: "",
    paths: {
      // "platform-api": "your-group/platform-api"
    }
  }
};

// Optional [pattern, canonical name] pairs (matched case-insensitively, first
// match wins) that fold project-name drift together so the dashboard's
// lingering-project detection survives "api" vs "API / Platform".
window.PROJECT_ALIASES = [
  // ["platform|api", "Platform API"]
];
