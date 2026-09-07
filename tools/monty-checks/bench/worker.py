from celld import Context, Response


def hello(name: str = "world") -> str:
    return f"Hello, {name}!"


def echo(value: str) -> dict[str, str]:
    return {"value": value}


def increment(ctx: Context, id: str) -> dict[str, int]:
    return {"value": Counter(id, ctx).increment()}


async def pause(ctx: Context) -> str:
    await ctx.sleep(0.01)
    return "done"


class Counter:
    def __init__(self, id: str, ctx: Context):
        self.id = id
        self._ctx = ctx

    def ping(self) -> int:
        return 1

    def increment(self) -> int:
        with self._ctx.storage.transaction() as storage:
            previous = storage.get("count", 0)
            assert isinstance(previous, int)
            value = previous + 1
            storage.set("count", value)
        return value


def object(ctx: Context, id: str) -> dict[str, int]:
    return {"value": Counter(id, ctx).ping()}


async def fetch(ctx: Context, url: str) -> Response:
    return await ctx.fetch(url, method="POST", body='{"value":"' + "x" * 16384 + '"}')
