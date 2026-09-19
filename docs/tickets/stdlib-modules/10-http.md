# 10: `http`

**What to build:** `require("http")` — an HTTP client, so a script can call a
web API without leaving Lua. Requests with a method, headers and a body; a
response object carrying status, headers and body; and decoding a JSON body in
one step, since that is the common case.

**Blocked by:** 01 (async-first execution), 04 (module selection).

**Status:** ready-for-agent

This ticket carries its own test infrastructure, because `http` cannot be tested
at the seam the rest of the suite uses without it.

- [ ] GET, POST, PUT, PATCH and DELETE, with headers and a body.
- [ ] A response object exposing status, headers and body, so a script can branch on what the server actually said.
- [ ] A non-2xx status reaches Lua as a response, not as an error. Only a transport failure is an error.
- [ ] Decoding a JSON response body is one call.
- [ ] A transport failure — connection refused, DNS failure, timeout — raises a Lua error a script can `pcall`.
- [ ] **A loopback test server**, added to the shared test helpers: a listener on an ephemeral local port in a thread, serving canned responses and recording what it received. Roughly forty lines, no new dependency, and no network access during the test run. This is what keeps `http` testable at the primary seam; the alternative was a mock-server crate, which is a dependency.
- [ ] Astra's `http.lua` mixes client and server in one 475-line file — `HTTPServer`, `http.server` and `http.middleware` are axum and tower code we are not taking. It is cut to roughly its client third, and that cut is the largest single `Changes from the original:` entry in the crate. The file most worth reading carefully after this ticket.
- [ ] Websockets are not carried over. Two further dependencies for a feature with no server on the other side of it in this project, and additive later.
