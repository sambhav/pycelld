from celld import Context


async def run(ctx: Context, id: str, backend: str):
    print("HTTP request", id)
    return await Reporter(id, ctx).report(backend)


class Reporter:
    def __init__(self, id: str, ctx: Context):
        self._ctx = ctx

    async def report(self, backend: str):
        with self._ctx.span("load-report", fields={"format": "text"}):
            self._ctx.log("loading", fields={"execution": "cannot replace host identity"})
            response = await self._ctx.fetch(backend)
            print("fetch completed", response.status)
            return {"status": response.status}
