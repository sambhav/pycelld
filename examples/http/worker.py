from celld import Context, Request, Response, http


@http
async def handle(request: Request, ctx: Context) -> Response:
    if request.path == "/health" and request.method == "GET":
        return Response("ok")
    if request.path == "/v1/echo" and request.method in ["POST", "PUT"]:
        return Response(request.body, headers={"content-type": "application/octet-stream"})
    if request.path == "/v1/webhook" and request.method == "POST":
        try:
            payload = request.json()
        except ValueError:
            return Response("invalid JSON", status=400)
        ctx.log(f"webhook received: {payload}")
        return Response(status=204)
    return Response("not found", status=404)


def hello(name: str = "world") -> str:
    return f"Hello, {name}!"
