// `option_env!` is invisible to Cargo's change detection: without these, a
// cached build keeps whatever OAuth client it was first compiled with, so a
// rotated secret (or a `.env` that wasn't sourced) silently ships stale.
fn main() {
    println!("cargo::rerun-if-env-changed=THOCK_GOOGLE_CLIENT_ID");
    println!("cargo::rerun-if-env-changed=THOCK_GOOGLE_CLIENT_SECRET");
}
