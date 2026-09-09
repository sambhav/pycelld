"""Enable with ``pytest_plugins = ['pycelld.pytest_plugin']`` in conftest.py."""
from contextlib import ExitStack

import pytest

from .testing import FetchServer, Worker


def pytest_addoption(parser):
    parser.addoption("--celld-binary", help="Path to the real celld executable (or set PYCELLD_BINARY)")


@pytest.fixture
def celld_worker(request):
    """Factory for isolated workers, stopped and removed even if the test fails."""
    with ExitStack() as stack:
        def create(project, **options):
            options.setdefault("binary", request.config.getoption("--celld-binary"))
            return stack.enter_context(Worker(project, **options))
        yield create


@pytest.fixture
def fetch_server():
    with FetchServer() as server:
        yield server
