#!/usr/bin/env python3
"""Provider-owned resident lifecycle for the MNCS Language Service.

Environment (and Doctor, and the reconciler) operate the resident service
only through this tool. Socket paths, lease files, host-binary discovery,
process supervision, and toolchain measurement are provider-owned details;
callers pass a workspace root and receive bounded JSON documents.

Subcommands:
  status    read-only probe (Doctor probe / ambient observation)
  ensure    bounded reconcile: attach, or start when absent/stale
  stop      stop only a provider-owned lease (idempotent, never foreign)
  poll      resume the resident event stream (stream identity required)
  capsule   fetch the bounded semantic capsule
  query     invoke one allowlisted read-only resident RPC

Exit codes: 0 ok; 2 usage/workspace error; 3 foreign/refused; 4 start
failed or timed out; 5 host binary unavailable; 6 resident RPC error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import NoReturn

STATUS_SCHEMA = "mncs.language-service.resident-status/1"
LEASE_SCHEMA = "mncs.language-service.lease/1"
SOCKET_NAME = "mnls-language-service.sock"
LEASE_NAME = "mnls-language-service.json"
HOST_BINARY = "mnls-language-service-host"

#: Resident RPCs reachable through `query`. Read-only projections only:
#: lifecycle writes, buffer edits, and unbounded source dumps are excluded.
QUERY_ALLOWLIST = frozenset({
    "service_status",
    "workspace_status",
    "semantic_capsule",
    "document_diagnostics",
    "subjects_at",
    "debug_source_binding",
    "describe_identity",
    "describe_position",
    "definition",
    "references",
    "document_symbols",
    "workspace_symbols",
    "dependencies",
    "dependents",
    "semantic_impact",
    "obligations",
    "native_obligations",
    "native_kind_count",
    "context_packet",
    "language_capabilities",
    "family_agent_context",
    "hover",
    "highlights",
    "signature_help",
    "declaration",
    "type_definition",
    "prepare_call_hierarchy",
    "incoming_calls",
    "outgoing_calls",
    "semantic_tokens",
    "folding_ranges",
    "selection_ranges",
    "inlay_hints",
    "completion",
    "buffer_version",
})

MAX_RPC_BYTES = 4 * 1024 * 1024


def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def provider_repo() -> Path:
    return Path(__file__).resolve().parent.parent


def fail(message: str, code: int) -> "NoReturn":
    sys.stderr.write(f"mnls-provider: {message}\n")
    raise SystemExit(code)


def resolve_workspace(raw: str) -> Path:
    root = Path(raw).expanduser()
    if not root.is_absolute():
        fail(f"workspace must be an absolute path: {raw}", 2)
    try:
        resolved = root.resolve()
    except OSError as error:
        fail(f"workspace is unavailable: {error}", 2)
    if not resolved.is_dir():
        fail(f"workspace is not a directory: {resolved}", 2)
    return resolved


def socket_path(workspace: Path) -> Path:
    return workspace / ".mncs" / SOCKET_NAME


def lease_path(workspace: Path) -> Path:
    return workspace / ".mncs" / LEASE_NAME


def read_lease(workspace: Path) -> dict | None:
    try:
        raw = lease_path(workspace).read_bytes()
    except OSError:
        return None
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def write_lease(workspace: Path, value: dict) -> None:
    target = lease_path(workspace)
    target.parent.mkdir(parents=True, exist_ok=True)
    encoded = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    tmp = target.parent / f".{target.name}.{os.getpid()}.tmp"
    tmp.write_bytes(encoded)
    os.replace(tmp, target)


def pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def host_command() -> list[str]:
    """Provider-owned host discovery: explicit env, own build tree, PATH."""
    override = os.environ.get("MNLS_LANGUAGE_SERVICE_HOST")
    if override:
        return shlex.split(override)
    repo = provider_repo()
    for profile in ("release", "debug"):
        candidate = repo / "target" / profile / HOST_BINARY
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return [str(candidate)]
    found = shutil.which(HOST_BINARY)
    if found:
        return [found]
    return []


def language_toolchain() -> dict:
    """Best-effort toolchain measurement for identity binding.

    Preference order is binding-resolved compiler ($MNCS), explicit
    language root, then the provider repo's sibling checkout. Explicit
    environment values are echoed verbatim (never existence-checked):
    the resident host binds the same strings, so measurement and host
    agree by construction. Only derived guesses are validated. Absence
    is reported, never invented: the resident host runs without
    library bindings, and only library-backed kernels degrade.
    """
    language_root: str | None = None
    mncs_bin = os.environ.get("MNCS")
    if mncs_bin and Path(mncs_bin).is_file():
        try:
            parents = Path(mncs_bin).resolve().parents
            # <root>/target/<profile>/mncs -> <root>
            language_root = str(parents[2])
        except IndexError:
            language_root = None
    if language_root is None:
        explicit = os.environ.get("MNCS_LANGUAGE_ROOT")
        if explicit:
            language_root = explicit
    if language_root is None:
        sibling = provider_repo().parent / "mncs-language"
        if sibling.is_dir():
            language_root = str(sibling.resolve())
    library_path: str | None = None
    explicit_library = os.environ.get("MNCS_LIBRARY_PATH")
    if explicit_library:
        library_path = explicit_library
    elif language_root is not None:
        candidate = Path(language_root) / "library"
        if candidate.is_dir():
            library_path = str(candidate)
    pinned = os.environ.get("MNLS_TOOLCHAIN_IDENTITY")
    if pinned is None and language_root is not None:
        try:
            completed = subprocess.run(
                ["git", "-C", language_root, "rev-parse", "HEAD"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                timeout=5, check=False, text=True,
            )
            head = (completed.stdout or "").strip()
            if head:
                pinned = f"mncs-language:{head}"
        except (OSError, subprocess.SubprocessError):
            pinned = None
    digest = hashlib.sha256(b"mncs.language-service.toolchain/1\n")
    for field in (language_root or "", library_path or "", pinned or ""):
        digest.update(field.encode("utf-8"))
        digest.update(b"\x00")
    return {
        "language_root": language_root,
        "library_path": library_path,
        "pinned": pinned,
        "digest": f"sha256:{digest.hexdigest()}",
    }


def rpc_call(sock: Path, method: str, params: dict, timeout: float) -> dict:
    request = json.dumps({"id": 1, "method": method, "params": params}) + "\n"
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(timeout)
    try:
        client.connect(str(sock))
        client.sendall(request.encode("utf-8"))
        chunks: list[bytes] = []
        total = 0
        while True:
            chunk = client.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_RPC_BYTES:
                raise RuntimeError("resident response exceeds the provider bound")
            if b"\n" in chunk:
                break
    finally:
        client.close()
    try:
        response = json.loads(b"".join(chunks).decode("utf-8"))
    except ValueError as error:
        raise RuntimeError(f"resident response is not JSON: {error}") from error
    if not isinstance(response, dict) or not response.get("ok"):
        detail = response.get("error") if isinstance(response, dict) else None
        raise RuntimeError(f"resident refused {method}: {detail}")
    result = response.get("result")
    if not isinstance(result, dict):
        raise RuntimeError(f"resident {method} returned a malformed result")
    return result


def probe(workspace: Path, timeout: float) -> dict:
    """Read-only resident probe with identity verification.

    Returns (state, service_status_doc_or_None, detail). The socket is
    trusted only when it reports this exact canonical workspace root.
    """
    sock = socket_path(workspace)
    if not sock.exists():
        return ("absent", None, "no resident socket at the provider-owned path")
    try:
        observed = rpc_call(sock, "service_status", {}, timeout)
    except (OSError, RuntimeError) as error:
        return ("unreachable", None, f"resident socket did not answer: {error}")
    reported = observed.get("workspace_root")
    if not isinstance(reported, str):
        return ("foreign", observed, "resident reports no workspace root")
    try:
        if Path(reported).resolve() != workspace:
            return ("foreign", observed,
                    f"resident workspace root is not {workspace}")
    except OSError:
        return ("foreign", observed, "resident workspace root does not resolve")
    measured = language_toolchain()
    reported_digest = observed.get("toolchain_digest")
    if (isinstance(reported_digest, str) and reported_digest != measured["digest"]):
        return ("stale-toolchain", observed,
                "resident toolchain binding differs from the measured toolchain")
    return ("ready", observed, "provider readiness contract satisfied")


def status_document(workspace: Path, timeout: float) -> dict:
    state, observed, detail = probe(workspace, timeout)
    measured = language_toolchain()
    lease = read_lease(workspace)
    document: dict = {
        "schema_version": STATUS_SCHEMA,
        "ready": state == "ready",
        "state": state,
        "detail": detail,
        "observed_at": utcnow(),
        "selected": {
            "workspace_root": str(workspace),
            "toolchain": measured,
        },
        "socket": str(socket_path(workspace)),
        "lease": {
            "path": str(lease_path(workspace)),
            "present": lease is not None,
            "owned": bool(isinstance(lease, dict) and lease.get("owned_by_provider")),
            "pid": lease.get("pid") if isinstance(lease, dict) else None,
            "pid_alive": pid_alive(int(lease.get("pid") or 0)) if isinstance(lease, dict) else False,
        },
    }
    if observed is not None:
        service = observed.get("service", {}) if isinstance(observed, dict) else {}
        diagnostics = observed.get("diagnostics", {}) if isinstance(observed, dict) else {}
        obligations = observed.get("obligations", {}) if isinstance(observed, dict) else {}
        document["service"] = {
            "instance_id": service.get("instance_id"),
            "pid": service.get("pid"),
            "version": service.get("version"),
            "build_fingerprint": service.get("build_fingerprint"),
        }
        # Doctor persists `observed`/`selected` into service observations,
        # so the ambient semantic pass can compare generations without
        # re-invoking the probe on quiet entries.
        document["observed"] = {
            "generation": observed.get("generation"),
            "stream_identity": observed.get("stream_identity"),
            "event_cursor": observed.get("event_cursor"),
            "documents": observed.get("documents"),
            "diagnostics_error": diagnostics.get("error"),
            "diagnostics_warning": diagnostics.get("warning"),
            "obligations_fail": obligations.get("fail"),
            "obligations_unknown": obligations.get("unknown"),
        }
        readiness = observed.get("readiness", {}) if isinstance(observed, dict) else {}
        if isinstance(readiness, dict) and readiness.get("reasons"):
            document["provider_reasons"] = list(readiness["reasons"])[:8]
    return document


def cmd_status(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    print(json.dumps(status_document(workspace, args.timeout), indent=2, sort_keys=True))
    return 0


def terminate_owned(workspace: Path, lease: dict | None, timeout: float) -> dict:
    """Stop only a provider-owned lease bound to this workspace."""
    sock = socket_path(workspace)
    if not isinstance(lease, dict) or not lease.get("owned_by_provider"):
        return {"state": "not-owned", "detail": "no provider-owned lease for this workspace"}
    if lease.get("workspace_root") != str(workspace):
        return {"state": "refused", "detail": "lease names a different workspace"}
    pid = lease.get("pid")
    pid = int(pid) if isinstance(pid, int) and pid > 0 else 0
    if pid and pid_alive(pid):
        # Confirm the live socket belongs to the leased instance before
        # signaling; a foreign host on our path is never killed. A lease
        # without an instance id (interrupted start) is adopted only when
        # the live socket reports this exact workspace.
        try:
            observed = rpc_call(sock, "service_status", {}, timeout)
            service = observed.get("service", {})
            if lease.get("instance_id") is None:
                reported = observed.get("workspace_root")
                if not isinstance(reported, str) or Path(reported).resolve() != workspace:
                    return {"state": "refused",
                            "detail": "live resident does not report this workspace"}
            elif service.get("instance_id") != lease.get("instance_id"):
                return {"state": "refused",
                        "detail": "live resident instance does not match the owned lease"}
        except (OSError, RuntimeError):
            pass
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            pass
        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline and pid_alive(pid):
            time.sleep(0.05)
        if pid_alive(pid):
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
    try:
        sock.unlink(missing_ok=True)
    except OSError:
        pass
    try:
        lease_path(workspace).unlink(missing_ok=True)
    except OSError:
        pass
    return {"state": "stopped", "detail": "provider-owned lease stopped"}


def cmd_stop(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    lease = read_lease(workspace)
    result = terminate_owned(workspace, lease, args.timeout)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["state"] in ("stopped", "not-owned") else 3


def cmd_ensure(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    state, observed, detail = probe(workspace, args.timeout)
    if state == "ready":
        assert observed is not None
        print(json.dumps({
            "state": "attached",
            "detail": detail,
            "generation": observed.get("generation"),
            "stream_identity": observed.get("stream_identity"),
            "event_cursor": observed.get("event_cursor"),
        }, indent=2, sort_keys=True))
        return 0
    if state == "foreign":
        print(json.dumps({"state": "refused", "detail": detail},
                         indent=2, sort_keys=True))
        return 3
    command = host_command()
    if not command:
        print(json.dumps({
            "state": "host-unavailable",
            "detail": "mnls-language-service-host is not built or configured",
            "build_hint": {
                "cwd": str(provider_repo()),
                "argv": ["cargo", "build", "-p", "mncs-service-core",
                         "--bin", HOST_BINARY],
            },
        }, indent=2, sort_keys=True))
        return 5
    # Bounded reconcile: stop only what we own, then start one host.
    # A live socket without an owned lease is never killed: it belongs to
    # another supervisor (or an operator) and must be stopped explicitly.
    lease = read_lease(workspace)
    if state == "stale-toolchain":
        if not (isinstance(lease, dict) and lease.get("owned_by_provider")):
            print(json.dumps({
                "state": "refused",
                "detail": ("resident toolchain is stale but the live host is not "
                           "provider-owned; stop it explicitly"),
            }, indent=2, sort_keys=True))
            return 3
        stopped = terminate_owned(workspace, lease, args.timeout)
        if stopped["state"] == "refused":
            print(json.dumps(stopped, indent=2, sort_keys=True))
            return 3
    elif state == "unreachable":
        # Dead socket file: reclaim the path, plus an owned lease if any.
        if isinstance(lease, dict) and lease.get("owned_by_provider"):
            stopped = terminate_owned(workspace, lease, args.timeout)
            if stopped["state"] == "refused":
                print(json.dumps(stopped, indent=2, sort_keys=True))
                return 3
        else:
            try:
                socket_path(workspace).unlink(missing_ok=True)
            except OSError:
                pass
    elif lease is not None:
        stopped = terminate_owned(workspace, lease, args.timeout)
        if stopped["state"] == "refused":
            print(json.dumps(stopped, indent=2, sort_keys=True))
            return 3
    else:
        try:
            socket_path(workspace).unlink(missing_ok=True)
        except OSError:
            pass
    measured = language_toolchain()
    sock = socket_path(workspace)
    log_path = workspace / ".mncs" / "mnls-language-service.log"
    env = dict(os.environ)
    env["MNLS_WORKSPACE_ROOT"] = str(workspace)
    env["MNLS_SERVICE_SOCKET"] = str(sock)
    if measured["language_root"]:
        env.setdefault("MNCS_LANGUAGE_ROOT", measured["language_root"])
    if measured["library_path"]:
        env.setdefault("MNCS_LIBRARY_PATH", measured["library_path"])
    if measured["pinned"]:
        env["MNLS_TOOLCHAIN_IDENTITY"] = measured["pinned"]
    log_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        with log_path.open("ab") as log:
            process = subprocess.Popen(
                command, cwd=str(workspace), env=env,
                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                start_new_session=True, close_fds=True,
            )
    except OSError as error:
        print(json.dumps({"state": "start-failed", "detail": str(error)},
                         indent=2, sort_keys=True))
        return 4
    write_lease(workspace, {
        "schema_version": LEASE_SCHEMA,
        "owned_by_provider": True,
        "pid": process.pid,
        "instance_id": None,
        "workspace_root": str(workspace),
        "toolchain": measured,
        "started_at": utcnow(),
    })
    deadline = time.monotonic() + args.start_timeout
    last_detail = "resident did not become ready"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            try:
                sock.unlink(missing_ok=True)
            except OSError:
                pass
            try:
                lease_path(workspace).unlink(missing_ok=True)
            except OSError:
                pass
            print(json.dumps({"state": "start-failed", "detail": last_detail},
                             indent=2, sort_keys=True))
            return 4
        state, observed, detail = probe(workspace, args.timeout)
        last_detail = detail
        if state == "ready":
            assert observed is not None
            service = observed.get("service", {})
            lease = read_lease(workspace) or {}
            lease["instance_id"] = service.get("instance_id")
            write_lease(workspace, lease)
            print(json.dumps({
                "state": "started",
                "detail": detail,
                "pid": process.pid,
                "generation": observed.get("generation"),
                "stream_identity": observed.get("stream_identity"),
                "event_cursor": observed.get("event_cursor"),
            }, indent=2, sort_keys=True))
            return 0
        time.sleep(0.1)
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.SubprocessError:
            process.kill()
    print(json.dumps({"state": "start-timeout", "detail": last_detail},
                     indent=2, sort_keys=True))
    return 4


def require_ready(workspace: Path, timeout: float) -> None:
    state, _, detail = probe(workspace, timeout)
    if state != "ready":
        fail(f"resident service is {state}: {detail}", 6 if state != "foreign" else 3)


def cmd_poll(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    require_ready(workspace, args.timeout)
    params: dict = {"after_cursor": args.after, "max_events": args.max}
    if args.stream is not None:
        params["stream_identity"] = args.stream
    try:
        result = rpc_call(socket_path(workspace), "poll_events", params, args.timeout)
    except (OSError, RuntimeError) as error:
        fail(str(error), 6)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


def cmd_capsule(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    require_ready(workspace, args.timeout)
    params: dict = {"known_cursor": args.after}
    if args.stream is not None:
        params["known_stream_identity"] = args.stream
    try:
        result = rpc_call(socket_path(workspace), "semantic_capsule", params, args.timeout)
    except (OSError, RuntimeError) as error:
        fail(str(error), 6)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


def cmd_query(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    if args.method not in QUERY_ALLOWLIST:
        fail(f"method is not an allowlisted read-only query: {args.method}", 2)
    try:
        params = json.loads(args.params) if args.params else {}
    except ValueError as error:
        fail(f"params are not JSON: {error}", 2)
    if not isinstance(params, dict):
        fail("params must be a JSON object", 2)
    require_ready(workspace, args.timeout)
    try:
        result = rpc_call(socket_path(workspace), args.method, params, args.timeout)
    except (OSError, RuntimeError) as error:
        fail(str(error), 6)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="mnls-provider",
                                     description=__doc__)
    parser.add_argument("--timeout", type=float, default=20.0,
                        help="resident RPC timeout in seconds")
    sub = parser.add_subparsers(dest="command", required=True)
    status = sub.add_parser("status", help="read-only resident probe")
    status.add_argument("--workspace", required=True)
    status.set_defaults(func=cmd_status)
    ensure = sub.add_parser("ensure", help="bounded resident reconcile")
    ensure.add_argument("--workspace", required=True)
    ensure.add_argument("--start-timeout", type=float, default=30.0)
    ensure.set_defaults(func=cmd_ensure)
    stop = sub.add_parser("stop", help="stop an owned lease")
    stop.add_argument("--workspace", required=True)
    stop.set_defaults(func=cmd_stop)
    poll = sub.add_parser("poll", help="resume the resident event stream")
    poll.add_argument("--workspace", required=True)
    poll.add_argument("--stream", default=None)
    poll.add_argument("--after", type=int, default=0)
    poll.add_argument("--max", type=int, default=32)
    poll.set_defaults(func=cmd_poll)
    capsule = sub.add_parser("capsule", help="fetch the semantic capsule")
    capsule.add_argument("--workspace", required=True)
    capsule.add_argument("--stream", default=None)
    capsule.add_argument("--after", type=int, default=0)
    capsule.set_defaults(func=cmd_capsule)
    query = sub.add_parser("query", help="invoke an allowlisted read-only RPC")
    query.add_argument("--workspace", required=True)
    query.add_argument("--method", required=True)
    query.add_argument("--params", default="")
    query.set_defaults(func=cmd_query)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
