// GitHub integration: the API client and report building (api), plus the two
// background watchers that poll it (issue_watcher, ci_watcher).
//
// `api` is re-exported flat so callers keep using `crate::github::<item>`
// rather than `crate::github::api::<item>` — the split is an organisational
// one, not a change to this module's public surface.
pub mod api;
pub mod ci_watcher;
pub mod issue_watcher;

pub use api::*;
