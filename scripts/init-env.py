#!/usr/bin/env python3
"""Create local development secrets without overwriting existing configuration."""
import base64
import os
from pathlib import Path
import secrets

root = Path(__file__).resolve().parents[1]
target = root / ".env"
password = secrets.token_urlsafe(32)
key = base64.b64encode(secrets.token_bytes(32)).decode()
template = (root / ".env.example").read_text()
content = template.replace("CHANGE_ME", password).replace("CREDENTIAL_KEY_BASE64=\n", f"CREDENTIAL_KEY_BASE64={key}\n")
try:
    fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
except FileExistsError:
    print(".env already exists; kept the existing secrets.")
else:
    with os.fdopen(fd, "w") as output:
        output.write(content)
    print("Created .env with random credentials (permissions 0600).")
