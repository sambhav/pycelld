"""Cache an approved upstream API in a durable object."""
from celld import Context, Json


class Catalog:
    def __init__(self, id: str, ctx: Context):
        self._ctx = ctx

    async def refresh(self) -> Json:
        response = await self._ctx.fetch(self._ctx.env["CATALOG_URL"])
        if response.status != 200:
            raise ValueError(f"catalog returned HTTP {response.status}")
        payload = response.json()
        if not isinstance(payload, dict) or "items" not in payload:
            raise ValueError("catalog must return a JSON object containing items")
        self._ctx.storage.set("catalog", payload)
        self._ctx.storage.sync()
        return payload

    def cached(self) -> Json:
        return self._ctx.storage.get("catalog", {"items": []})


async def refresh(ctx: Context) -> Json:
    return await Catalog("catalog", ctx).refresh()


def cached(ctx: Context) -> Json:
    return Catalog("catalog", ctx).cached()
