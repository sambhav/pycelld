#!/usr/bin/env python3
"""Real native HTTP -> object -> fetch telemetry check; stdlib-only OTLP receiver."""
import concurrent.futures
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from urllib.request import Request, urlopen


def fields(data):
    """Decode the protobuf wire types used by OTLP, preserving repeated fields."""
    at = 0
    def varint():
        nonlocal at
        result = shift = 0
        while True:
            byte = data[at]
            at += 1
            result |= (byte & 127) << shift
            if byte < 128:
                return result
            shift += 7
    result = {}
    while at < len(data):
        key = varint()
        number, wire = key >> 3, key & 7
        if wire == 0:
            value = varint()
        elif wire in (1, 5):
            size = 8 if wire == 1 else 4
            value = data[at:at + size]
            at += size
        elif wire == 2:
            size = varint()
            value = data[at:at + size]
            at += size
        else:
            raise AssertionError(f"unsupported wire type {wire}")
        result.setdefault(number, []).append(value)
    return result


def records(data):
    for resource in fields(data).get(1, []):
        for scope in fields(resource).get(2, []):
            for record in fields(scope).get(2, []):
                yield fields(record)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    binary = str(Path(sys.argv[1]).resolve())
    captured, backend_traces = [], []
    lock = threading.Lock()
    fail = threading.Event()
    class Receiver(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_GET(self):
            with lock:
                backend_traces.append(self.headers.get("traceparent"))
            time.sleep(0.03)
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"report")
        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            with lock:
                captured.extend((self.path, record) for record in records(body))
            self.send_response(503 if fail.is_set() else 200)
            self.end_headers()
    receiver = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Receiver)
    thread = threading.Thread(target=receiver.serve_forever, daemon=True)
    thread.start()
    port = free_port()
    endpoint = f"http://127.0.0.1:{receiver.server_port}"
    with tempfile.TemporaryDirectory(prefix="pycelld-observability-") as temporary:
        root = Path(temporary)
        project = root / "project"
        shutil.copytree(Path(__file__).resolve().parents[2] / "examples/observability", project)
        env = {key: value for key, value in os.environ.items() if not key.startswith("CELLD_") and not key.startswith("OTEL_")}
        env.update(CELLD_OTEL="1", CELLD_OTEL_SINK="otlp", CELLD_OTEL_FLUSH_MS="100", OTEL_EXPORTER_OTLP_ENDPOINT=endpoint, OTEL_EXPORTER_OTLP_TIMEOUT="100", CELLD_MAX_STATELESS_ISOLATES="2")
        def call(number):
            trace = f"{number:032x}"
            request = Request(f"http://127.0.0.1:{port}/run", data=json.dumps({"id": f"object-{number}", "backend": endpoint}).encode(), headers={"content-type":"application/json", "traceparent":f"00-{trace}-0123456789abcdef-01"})
            with urlopen(request, timeout=10) as response:
                assert response.status == 200
                response.read()
            return bytes.fromhex(trace)
        with (root / "server.log").open("w+") as log:
            process = subprocess.Popen([binary, "dev", str(project), "--port", str(port), "--logs"], env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 120
                while True:
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise AssertionError("server failed to start")
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.2):
                            break
                    except OSError:
                        time.sleep(.1)
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    traces = list(pool.map(call, [1, 2]))
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    with lock:
                        snapshots = list(captured)
                    if (len([r for p, r in snapshots if p.endswith("traces") and r.get(5) == [b"load-report"]]) >= 2
                            and len([r for p, r in snapshots if p.endswith("logs")]) >= 6):
                        break
                    time.sleep(.1)
                for number, trace in enumerate(traces, 1):
                    spans = [r for p, r in snapshots if p.endswith("traces") and r.get(1) == [trace]]
                    logs = [r for p, r in snapshots if p.endswith("logs") and r.get(9) == [trace]]
                    assert len(spans) >= 4, spans
                    span_ids = {r[2][0] for r in spans}
                    assert any(r.get(5) == [b"load-report"] and r[4][0] in span_ids for r in spans)
                    assert sum(r.get(4, [None])[0] in span_ids for r in spans) >= 3
                    decoded = [json.loads(fields(r[5][0])[1][0]) for r in logs]
                    assert len(decoded) >= 3, decoded
                    assert all(r[10][0] in span_ids for r in logs)
                    object_logs = [r for r in decoded if r["execution"]["object_id"]]
                    assert object_logs and all(r["execution"]["object_id"] == f"object-{number}" for r in object_logs)
                    assert len({r["execution"]["invocation_id"] for r in decoded}) == 2
                    assert all(r["execution"]["invocation_id"] != "cannot replace host identity" for r in decoded)
                assert {value.split("-")[1] for value in backend_traces} == {trace.hex() for trace in traces}
                fail.set()
                started = time.monotonic()
                for number in range(3, 8):
                    call(number)
                assert time.monotonic() - started < 10, "collector outage stalled requests"
                print("PASS: linked traces, per-invocation logs, custom spans, concurrent identities, collector failure")
            except Exception:
                log.flush()
                print((root / "server.log").read_text(), file=sys.stderr)
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    receiver.shutdown()


if __name__ == "__main__":
    main()
