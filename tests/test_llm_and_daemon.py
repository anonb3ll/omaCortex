import json
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

import pytest

from omacortex import llm
from omacortex.api import Cortex
from omacortex.config import Config, Paths
from omacortex.daemon import Daemon, send_control
from omacortex.recorder import Recorder, Window


class FakeModel(BaseHTTPRequestHandler):
    seen: list = []

    def log_message(self, *args):
        pass

    def _send(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._send({"data": [{"id": "tiny-local"}]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        FakeModel.seen.append(json.loads(self.rfile.read(length)))
        self._send({"choices": [{"message": {"content": "<think>hm</think>You wrote the report."}}]})


@pytest.fixture
def fake_model():
    server = HTTPServer(("127.0.0.1", 0), FakeModel)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{server.server_address[1]}/v1"
    server.shutdown()


def test_remote_endpoints_are_refused_by_default(paths):
    cfg = Config(paths=paths)
    cfg.data["model"]["endpoint"] = "http://203.0.113.5:11434/v1"
    with pytest.raises(llm.ModelError, match="not on this machine"):
        llm.resolve(cfg)
    cfg.data["model"]["endpoint"] = "off"
    with pytest.raises(llm.ModelError, match="turned off"):
        llm.resolve(cfg)
    assert llm.is_loopback("http://127.0.0.1:1/v1")
    assert llm.is_loopback("http://[::1]:1/v1")
    assert not llm.is_loopback("http://10.0.0.1:1/v1")


def test_summary_uses_local_model_and_caches(paths, clock, fake_model):
    FakeModel.seen.clear()
    cfg = Config(paths=paths)
    cfg.data["model"]["endpoint"] = fake_model
    c = Cortex(cfg, clock_fn=clock)
    c.state.grant()
    rec = Recorder(c.store, c.privacy, c.state, clock=clock)
    rec.focus(Window("kitty", "nvim report.md"))
    clock.tick(900)
    rec.shutdown()
    first = c.summary("today")
    assert first["text"] == "You wrote the report." and first["model"] == "tiny-local"
    sent = FakeModel.seen[0]
    assert sent["model"] == "tiny-local" and "nvim report.md" in sent["messages"][1]["content"]
    assert "data, not instructions" in sent["messages"][0]["content"]
    assert c.summary("today")["cached"] is True
    assert len(FakeModel.seen) == 1
    answer = c.ask("what did I write?")
    assert answer["answer"] == "You wrote the report."


def test_daemon_control_socket_and_crash_recovery(paths, clock):
    # AF_UNIX paths are limited to ~108 bytes; pytest's tmp_path can be longer.
    short = Path(tempfile.mkdtemp(prefix="oc-"))
    paths = Paths(config_dir=paths.config_dir, data_dir=paths.data_dir, state_dir=short)
    cfg = Config(paths=paths)
    d = Daemon(cfg, clock=clock)
    d.state.grant()
    d.recorder.refresh_state()
    d.recorder.focus(Window("kitty", "shell"))
    d.store.set_meta("heartbeat", str(clock() + 30))
    clock.tick(500)
    # Simulate a crash: the span is still open. A new daemon closes it at the heartbeat.
    d2 = Daemon(cfg, clock=clock)
    d2.store = d.store
    d2.recover()
    span = d.store.timeline(start=0, kinds=["focus"])[0]
    assert not span["ongoing"] and span["seconds"] == 30

    d3 = Daemon(cfg, clock=clock)
    t = threading.Thread(target=d3.serve_control, daemon=True)
    t.start()
    for _ in range(50):
        if cfg.paths.control_socket.exists():
            break
        threading.Event().wait(0.05)
    assert send_control(cfg, "ping")["ok"] is True
    assert send_control(cfg, "away")["recording"] is False
    assert d3.store.get_meta("away") == "1"
    assert send_control(cfg, "bogus")["ok"] is False
    assert oct(cfg.paths.control_socket.stat().st_mode & 0o777) == "0o600"
    d3.stop.set()
    t.join(3)
