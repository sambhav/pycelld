"""Reject Linux release binaries whose runtime ABI exceeds the supported floor."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

MAX_GLIBC = (2, 28)
TARGETS = {
    "x86_64-unknown-linux-gnu": ("Advanced Micro Devices X86-64", "/lib64/ld-linux-x86-64.so.2"),
    "aarch64-unknown-linux-gnu": ("AArch64", "/lib/ld-linux-aarch64.so.1"),
}
# V8 ships its C++ runtime statically. Do not silently pick up the compiler
# image's libstdc++, libssl or another library absent on ordinary installations.
LIBRARIES = {"libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2",
             "librt.so.1", "libutil.so.1", "libgcc_s.so.1",
             "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1"}


def inspect(headers, program, dynamic, versions, target):
    machine, loader = TARGETS[target]
    if not re.search(r"Class:\s+ELF64\s*$", headers, re.M):
        raise ValueError("expected a 64-bit ELF binary")
    if not re.search(r"Machine:\s+" + re.escape(machine) + r"\s*$", headers, re.M):
        raise ValueError("ELF architecture does not match release target")
    if f"[Requesting program interpreter: {loader}]" not in program:
        raise ValueError("unexpected dynamic loader")
    if re.search(r"\((?:RPATH|RUNPATH)\)", dynamic):
        raise ValueError("release binary must not depend on a build-directory runtime path")
    needed = sorted(set(re.findall(r"\(NEEDED\).*?\[([^]]+)\]", dynamic)))
    if not needed or set(needed) - LIBRARIES:
        raise ValueError(f"unexpected runtime libraries: {needed}")
    names = set(re.findall(r"Name:\s+(GLIBC_\S+)", versions))
    if not names:
        raise ValueError("no glibc version requirements found")
    numeric = []
    for name in names:
        match = re.fullmatch(r"GLIBC_(\d+(?:\.\d+)+)", name)
        if not match:
            raise ValueError(f"unsupported glibc ABI requirement: {name}")
        version = tuple(map(int, match[1].split('.')))
        if version > MAX_GLIBC:
            raise ValueError(f"{name} exceeds GLIBC_2.28")
        numeric.append(version)
    return {"baseline": "2.28", "required_glibc": '.'.join(map(str, max(numeric))),
            "libraries": needed, "interpreter": loader}


def audit(binary, target):
    def readelf(option):
        return subprocess.check_output(["readelf", "--wide", option, str(binary)],
                                       text=True, env={**os.environ, "LC_ALL": "C"})
    result = inspect(readelf("--file-header"), readelf("--program-headers"),
                     readelf("--dynamic"), readelf("--version-info"), target)
    result["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = audit(args.binary, args.target)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, sort_keys=True))
