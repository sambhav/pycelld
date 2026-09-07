"""Exercise the built celld binary against strict S3-compatible HTTP fixtures.

Usage: python3 tools/storage-checks/probe.py /path/to/celld [unittest arguments]
These fixtures test wire behavior and fencing; they are not a live Ceph cluster.
"""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

BINARY = str(Path(sys.argv.pop(1)).resolve())


@contextlib.contextmanager
def endpoint(response_style, required_style, *, faulty=None, token=None):
    objects = {}
    calls = []

    def spell(value, style):
        return f'"{value}"' if style == "quoted" else value

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def reply(self, status, body=b"", etag=None):
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            if etag is not None:
                self.send_header("ETag", etag)
            self.end_headers()
            if body:
                self.wfile.write(body)

        def do_GET(self):
            calls.append(("GET", self.path, None, None, 200))
            self.reply(200, b'<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>celld-test</Name><Prefix></Prefix><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated></ListBucketResult>')

        def do_PUT(self):
            path = urlsplit(self.path).path
            body = self.rfile.read(int(self.headers["Content-Length"]))
            current = objects.get(path)
            match, absent = self.headers.get("If-Match"), self.headers.get("If-None-Match")
            expected = spell(current, required_style) if current else None
            rejected = (absent == "*" and current is not None) or (match is not None and match != expected)
            if faulty == "ignore-stale" and match is not None:
                rejected = False
            status = 412 if rejected else 200
            if not rejected:
                objects[path] = hashlib.md5(body).hexdigest()
                if faulty == "ambiguous-update" and match is not None:
                    status = 500  # committed, response lost/failed; MUST NOT retry CAS
            calls.append(("PUT", path, match, absent, status))
            if status >= 400:
                code = "PreconditionFailed" if status == 412 else "InternalError"
                self.reply(status, f"<Error><Code>{code}</Code><Message>fixture</Message></Error>".encode())
            else:
                etag = token if token is not None else spell(objects[path], response_style)
                self.reply(status, etag=etag)

        def do_DELETE(self):
            objects.pop(urlsplit(self.path).path, None)
            calls.append(("DELETE", self.path, None, None, 204))
            self.reply(204)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", calls
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def diagnose(url, mode=None):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CELLD_", "AWS_", "S3_", "AZURE_", "GOOGLE_"))}
    env.update(AWS_ACCESS_KEY_ID="fixture", AWS_SECRET_ACCESS_KEY="fixture", AWS_EC2_METADATA_DISABLED="true", NO_PROXY="127.0.0.1,localhost")
    if mode is not None:
        env["CELLD_S3_ETAG_MODE"] = mode
    return subprocess.run([BINARY, "diagnose", "--bucket", "celld-test", "--endpoint", url,
        "--region", "us-east-1", "--listen", "127.0.0.1:0", "--internal-listen", "127.0.0.1:0", "--json"],
        env=env, capture_output=True, text=True, timeout=30)


class NativeCasTests(unittest.TestCase):
    def check_probe(self, response, required, mode, *, ok, faulty=None, token=None):
        with endpoint(response, required, faulty=faulty, token=token) as (url, calls):
            result = diagnose(url, mode)
        rows = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        verdicts = [row["verdict"] for row in rows if row.get("check") == "bucket conditional write"]
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        self.assertEqual(verdicts, ["ok" if ok else "fail"], result.stdout + result.stderr)
        puts = [call for call in calls if call[0] == "PUT"]
        if ok:
            self.assertEqual([call[4] for call in puts], [200, 412, 200, 412])
            self.assertEqual([call[3] for call in puts[:2]], ["*", "*"])
            self.assertEqual(puts[2][2], puts[3][2], "stale update must reuse the original token")
        self.assertEqual([call[0] for call in calls if call[0] != "GET"][-1], "DELETE", "probe must clean up after success or failure")
        return puts

    def test_preserve_round_trips_provider_spelling(self):
        for style in ["quoted", "unquoted"]:
            with self.subTest(style=style):
                self.check_probe(style, style, None, ok=True)

    def test_quoted_mode_fixes_bare_response_tags(self):
        self.check_probe("unquoted", "quoted", "preserve", ok=False)
        puts = self.check_probe("unquoted", "quoted", "quoted", ok=True)
        self.assertTrue(puts[2][2].startswith('"'))

    def test_unquoted_mode_fixes_quoted_response_tags(self):
        self.check_probe("quoted", "unquoted", "preserve", ok=False)
        puts = self.check_probe("quoted", "unquoted", "unquoted", ok=True)
        self.assertFalse(puts[2][2].startswith('"'))

    def test_compatibility_never_bypasses_stale_write_rejection(self):
        for mode in ["quoted", "unquoted"]:
            with self.subTest(mode=mode):
                puts = self.check_probe("unquoted", mode, mode, ok=False, faulty="ignore-stale")
                self.assertEqual([call[4] for call in puts], [200, 412, 200, 200])

    def test_ambiguous_conditional_writes_are_not_retried(self):
        puts = self.check_probe("unquoted", "quoted", "quoted", ok=False, faulty="ambiguous-update")
        self.assertEqual([call[4] for call in puts], [200, 412, 500])

    def test_unsafe_tokens_never_reach_if_match(self):
        for token in ['*', '"*"', 'W/"abc"', '"a","b"', '""']:
            with self.subTest(token=token):
                puts = self.check_probe("quoted", "quoted", "quoted", ok=False, token=token)
                self.assertEqual(len(puts), 1)
                self.assertIsNone(puts[0][2])

    def test_invalid_mode_fails_before_storage_access(self):
        with endpoint("quoted", "quoted") as (url, calls):
            result = diagnose(url, "typo")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CELLD_S3_ETAG_MODE", result.stderr + result.stdout)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
