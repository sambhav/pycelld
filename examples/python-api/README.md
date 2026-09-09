# Durable API cache

`POST /refresh` fetches the configured catalog, validates its response and
persists it. `POST /cached` returns the last saved response after process restarts.
The URL comes from host configuration, rather than request parameters.

```sh
pycelld types examples/python-api
pycelld check examples/python-api
pycelld dev examples/python-api
curl -X POST localhost:9876/cached -d '{}'
python -m pytest examples/python-api/tests
```

Outbound fetch is denied by default, so `/refresh` requires an embedding host
with a policy approving the configured API. The existing
[custom host](../extended-host/main.rs) permits `https://api.example.com`;
set `PYCELLD_BINARY` to that host when using a real catalog there. Use an
operator-managed Python policy when running a binary with that feature.
Neither a configured URL nor the test fixture grants network permission.

For an embedding host configured to approve the loopback fixture, an integration
test can point `CATALOG_URL` at `fetch_server.url + "/catalog"`, register
`fetch_server.respond("/catalog", {"items": [{"name": "tea"}]})`, call `refresh`,
restart the worker, and assert `cached` equals the same payload. The committed
test exercises the supplied binary's default denial and proves the upstream
received no request. [Testing guide](../../docs/testing.md) explains fixture
lifecycle and alarm polling.
