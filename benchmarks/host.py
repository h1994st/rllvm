"""Stable host identity and available hardware data, outside timing."""

import hashlib
import os
import platform
import socket
import subprocess
import sys
from pathlib import Path
from typing import Any


def host_metadata() -> dict[str, Any]:
    unavailable = {}
    model = memory = None
    if sys.platform == "darwin":
        values = {}
        for key in ("machdep.cpu.brand_string", "hw.memsize"):
            try:
                result = subprocess.run(
                    ("/usr/sbin/sysctl", "-n", key),
                    capture_output=True,
                    text=True,
                    check=True,
                    timeout=5,
                )
                values[key] = result.stdout.strip()
            except (OSError, subprocess.SubprocessError) as error:
                unavailable[key] = str(error)
        model = values.get("machdep.cpu.brand_string") or None
        try:
            memory = int(values["hw.memsize"])
        except KeyError, ValueError:
            pass
    else:
        try:
            fields = (
                line.partition(":")
                for line in Path("/proc/cpuinfo").read_text().splitlines()
            )
            model = next(
                (
                    value.strip()
                    for key, _, value in fields
                    if key.strip() in {"model name", "Hardware", "Processor"}
                    and value.strip()
                ),
                None,
            )
        except OSError as error:
            unavailable["cpu_model"] = str(error)
        try:
            memory = os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")
        except (OSError, ValueError) as error:
            unavailable["memory_bytes"] = str(error)
    if not model:
        unavailable.setdefault("cpu_model", "CPU model unavailable from OS")
    if memory is None or memory <= 0:
        memory = None
        unavailable.setdefault(
            "memory_bytes", "physical memory unavailable from OS"
        )
    return {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "hostname_sha256": hashlib.sha256(
            socket.gethostname().encode()
        ).hexdigest(),
        "cpu_count": os.cpu_count(),
        "cpu_model": model,
        "memory_bytes": memory,
        "unavailable": unavailable,
        "load_start": os.getloadavg(),
    }
