This source snapshot contains the five Rust library crates from
`helenananaa/pine-compat-runtime` at commit
`8838ed004c7475ce2877189bcd0e43a23bcf365f` (0.3.1), with the external-account
extension from `runtime-patches/pine-compat-0.3.1-external-session.patch`.
It is the same core used by the installed private 0.3.1+mf1 Python adapter.
The MIT license is retained in LICENSE. This isolated snapshot makes the
optional MarketForge experiment buildable without another local checkout.

MarketForge additionally exports ExternalRequestStream and adds an optional
cooperative wall-clock deadline to the runtime. With no deadline configured,
the original deterministic execution budgets and numerical behavior apply.
Its unused JSON snapshot dependency does not enable float_roundtrip globally;
the host's existing JSON parser configuration is preserved. Reconstruction
uses the frozen broker transcript rather than runtime JSON snapshots.

The strategy ecosystem extension also resolves external order arguments from
their canonical builtin slots, preserves `when` conditions, and evaluates
notification/display arguments without changing order execution. The standalone
patch is `runtime-patches/pine-compat-0.3.1-order-notifications.patch`. Its Python
build installs into a separate private runtime directory; no published upstream
version or general community-strategy compatibility is claimed.
