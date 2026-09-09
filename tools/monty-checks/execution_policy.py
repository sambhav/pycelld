"""Live execution-policy changes on the same compiled worker and running process."""
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import sys
import tempfile

from pycelld.testing import Worker, wait_until

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    project = root / "app"
    project.mkdir()
    (project / "wrangler.jsonc").write_text(json.dumps({"name": "live-policy-test", "main": "worker.py"}))
    (project / "worker.py").write_text('''
from celld import Context
from datetime import timedelta
def identify(ctx):
    return {"revision": ctx.execution.policy_revision,
            "instance": ctx.execution.runtime_instance_id,
            "invocation": ctx.execution.invocation_id,
            "project": ctx.execution.application.project_id,
            "application": ctx.execution.application.application_id,
            "stage": ctx.execution.application.stage,
            "tier": ctx.execution.application.tier,
            "labels": ctx.execution.application.labels,
            "cpu": ctx.limits.cpu_ms, "wall": ctx.limits.wall_ms,
            "operations": ctx.limits.max_operations}
async def slow(ctx):
    print("slow invocation admitted")
    await ctx.sleep(2)
    return identify(ctx)
def operations(ctx):
    ctx.now()
    ctx.now()
    return 'done'
def busy(ctx):
    while True: pass
def payload(): return 'x' * 2000
class Probe:
    def __init__(self, id: str, ctx: Context): self._ctx = ctx
    def info(self): return identify(self._ctx)
    def work(self): return operations(self._ctx)
    def schedule(self): self._ctx.alarms.set(timedelta(seconds=1))
    def alarm(self): self._ctx.storage.set('alarm_revision', self._ctx.execution.policy_revision)
    def alarm_revision(self): return self._ctx.storage.get('alarm_revision')
def durable_info(ctx: Context): return Probe('test', ctx).info()
def durable_work(ctx: Context): return Probe('test', ctx).work()
def schedule(ctx: Context): return Probe('test', ctx).schedule()
def alarm_revision(ctx: Context): return Probe('test', ctx).alarm_revision()
''')
    policy = root / "policy.json"
    config = {
        "revision": "one", "defaults": {"cpu_ms": 100, "wall_ms": 5000},
        "workers": {"live-policy-test": {
            "project_id": "project-a", "application_id": "orders", "stage": "test", "tier": "free",
            "labels": {"region": "eu"}}},
        "rules": [{"matches": {"tier": "free", "labels.region": "eu"}, "limits": {"max_operations": 2}}],
    }

    def publish(value):
        temporary = root / "policy.next"
        temporary.write_text(json.dumps(value))
        temporary.replace(policy)

    publish(config)
    with Worker(project, binary=binary, env={"CELLD_EXECUTION_POLICY": str(policy),
                                           "CELLD_MAX_STATELESS_ISOLATES": "1"}) as worker:
        try:
            first = worker.call("identify")
            assert first["project"] == "project-a" and first["labels"] == {"region": "eu"}
            assert first["operations"] == 2
            assert worker.call("operations") == "done"
            assert worker.call("operations") == "done"  # independent budget
            durable = worker.call("durable_info")
            assert durable["project"] == "project-a" and durable["revision"] == "one"
            assert worker.call("durable_work") == "done"
            with ThreadPoolExecutor(max_workers=1) as pool:
                pending = pool.submit(worker.call, "slow")
                wait_until(lambda: "slow invocation admitted" in worker.logs)
                config["revision"] = "two"
                config["defaults"] = {"cpu_ms": 20, "wall_ms": 100, "max_payload_bytes": 1024}
                config["rules"][0]["limits"]["max_operations"] = 1
                publish(config)
                wait_until(lambda: worker.call("identify")["revision"] == "two")
                current = worker.call("identify")
                assert current["instance"] == first["instance"]
                assert current["invocation"] != first["invocation"]
                assert current["cpu"] == 20 and current["wall"] == 100 and current["operations"] == 1
                previous = pending.result(timeout=10)
                assert previous["revision"] == "one" and previous["wall"] == 5000
            for name in ["operations", "busy", "slow", "payload"]:
                assert worker.request(name, method="POST", body=b"{}").status >= 400, name
            updated_durable = worker.call("durable_info")
            assert updated_durable["instance"] == durable["instance"]
            assert updated_durable["revision"] == "two" and updated_durable["operations"] == 1
            assert worker.request("durable_work", method="POST", body=b"{}").status >= 400
            worker.call("schedule")
            wait_until(lambda: worker.call("alarm_revision") == "two")
            assert worker.call("identify")["instance"] == first["instance"]
            # Bad updates fail admission and cannot silently fall back to a
            # previous, potentially more permissive budget. Recovery is live.
            config["defaults"]["max_memory_bytes"] = 1048576
            publish(config)
            wait_until(lambda: worker.request("identify", method="POST", body=b"{}").status == 503)
            del config["defaults"]["max_memory_bytes"]
            config["revision"] = "three"
            publish(config)
            wait_until(lambda: worker.request("identify", method="POST", body=b"{}").status == 200)
            assert worker.call("identify")["revision"] == "three"
            config["workers"] = {}
            publish(config)
            wait_until(lambda: worker.request("identify", method="POST", body=b"{}").status == 503)
            policy.unlink()
            assert worker.request("identify", method="POST", body=b"{}").status == 503
            print("Live mapping, limits, invocation snapshots, fail-closed reloads and unchanged worker instance passed.")
        except BaseException:
            print(worker.logs, file=sys.stderr)
            raise
