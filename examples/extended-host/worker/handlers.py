from celld import Context
from acme.greeters import Greeter, Greeting
from acme.native import shout
from .objects import Room


async def hello(ctx: Context, name: str = "world") -> Greeting:
    greeter = Greeter(ctx)
    await greeter.pause()
    return greeter.greet(name)


def quiet(text: str) -> str:
    return shout(text, excited=False)


def greet_room(ctx: Context, id: str, name: str) -> Greeting:
    return Room(id, ctx).greet(name)


def last(ctx: Context, id: str) -> str:
    return Room(id, ctx).last()
