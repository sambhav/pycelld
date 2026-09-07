from dataclasses import dataclass
from celld import Context

@dataclass
class Greeting:
    message: str
    request_id: str

class Greeter:
    def __init__(self, ctx: Context) -> None: ...
    def greet(self, name: str) -> Greeting: ...
    async def pause(self) -> None: ...
