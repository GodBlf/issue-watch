# Review against d96bb384f560795908294f85d9e61f54fa47d8a5

## Standards

0 hard violations. One nonblocking possible Duplicated Code: cancellation uses the same sending→cancelled_inflight / other→cancelled transition in repository, tracking, and subscription removal. Rules are consistent; keep the scoped SQL predicates explicit for this fix and consider small shared cancellation helpers during future maintenance.

## Spec

One P2 found: baseline sampled before waiting for SQLite's write lock could include Issues created before readd acceptance. Fixed by passing an external clock into reconciliation and sampling the new period boundary under the write lock immediately before commit. Production startup, admin initialization and watcher use that clock path; fixed-time public helpers remain for deterministic tests.

Regression: a second real SQLite connection holds the write lock; the old implementation fails with `baseline included the pre-acceptance lock wait`. After correction, the new baseline excludes that wait and an Issue created during the wait is not discovered or queued.

## Validation

Lifecycle regressions, existing broadcast/tracking/configuration tests, browser tests, Linux deployment tests and shell syntax checks exercised. The full Rust suite passed after one transient HTTP 502 in an unchanged QQ fixture; the isolated test and complete rerun passed. Docker Desktop daemon is unavailable locally; the PR pipeline runs Linux Rust 1.90 checks and isolated production Compose smoke before merge.
