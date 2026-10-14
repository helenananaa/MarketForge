# Pine host prototypes

`2026-10-14-pine-host-prototypes.patch` preserves the rejected and exploratory
implementations plus their tests and benchmark drivers. It applies to
`05ce347478f9b870996e74ac66c9afc7a980b27e` in a separate checkout, not on top of
the final selected change. SHA256:
`7e7d3c6b1be5da2d333e5a8b6febf09f1081df8c61af047751cf6940328f5390`.

It is the final prototype snapshot, including the additional per-bot identity
lock trial. Earlier measured executable versions differ; use the version/hash
and scope recorded in the parent report rather than relabeling new builds with
old timings. Exact old executables, source manifests, reports and traces remain
under the task's ignored `.local/pine-host-performance-20261013/` directory.

The selected production path keeps source admission sharing only. Identity/seal
caches, input reuse, chain cursors, worker/batch tuning, wake offsets, background
JSON fragments and alternate checkpoint policies were removed after screening.
Typed-state and independent checkpoint experiments remain prototypes; neither
is a deployed structural runtime/storage rewrite.

The patch includes fixed-work tests for complete state/cache work, byte-equal
borrowed host calls, input bounds, private/related cursor coverage and recovery
ordering. Saved live-state fixtures are local evidence. The complete borrowed
turn benchmark also generates a fresh deterministic synthetic fixture; this is
not the missing historical v14 fixture.

See `../2026-10-13-pine-host-performance.md` and its JSON companion for results,
invalid-attempt exclusions, evidence hashes and qualification limits. No measured
capacity gate is relaxed to make a rejected option pass.
