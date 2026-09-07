from celld import Context, Greeter, Greeting, shout


async def hello(ctx: Context, name: str = "world") -> Greeting:
    greeter = Greeter(ctx)
    await greeter.pause()
    return greeter.greet(name)


def quiet(text: str) -> str:
    return shout(text, excited=False)


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


def greet_room(ctx: Context, id: str, name: str) -> Greeting:
    return Room(id, ctx).greet(name)


def last(ctx: Context, id: str) -> str:
    return Room(id, ctx).last()
