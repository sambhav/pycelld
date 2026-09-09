from typing import Literal, NotRequired, TypedDict

class Request(TypedDict):
    url: str
    scheme: str
    host: str
    port: int
    method: str
    headers: list[list[str]]
    body: bytes

class Principal(TypedDict):
    tenant_id: str | None
    subject: str
    claims: object

class Execution(TypedDict):
    worker_id: str
    deployment_id: str
    invocation_id: str
    runtime_instance_id: str
    root_invocation_id: str
    parent_invocation_id: str | None
    principal: Principal | None

# `class` is a Python keyword, so this dictionary type uses functional syntax.
ObjectIdentity = TypedDict("ObjectIdentity", {"class": str, "id": str})

class Limits(TypedDict):
    cpu_ms: int
    wall_ms: int
    max_operations: int
    max_payload_bytes: int

class Context(TypedDict):
    request_url: str
    execution: Execution
    object: ObjectIdentity | None
    alarm: bool
    limits: Limits

class Forward(TypedDict):
    action: Literal["forward"]
    url: NotRequired[str]
    method: NotRequired[str]
    headers: NotRequired[list[tuple[str, str] | list[str]]]
    body: NotRequired[bytes | str]

class Respond(TypedDict):
    action: Literal["respond"]
    status: int
    headers: NotRequired[list[tuple[str, str] | list[str]]]
    body: NotRequired[bytes | str]

class Deny(TypedDict):
    action: Literal["deny"]
    reason: NotRequired[str]

def policy(request: Request, context: Context) -> Forward | Respond | Deny: ...
