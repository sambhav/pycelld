# HTTP handlers

Export one function decorated with `celld.http` to handle ordinary HTTP requests:

```python
from celld import Request, Response, http

@http
def handle(request: Request) -> Response:
    if request.method == "PUT" and request.path == "/v1/files":
        return Response(request.body, status=201)
    return Response("not found", status=404)
```

The handler takes `request` and optionally `ctx: Context`, and can be synchronous
or asynchronous. `ctx.request` is the same request. No framework is needed.
Run the [example](../examples/http/worker.py) with `celld dev examples/http`.
New Python deployments require the `monty-http-v1` host feature, so older nodes
reject them before worker activation.

## Routing and exports

Exact public-function paths retain precedence: `POST /hello` calls an exported
`hello` function with JSON arguments; other methods at `/hello` return 405 with
`Allow: POST`. Every other path and method goes to the HTTP handler, including
`/`, nested paths, HEAD and OPTIONS. Without a handler these paths return 404.
The HTTP handler itself does not also become a public-function endpoint.

Only entry-module exports count. A package can re-export a decorated handler
from a submodule; include its public name in `__all__` if using explicit exports.
Exporting multiple HTTP handlers (including aliases of the same handler) fails
at compilation. Private names and functions omitted from `__all__` are not
handlers. The decorator must be imported from `celld` (`@http`, an imported
alias, or `@celld.http`); arbitrary decorators and `@http()` are unsupported.

## Request and response

- `request.body` is buffered `bytes`, including zero bytes and arbitrary binary data.
- `request.text()` decodes UTF-8; `request.json()` parses JSON of any shape.
  Catch `ValueError` for malformed JSON and return a 400 response. Uncaught
  exceptions use the normal structured runtime error response.
- `request.url` is the full URL. `request.path` is its percent-encoded pathname.
- `request.query` contains decoded query parameters, with the last value winning.
  `query_items` preserves all pairs; `get_all_query(name)` returns repeated values.
- `request.headers` retains the existing lowercase dictionary view.
  `header_items` preserves repeated pairs; `get_all_headers(name)` is case-insensitive.
- `Response(body, status=200, headers=...)` accepts text or bytes. Use a dictionary
  for ordinary headers or a list of pairs for repeated headers such as `Set-Cookie`.
  Header names and values are validated. Bodyless statuses discard their body.

String, bytes, JSON-compatible values and `None` can also be returned directly,
as for public-function endpoints. Requests and results remain buffered and
bounded by the native runtime's existing limits (1 MiB by default). Awaited host
operations retain normal disconnect cancellation. There is no streaming,
`yield`, multipart parser or framework compatibility layer in this API.

Request bodies are available on the receiving stateless handler's `ctx.request`.
Calls to durable objects forward request metadata; they do not copy the original
HTTP body into the durable object's context.
