// Lifestyle money data — THE numeric record. The Money Ritual appends one
// entry per period to window.PERIODS; Set Up Lifestyle and Plan Re-check
// write window.PLAN. The dashboard (index.html) computes everything from
// this file alone. Plain text you own — but keep it out of version history:
// it is listed in .thockignore for a reason.
//
// Schema (per period):
// {
//   id: "2026-W35",                    // unique period id
//   period:  { kind: "week", start: "2026-08-24", end: "2026-08-30" },
//                                      // kind: "week" | "month" | "statement"
//   quality: "measured",               // measured | partial | manual
//   income:  4210,
//   spend:   { total: 3180, categories: { groceries: 612, dining: 188 } },
//   balances:{ chequing: 2140, visa: -1203 },
//   outliers:[ { label: "Bike repair", amount: 420, category: "shopping" } ],
//   actions: [ { label: "Paid CIBC VISA to zero", amount: 1203 } ],
//   progress:{ runway: 41200 },        // keyed by window.PLAN target id
//   amended: null                      // or the date the entry was corrected
// }

window.CONFIG = { currency: "" };

// Written by Set Up Lifestyle, rewritten by Plan Re-check — never by the
// Money Ritual. Target ids are stable across revisions.
window.PLAN = null;
// window.PLAN = {
//   revised: "2026-08-29",
//   horizons: { five_year: "2031", ten_year: "2036" },
//   annual_cost_of_vision: 96000,
//   assumption: "3% real growth",
//   targets: [
//     { id: "runway", label: "Twelve months of runway", unit: "CAD", target: 96000, by: "2028-12-31" }
//   ]
// };

window.PERIODS = [];
