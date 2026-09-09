import json
from pathlib import Path


PROJECT = Path(__file__).resolve().parents[1]


def test_empty_cache_and_default_network_denial(celld_worker, fetch_server, tmp_path):
    # Configure an actual loopback upstream, then prove the stock policy denies
    # the fetch before a transport connection is made.
    import shutil
    project = tmp_path / "catalog"
    shutil.copytree(PROJECT, project)
    config = json.loads((project / "wrangler.jsonc").read_text())
    config["vars"]["CATALOG_URL"] = fetch_server.url + "/catalog"
    (project / "wrangler.jsonc").write_text(json.dumps(config))
    fetch_server.respond("/catalog", {"items": [{"name": "tea"}]})
    # Request diagnostic detail for this assertion; the stock binary otherwise
    # redacts Python exception messages in public responses.
    worker = celld_worker(project, env={"CELLD_PYTHON_PUBLIC_ERRORS": "1"})
    assert worker.call("cached") == {"items": []}
    response = worker.request("/refresh", method="POST", body=b"{}")
    assert response.status == 500
    assert "outbound HTTP is disabled" in response.text
    assert fetch_server.requests == []
    worker.restart()
    assert worker.call("cached") == {"items": []}
