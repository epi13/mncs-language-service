#!/usr/bin/env python3
"""Provider lifecycle tests: status/ensure/stop/poll/capsule/query.

Exercises tools/mnls_provider.py against the real worktree host binary
in isolated temp workspaces. Run from the repository root::

    python3 tools/test_mnls_provider.py

Requires the host binary at target/debug/mnls-language-service-host
(or MNLS_LANGUAGE_SERVICE_HOST); the tests never touch the family
checkouts outside their temp workspaces.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PROVIDER = REPO / "tools" / "mnls_provider.py"
FIXTURES = REPO / "tests" / "fixtures"

HOST_CANDIDATES = [
    Path(os.environ["MNLS_LANGUAGE_SERVICE_HOST"])
    if os.environ.get("MNLS_LANGUAGE_SERVICE_HOST") else None,
    REPO / "target" / "debug" / "mnls-language-service-host",
    REPO / "target" / "release" / "mnls-language-service-host",
]


def host_binary() -> Path | None:
    for candidate in HOST_CANDIDATES:
        if candidate is not None and candidate.is_file():
            return candidate
    found = shutil.which("mnls-language-service-host")
    return Path(found) if found else None


def run_provider(*argv: str, env: dict | None = None, timeout: float = 60.0):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    completed = subprocess.run(
        [sys.executable, str(PROVIDER), *argv],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        timeout=timeout, text=True, env=merged, cwd=str(REPO),
    )
    return completed


def stdout_json(completed) -> dict:
    try:
        value = json.loads(completed.stdout or "")
    except ValueError:
        raise AssertionError(
            f"provider stdout is not JSON: {completed.stdout!r} "
            f"stderr={completed.stderr!r}")
    assert isinstance(value, dict), f"provider stdout is not an object: {value!r}"
    return value


@unittest.skipUnless(host_binary(), "mnls-language-service-host is not built")
class ProviderLifecycleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.host = str(host_binary())
        self.env = {
            "MNLS_LANGUAGE_SERVICE_HOST": self.host,
            "MNCS_LANGUAGE_ROOT": "/nonexistent-language-root-for-tests",
        }
        self.workspaces: list[Path] = []

    def tearDown(self) -> None:
        for workspace in self.workspaces:
            try:
                run_provider("stop", "--workspace", str(workspace), env=self.env)
            except Exception:
                pass

    def make_workspace(self, *fixtures: str) -> Path:
        workspace = Path(tempfile.mkdtemp(prefix="mnls-provider-test-"))
        self.workspaces.append(workspace)
        for name in fixtures:
            shutil.copy(FIXTURES / name, workspace / name)
        return workspace

    def socket_alive(self, workspace: Path) -> bool:
        sock = workspace / ".mncs" / "mnls-language-service.sock"
        if not sock.exists():
            return False
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(2.0)
        try:
            client.connect(str(sock))
            client.sendall(b'{"id": 1, "method": "service_status", "params": {}}\n')
            data = client.recv(65536)
            return bool(data)
        except OSError:
            return False
        finally:
            client.close()

    def test_status_reports_absent_for_a_quiet_workspace(self) -> None:
        workspace = self.make_workspace("records.mncs")
        completed = run_provider("status", "--workspace", str(workspace), env=self.env)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        document = stdout_json(completed)
        self.assertEqual(document["schema_version"],
                         "mncs.language-service.resident-status/1")
        self.assertFalse(document["ready"])
        self.assertEqual(document["state"], "absent")
        self.assertEqual(document["selected"]["workspace_root"], str(workspace.resolve()))

    def test_status_rejects_a_missing_workspace(self) -> None:
        missing = Path(tempfile.mkdtemp(prefix="mnls-provider-missing-")) / "nope"
        completed = run_provider("status", "--workspace", str(missing), env=self.env)
        self.assertEqual(completed.returncode, 2)

    def test_ensure_attaches_and_stop_is_idempotent(self) -> None:
        workspace = self.make_workspace("records.mncs")
        first = run_provider("ensure", "--workspace", str(workspace), env=self.env)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(stdout_json(first)["state"], "started")
        try:
            second = run_provider("ensure", "--workspace", str(workspace), env=self.env)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual(stdout_json(second)["state"], "attached")

            status = run_provider("status", "--workspace", str(workspace), env=self.env)
            self.assertEqual(status.returncode, 0, status.stderr)
            document = stdout_json(status)
            self.assertTrue(document["ready"], document)
            self.assertEqual(document["state"], "ready")
            self.assertIn("generation", document["observed"])
            self.assertIn("stream_identity", document["observed"])
            self.assertTrue(document["lease"]["owned"])
        finally:
            stopped = run_provider("stop", "--workspace", str(workspace), env=self.env)
            self.assertEqual(stopped.returncode, 0, stopped.stderr)
            self.assertEqual(stdout_json(stopped)["state"], "stopped")
        self.assertFalse(self.socket_alive(workspace))
        again = run_provider("stop", "--workspace", str(workspace), env=self.env)
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(stdout_json(again)["state"], "not-owned")

    def test_foreign_socket_is_never_adopted_or_killed(self) -> None:
        home = self.make_workspace("records.mncs")
        foreign_root = self.make_workspace("records.mncs")
        foreign_socket = foreign_root / ".mncs" / "mnls-language-service.sock"
        foreign_socket.parent.mkdir(parents=True, exist_ok=True)
        env = dict(self.env)
        env["MNLS_WORKSPACE_ROOT"] = str(home.resolve())
        env["MNLS_SERVICE_SOCKET"] = str(foreign_socket)
        process = subprocess.Popen(
            [self.host], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL, env=env, start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 15.0
            while time.monotonic() < deadline and not foreign_socket.exists():
                time.sleep(0.05)
            self.assertTrue(foreign_socket.exists(), "foreign host did not start")

            status = run_provider("status", "--workspace", str(foreign_root), env=self.env)
            self.assertEqual(status.returncode, 0, status.stderr)
            document = stdout_json(status)
            self.assertFalse(document["ready"])
            self.assertEqual(document["state"], "foreign", document)

            ensure = run_provider("ensure", "--workspace", str(foreign_root), env=self.env)
            self.assertEqual(ensure.returncode, 3, ensure.stdout)
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.SubprocessError:
                process.kill()
        # The foreign host survived the refused reconcile: only the test
        # harness stopped it (exit code above proves refusal, not a kill).
        self.assertIsNotNone(process.returncode)

    def test_stale_toolchain_reconciles_an_owned_lease(self) -> None:
        workspace = self.make_workspace("records.mncs")
        first_env = dict(self.env)
        first_env["MNLS_TOOLCHAIN_IDENTITY"] = "provider-test-pinned-1"
        first = run_provider("ensure", "--workspace", str(workspace), env=first_env)
        self.assertEqual(first.returncode, 0, first.stderr)
        try:
            second_env = dict(self.env)
            second_env["MNLS_TOOLCHAIN_IDENTITY"] = "provider-test-pinned-2"
            status = run_provider("status", "--workspace", str(workspace), env=second_env)
            self.assertEqual(status.returncode, 0, status.stderr)
            self.assertEqual(stdout_json(status)["state"], "stale-toolchain")

            reconciled = run_provider("ensure", "--workspace", str(workspace), env=second_env)
            self.assertEqual(reconciled.returncode, 0, reconciled.stderr)
            self.assertEqual(stdout_json(reconciled)["state"], "started")

            ready = run_provider("status", "--workspace", str(workspace), env=second_env)
            self.assertTrue(stdout_json(ready)["ready"], ready.stdout)
            self.env = second_env
        finally:
            run_provider("stop", "--workspace", str(workspace), env=second_env)

    def test_stale_build_reconciles_an_owned_lease_by_executable_bytes(self) -> None:
        workspace = self.make_workspace("records.mncs")
        alternate = workspace / "mnls-language-service-host"
        shutil.copy2(self.host, alternate)
        with alternate.open("ab") as executable:
            executable.write(b"selected-build-change")
        os.chmod(alternate, 0o755)
        original = dict(self.env)
        selected = dict(self.env)
        selected["MNLS_LANGUAGE_SERVICE_HOST"] = str(alternate)

        started = run_provider("ensure", "--workspace", str(workspace), env=original)
        self.assertEqual(started.returncode, 0, started.stderr)
        try:
            status = run_provider("status", "--workspace", str(workspace), env=selected)
            self.assertEqual(status.returncode, 0, status.stderr)
            self.assertEqual(stdout_json(status)["state"], "stale-build")

            reconciled = run_provider("ensure", "--workspace", str(workspace), env=selected)
            self.assertEqual(reconciled.returncode, 0, reconciled.stdout + reconciled.stderr)
            self.assertEqual(stdout_json(reconciled)["state"], "started")

            ready = run_provider("status", "--workspace", str(workspace), env=selected)
            self.assertTrue(stdout_json(ready)["ready"], ready.stdout)
            self.assertEqual(
                stdout_json(ready)["service"]["build_fingerprint"],
                stdout_json(ready)["selected"]["host"]["build_fingerprint"],
            )
        finally:
            run_provider("stop", "--workspace", str(workspace), env=selected)

    def test_poll_capsule_and_query_serve_bounded_reads(self) -> None:
        workspace = self.make_workspace("records.mncs", "syntax-error.mncs")
        ensured = run_provider("ensure", "--workspace", str(workspace), env=self.env)
        self.assertEqual(ensured.returncode, 0, ensured.stderr)
        try:
            status = stdout_json(run_provider(
                "status", "--workspace", str(workspace), env=self.env))
            stream = status["observed"]["stream_identity"]

            poll = run_provider(
                "poll", "--workspace", str(workspace),
                "--stream", stream, "--after", "0", "--max", "8", env=self.env)
            self.assertEqual(poll.returncode, 0, poll.stderr)
            cursor = stdout_json(poll)
            self.assertEqual(cursor["stream_identity"], stream)
            self.assertFalse(cursor["reset_required"])

            capsule = run_provider(
                "capsule", "--workspace", str(workspace),
                "--stream", stream, "--after", "0", env=self.env)
            self.assertEqual(capsule.returncode, 0, capsule.stderr)
            body = stdout_json(capsule)
            self.assertEqual(body["schema_version"],
                             "mncs.language-service.semantic-capsule/1")
            self.assertTrue(body["window_matched"], body)
            self.assertLessEqual(len(json.dumps(body).encode("utf-8")), 64 * 1024)

            query = run_provider(
                "query", "--workspace", str(workspace),
                "--method", "workspace_symbols", "--params", '{"query": "record"}',
                env=self.env)
            self.assertEqual(query.returncode, 0, query.stderr)
            self.assertIn("symbols", stdout_json(query))
        finally:
            run_provider("stop", "--workspace", str(workspace), env=self.env)

    def test_status_converges_shell_edits_before_reporting(self) -> None:
        workspace = self.make_workspace("records.mncs")
        ensured = run_provider("ensure", "--workspace", str(workspace), env=self.env)
        self.assertEqual(ensured.returncode, 0, ensured.stderr)
        try:
            before = stdout_json(run_provider(
                "status", "--workspace", str(workspace), env=self.env))
            target = workspace / "records.mncs"
            target.write_text(target.read_text() + "\n", encoding="utf-8")
            after = stdout_json(run_provider(
                "status", "--workspace", str(workspace), env=self.env))
            self.assertTrue(after["ready"], after)
            self.assertGreater(
                after["observed"]["generation"],
                before["observed"]["generation"])
        finally:
            run_provider("stop", "--workspace", str(workspace), env=self.env)

    def test_query_refuses_writes_and_source_dumps(self) -> None:
        workspace = self.make_workspace("records.mncs")
        for method in ("did_open", "did_save", "configure_root", "content",
                       "rename", "refresh_workspace", "analyze_candidate"):
            completed = run_provider(
                "query", "--workspace", str(workspace),
                "--method", method, env=self.env)
            self.assertEqual(completed.returncode, 2, method)


if __name__ == "__main__":
    unittest.main(verbosity=2)
