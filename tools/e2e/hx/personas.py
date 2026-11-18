"""Test personas and owner sessions. Secrets never reach stdout.

Passwords: hagency-server/secrets/e2e-<name>.password (0600, generated once).
Access tokens: <STATE_DIR>/tokens/<name>.json (0600), reused until rejected."""
import json
import os
import secrets
import time
from pathlib import Path

from . import config
from .matrix import Client, MatrixError, native_sign_in, username_available

PROVISION_WAIT_S = 90


def _write_private(path: Path, text: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(text)
    os.chmod(path, 0o600)


def _password(localpart: str) -> str:
    path = config.password_file(localpart)
    if not path.exists():
        _write_private(path, secrets.token_urlsafe(24) + "\n")
    return path.read_text().strip()


def _token_path(key: str) -> Path:
    return config.STATE_DIR / "tokens" / f"{key}.json"


def _cached(key: str, origin: str):
    path = _token_path(key)
    if not path.exists():
        return None
    try:
        return Client(origin, json.loads(path.read_text())["access_token"])
    except MatrixError as e:
        if e.status in (401, 403):  # expired or revoked: sign in again below
            return None
        raise


def _session(key: str, server: str, localpart: str, password: str, allow_register: bool) -> Client:
    origin = config.SERVERS[server]["http"]
    expected = config.mxid(localpart, server)
    client = _cached(key, origin)
    if client and client.user_id == expected:
        return client
    register = allow_register and username_available(origin, localpart)
    grant = native_sign_in(origin, localpart, password, register=register)
    _write_private(_token_path(key), json.dumps(grant))
    client = _await_provisioned(origin, grant["access_token"])
    if client.user_id != expected:
        raise MatrixError("sign-in", 0, f"signed in as {client.user_id}, expected {expected}")
    return client


def _await_provisioned(origin: str, token: str) -> Client:
    """A new account reaches the homeserver a few seconds after Pasion
    creates it; whoami answers 401 "not yet provisioned" until then."""
    deadline = time.time() + PROVISION_WAIT_S
    while True:
        try:
            return Client(origin, token)
        except MatrixError as e:
            if e.status != 401 or "provisioned" not in e.body or time.time() > deadline:
                raise
            time.sleep(2)


def persona(localpart: str) -> Client:
    """A throwaway zz-* account, registered on first use."""
    if localpart not in config.PERSONAS:
        raise KeyError(f"unknown persona {localpart}")
    return _session(localpart, config.PERSONAS[localpart], localpart, _password(localpart), True)
