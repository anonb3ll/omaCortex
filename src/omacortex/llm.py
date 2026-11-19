"""A local language model, reached over an OpenAI-compatible HTTP API.

omaCortex never ships activity to a hosted model. ``endpoint = "auto"``
probes only loopback ports used by local runtimes, in this order:

1. ``omarchy agent local`` models (ports from its ``ports.json``, then 11435+)
2. Ollama (11434)
3. llama.cpp ``llama-server`` (8080)
4. LM Studio (1234)

An explicit endpoint that does not resolve to a loopback address is refused
unless ``model.allow_remote = true`` is set.
"""

from __future__ import annotations

import ipaddress
import json
import os
import socket
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit

MAX_RESPONSE = 2 * 1024 * 1024


class ModelError(RuntimeError):
    pass


def omarchy_local_ports() -> list[int]:
    path = Path.home() / ".local/state/omarchy/agent-local/ports.json"
    ports: list[int] = []
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(data, dict):
            ports = sorted({int(p) for p in data.values() if 1024 <= int(p) <= 65535})
    except (OSError, ValueError, TypeError):
        pass
    return ports or [11435, 11436, 11437]


def candidate_endpoints() -> list[str]:
    ports = [*omarchy_local_ports(), 11434, 8080, 1234]
    seen: list[str] = []
    for port in ports:
        url = f"http://127.0.0.1:{port}/v1"
        if url not in seen:
            seen.append(url)
    return seen


def is_loopback(url: str) -> bool:
    host = urlsplit(url).hostname or ""
    if not host:
        return False
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        pass
    try:
        infos = socket.getaddrinfo(host, None)
    except OSError:
        return False
    return bool(infos) and all(ipaddress.ip_address(i[4][0]).is_loopback for i in infos)


def _http(url: str, payload: dict | None, timeout: float) -> dict:
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json", "Accept": "application/json"},
        method="POST" if payload is not None else "GET",
    )
    # Never pick up an HTTP(S)_PROXY from the environment for local traffic.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=timeout) as resp:
            body = resp.read(MAX_RESPONSE + 1)
    except (urllib.error.URLError, OSError, TimeoutError) as exc:
        raise ModelError(f"{url}: {exc}") from exc
    if len(body) > MAX_RESPONSE:
        raise ModelError(f"{url}: response too large")
    try:
        parsed = json.loads(body)
    except ValueError as exc:
        raise ModelError(f"{url}: not JSON") from exc
    if not isinstance(parsed, dict):
        raise ModelError(f"{url}: unexpected response")
    return parsed


@dataclass
class Model:
    endpoint: str
    model: str
    timeout: float = 120
    max_tokens: int = 600

    def chat(self, system: str, user: str) -> str:
        reply = _http(
            self.endpoint.rstrip("/") + "/chat/completions",
            {
                "model": self.model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                ],
                "temperature": 0.2,
                "max_tokens": self.max_tokens,
                "stream": False,
            },
            self.timeout,
        )
        try:
            text = reply["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError) as exc:
            raise ModelError("model returned no message") from exc
        return strip_reasoning(str(text or "")).strip()


def strip_reasoning(text: str) -> str:
    """Drop a leading <think>…</think> block that reasoning models emit."""
    start = text.find("<think>")
    end = text.find("</think>")
    if start != -1 and end > start:
        return text[:start] + text[end + len("</think>") :]
    return text


def list_models(endpoint: str, timeout: float = 1.5) -> list[str]:
    data = _http(endpoint.rstrip("/") + "/models", None, timeout)
    items = data.get("data") or data.get("models") or []
    names = []
    for item in items if isinstance(items, list) else []:
        if isinstance(item, dict):
            name = item.get("id") or item.get("name") or item.get("model")
            if isinstance(name, str) and name:
                names.append(name)
    return names


def resolve(cfg) -> Model:
    """Find a usable local model or raise ModelError explaining why not."""
    endpoint = str(cfg.get("model", "endpoint") or "auto").strip()
    allow_remote = bool(cfg.get("model", "allow_remote"))
    timeout = float(cfg.get("model", "timeout_seconds") or 120)
    max_tokens = int(cfg.get("model", "max_tokens") or 600)
    wanted = str(cfg.get("model", "model") or "").strip()
    override = os.environ.get("OMACORTEX_MODEL_ENDPOINT", "").strip()
    if override:
        endpoint = override

    if endpoint.lower() == "off":
        raise ModelError('the local model is turned off (model.endpoint = "off")')

    candidates = candidate_endpoints() if endpoint.lower() == "auto" else [endpoint]
    errors = []
    for url in candidates:
        if urlsplit(url).scheme not in ("http", "https"):
            errors.append(f"{url}: not an http(s) URL")
            continue
        if not allow_remote and not is_loopback(url):
            raise ModelError(f"{url} is not on this machine; set model.allow_remote = true to allow it")
        try:
            names = list_models(url)
        except ModelError as exc:
            errors.append(str(exc))
            continue
        model = wanted or (names[0] if names else "")
        if not model:
            errors.append(f"{url}: no models listed")
            continue
        return Model(endpoint=url, model=model, timeout=timeout, max_tokens=max_tokens)
    hint = "run `omarchy agent local use best`, or start Ollama / llama-server"
    raise ModelError("no local model answered (" + hint + ")" + (": " + errors[-1] if errors else ""))
