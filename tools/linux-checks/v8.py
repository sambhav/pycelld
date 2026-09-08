"""Run the shipped V8 engine, HTTP and durable SQLite on the oldest runtime."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
from urllib.error import URLError
from urllib.request import Request, urlopen

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root / 'wrangler.json').write_text(json.dumps({
        'name': 'glibc-check', 'main': 'index.js', 'no_bundle': True,
        'compatibility_date': '2026-01-01',
        'durable_objects': {'bindings': [{'name': 'COUNTER', 'class_name': 'Counter'}]},
        'migrations': [{'tag': 'v1', 'new_sqlite_classes': ['Counter']}],
    }))
    (root / 'index.js').write_text('''
export class Counter {
  constructor(ctx) { this.ctx = ctx; }
  async fetch(request) {
    const value = (await this.ctx.storage.get("value")) ?? 0;
    if (request.method === "POST") await this.ctx.storage.put("value", value + 1);
    return Response.json({value: request.method === "POST" ? value + 1 : value,
                          math: Math.exp(1) > 2 && Math.log2(8) === 3});
  }
}
export default { fetch(request, env) {
  return env.COUNTER.get(env.COUNTER.idFromName("same")).fetch(request);
}};
''')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    url = f'http://127.0.0.1:{port}/'
    env = {k: v for k, v in os.environ.items() if not k.startswith('CELLD_')}
    def call(method):
        with urlopen(Request(url, method=method), timeout=10) as response:
            return json.load(response)
    for expected in [0, 1]:
        with (root / 'server.log').open('w') as log:
            process = subprocess.Popen([binary, 'dev', str(root), '--port', str(port), '--no-watch', '--logs'],
                                       env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 90
                while True:
                    if process.poll() is not None:
                        raise AssertionError((root / 'server.log').read_text())
                    try:
                        value = call('GET')
                        break
                    except (URLError, ConnectionError):
                        if time.monotonic() >= deadline:
                            raise AssertionError((root / 'server.log').read_text())
                        time.sleep(.1)
                assert value == {'value': expected, 'math': True}, value
                if expected == 0:
                    assert call('POST') == {'value': 1, 'math': True}
            finally:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
print('V8, HTTP and durable SQLite restart passed on the compatibility runtime')
