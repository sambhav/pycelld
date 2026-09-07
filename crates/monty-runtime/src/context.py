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


class Response:
    def __init__(self, body: str | bytes = "", *, status: int = 200, headers: dict[str, str] | None = None):
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


class Context:
    def __init__(self, metadata):
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

    def log(self, message: str) -> None:
        _celld_host("log", message)
