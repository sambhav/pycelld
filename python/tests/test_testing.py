from urllib.error import HTTPError
from urllib.request import Request, urlopen

import pytest

from pycelld.testing import FetchServer, wait_until


def test_recorded_fetch_fixture_and_cleanup():
    with FetchServer() as upstream:
        upstream.respond("/quote", {"price": 10}, status=201, headers={"X-Fixture": "yes"})
        with urlopen(Request(upstream.url + "/quote", data=b"hello"), timeout=2) as response:
            assert response.status == 201
            assert response.headers["X-Fixture"] == "yes"
            assert response.read() == b'{"price": 10}'
        assert upstream.requests[0].method == "POST"
        assert upstream.requests[0].body == b"hello"
        with pytest.raises(HTTPError) as error:
            urlopen(upstream.url + "/missing", timeout=2)
        assert error.value.code == 404
    assert not upstream._thread.is_alive()


def test_alarm_wait_is_bounded_and_does_not_swallow_assertions():
    with pytest.raises(TimeoutError):
        wait_until(lambda: False, timeout=0.01)
    def broken():
        raise AssertionError("handler failed")
    with pytest.raises(AssertionError, match="handler failed"):
        wait_until(broken)
