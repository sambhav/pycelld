from dataclasses import dataclass
from celld import Context
from acme.native import shout


@dataclass
class Greeting:
    message: str
    request_id: str


class Greeter:
    def __init__(self, ctx: Context) -> None:
        self._ctx = ctx

    def greet(self, name: str) -> Greeting:
        return Greeting(shout("Hello, " + name), self._ctx.uuid())

    async def pause(self) -> None:
        await self._ctx.sleep(0.001)
