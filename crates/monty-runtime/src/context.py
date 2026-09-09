"""Built-in Monty API; signatures are published by `celld types`."""
import json as _celld_json
from datetime import datetime, date, time, timedelta, timezone


class _Transaction:
    def __init__(self, storage):
        self._storage = storage

    def __enter__(self):
        _celld_host("storage.transaction_begin")
        return self._storage

    def __exit__(self, exc_type, exc_value, traceback) -> bool:
        _celld_host("storage.transaction_commit" if exc_type is None else "storage.transaction_rollback")
        return False


class Storage:
    def set(self, key: str, value: Json) -> None:
        _celld_host("storage.put", key, value)

    def delete(self, key: str) -> bool:
        return _celld_host("storage.delete", key)

    def get(self, key: str, default: Json = None) -> Json:
        entry = _celld_host("storage.get", key)
        return entry["value"] if entry["found"] else default

    def list(self, prefix: str = "", *, limit: int = 1000, reverse: bool = False) -> dict[str, Json]:
        return _celld_host("storage.list", prefix, limit, reverse)

    def clear(self) -> None:
        _celld_host("storage.delete_all")

    def sql(self, query: str, *bindings: SqlValue) -> list[dict[str, SqlValue]]:
        return _celld_host("storage.sql", query, list(bindings))

    def sync(self) -> None:
        _celld_host("storage.sync")

    def transaction(self) -> _Transaction:
        return _Transaction(self)


class Alarms:
    def get(self) -> datetime | None:
        return _celld_host("storage.get_alarm")

    def set(self, when: datetime | timedelta) -> None:
        if not isinstance(when, (datetime, timedelta)):
            raise TypeError("alarm time must be a datetime or timedelta")
        _celld_host("storage.set_alarm", when)

    def delete(self) -> None:
        _celld_host("storage.delete_alarm")


def http(handler):
    """Declare the exported catchall HTTP handler (recognized at compile time)."""
    return handler


class Response:
    def __init__(self, body: str | bytes = "", *, status: int = 200, headers: dict[str, str] | list[tuple[str, str]] | None = None):
        self.body = body
        self.status = status
        self.headers = headers or {}

    def text(self) -> str:
        return self.body.decode("utf-8") if isinstance(self.body, bytes) else self.body

    def json(self) -> Json:
        return _celld_json.loads(self.text())


class Request:
    def __init__(self, metadata):
        self.method = metadata.get("method", "POST")
        self.url = metadata.get("url", "")
        self.headers = metadata.get("headers", {})
        self.header_items = [(key, value) for key, value in metadata.get("header_items", list(self.headers.items()))]
        self.path = metadata.get("path", "")
        self.query_items = [(key, value) for key, value in metadata.get("query_items", [])]
        self.query = dict(self.query_items)
        self.body = b""

    def text(self) -> str:
        return self.body.decode("utf-8")

    def json(self) -> Json:
        return _celld_json.loads(self.text())

    def get_all_headers(self, name: str) -> list[str]:
        return [value for key, value in self.header_items if key.lower() == name.lower()]

    def get_all_query(self, name: str) -> list[str]:
        return [value for key, value in self.query_items if key == name]


class ApplicationIdentity:
    def __init__(self, metadata):
        self.project_id = metadata.get("project_id", "")
        self.application_id = metadata.get("application_id", "")
        self.stage = metadata.get("stage")
        self.tier = metadata.get("tier")
        self.labels = metadata.get("labels", {})


class ExecutionMetadata:
    def __init__(self, metadata):
        self.worker_id = metadata.get("worker_id", "")
        self.deployment_id = metadata.get("deployment_id", "")
        self.invocation_id = metadata.get("invocation_id", "")
        self.runtime_instance_id = metadata.get("runtime_instance_id", "")
        self.root_invocation_id = metadata.get("root_invocation_id", "")
        self.parent_invocation_id = metadata.get("parent_invocation_id")
        self.principal = metadata.get("principal")
        self.application = ApplicationIdentity(metadata.get("application", {}))
        self.policy_revision = metadata.get("policy_revision", "")


class ExecutionLimits:
    def __init__(self, metadata):
        self.cpu_ms = metadata.get("cpu_ms", 100)
        self.wall_ms = metadata.get("wall_ms", 30000)
        self.max_operations = metadata.get("max_operations", 10000)
        self.max_payload_bytes = metadata.get("max_payload_bytes", 1048576)
        self.max_recursion_depth = metadata.get("max_recursion_depth", 100)
        self.max_memory_bytes = metadata.get("max_memory_bytes")


class Context:
    def __init__(self, metadata):
        self.execution = ExecutionMetadata(metadata.get("execution", {}))
        self.limits = ExecutionLimits(metadata.get("limits", {}))
        self.id = metadata.get("id")
        self.env = metadata.get("env", {})
        self.request = Request(metadata.get("request", {}))
        self.storage = Storage()
        self.alarms = Alarms()

    async def fetch(self, url: str, *, method: str = "GET", headers: dict[str, str] | None = None, body: str | bytes | None = None) -> Response:
        result = _celld_host("fetch", url, method, headers or {}, body)
        return Response(result["body"], status=result["status"], headers=result["headers"])

    async def sleep(self, seconds: float) -> None:
        _celld_host("sleep", seconds)

    def now(self) -> datetime:
        return _celld_host("now")

    def uuid(self) -> str:
        return _celld_host("uuid")

    def log(self, message: str, *, level: str = "info", fields: dict | None = None) -> None:
        _celld_host("log", message, level, fields or {})

    def span(self, name: str, *, fields: dict | None = None):
        return Span(name, fields or {})

class Span:
    def __init__(self, name: str, fields: dict | None = None):
        self._name = name
        self._fields = fields or {}
        self._token = None

    def __enter__(self):
        self._token = _celld_host("span", "start", self._name, self._fields)
        return self

    def __exit__(self, exc_type, exc, traceback):
        _celld_host("span", "end", self._token, exc_type is None)
        return False
