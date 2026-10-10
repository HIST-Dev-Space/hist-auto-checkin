#!/usr/bin/env python3
"""Run Cargo checks with the local development database; never print .env secrets."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
from urllib.parse import urlsplit

root = Path(__file__).resolve().parents[1]
environment = os.environ.copy()
config = root / ".env"
if config.exists():
    for line in config.read_text().splitlines():
        if line.strip() and not line.lstrip().startswith("#") and "=" in line:
            key, value = line.split("=", 1)
            environment.setdefault(key, value)
if environment.get("TEST_DATABASE_URL"):
    environment["DATABASE_URL"] = environment["TEST_DATABASE_URL"]
args = sys.argv[1:] or ["test", "--all-targets"]
if args[0] == "test" and "--lib" not in args:
    database = environment.get("DATABASE_URL", "")
    if urlsplit(database).hostname not in {"localhost", "127.0.0.1", "::1"}:
        raise SystemExit("Integration tests require a local PostgreSQL database with CREATE DATABASE permission.")
cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
raise SystemExit(subprocess.call([cargo, *args], cwd=root, env=environment))
