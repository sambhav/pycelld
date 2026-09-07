from celld import Context
from acme.greeters import Greeter, Greeting


class Room:
    def __init__(self, id: str, ctx: Context) -> None:
        self.id = id
        self._ctx = ctx

    def greet(self, name: str) -> Greeting:
        greeting = Greeter(self._ctx).greet(name)
        self._ctx.storage.set("last", greeting.message)
        return greeting

    def last(self) -> str:
        value = self._ctx.storage.get("last", "")
        assert isinstance(value, str)
        return value

