"""Host evidence separates machines and explains unavailable hardware data."""

import hashlib
import socket


def test_host_identity_is_stable_and_hardware_is_available_or_explained():
    from benchmarks.host import host_metadata

    first, second = host_metadata(), host_metadata()
    expected = hashlib.sha256(socket.gethostname().encode()).hexdigest()
    assert first["hostname_sha256"] == second["hostname_sha256"] == expected
    for name in ("cpu_model", "memory_bytes"):
        assert first[name] == second[name]
        assert first[name] or first["unavailable"][name]
    if first["memory_bytes"] is not None:
        assert first["memory_bytes"] > 0
