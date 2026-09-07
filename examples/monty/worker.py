from celld import Context, Response
from dataclasses import dataclass
from datetime import timedelta


@dataclass
class Count:
    id: str
    value: int


def hello(name: str = "world") -> str:
    return f"Hello, {name}!"


def increment(ctx: Context, id: str, amount: int = 1) -> Count:
    return Counter(id, ctx).increment(amount)


def fail() -> None:
    raise ValueError("example failure")


async def upstream(ctx: Context, url: str) -> Response:
    return await ctx.fetch(url)


class Counter:
    def __init__(self, id: str, ctx: Context):
        self.id = id
        self._ctx = ctx

    def increment(self, amount: int = 1) -> Count:
        with self._ctx.storage.transaction() as storage:
            previous = storage.get("count", 0)
            assert isinstance(previous, int)
            value = previous + amount
            storage.set("count", value)
        return Count(self.id, value)

    def schedule(self, delay: int = 1) -> None:
        self._ctx.alarms.set(timedelta(seconds=delay))

    def alarm(self) -> None:
        self._ctx.storage.set("alarmed", True)
