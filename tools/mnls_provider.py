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
import errno
import hashlib
import json
import os
import select
import shlex
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


def resolve_repository_roots(workspace: Path, raw: str | None) -> list[Path]:
    """Validate the exact selected repository roots supplied by Environment.

    Direct provider use defaults to the workspace itself. Environment callers
    pass a stable JSON list of selected checkouts so a resident bound to a
    broad workspace cannot silently scan unrelated repositories.
    """
    if raw is None:
        return [workspace]
    try:
        values = json.loads(raw)
    except ValueError as error:
        fail(f"repository roots are not JSON: {error}", 2)
    if not isinstance(values, list) or not values or any(not isinstance(item, str) for item in values):
        fail("repository roots must be a nonempty JSON array of absolute paths", 2)
    roots: list[Path] = []
    for value in values:
        candidate = Path(value)
        if not candidate.is_absolute():
            fail(f"selected repository root is not absolute: {value}", 2)
        try:
            candidate = candidate.resolve(strict=True)
        except OSError as error:
            fail(f"selected repository root is unavailable: {value}: {error}", 2)
        if not candidate.is_dir() or not candidate.is_relative_to(workspace):
            fail(f"selected repository root escapes workspace or is not a directory: {candidate}", 2)
        roots.append(candidate)
    if len(set(roots)) != len(roots):
        fail("selected repository roots contain duplicates", 2)
    return sorted(roots)


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
    except OSError as error:
        # A denied signal probe means the PID exists but this process cannot
        # inspect or control it. Treating it as dead could unlink its lease.
        return error.errno in (errno.EPERM, errno.EACCES)
    return True


def current_process_identity(pid: int) -> dict | None:
    """Bind a lease PID to namespace, start time, and exact executable bytes."""
    proc = Path("/proc") / str(pid)
    try:
        raw_stat = (proc / "stat").read_text()
        close = raw_stat.rfind(")")
        fields = raw_stat[close + 1:].split()
        start_ticks = int(fields[19])
        pid_namespace = (proc / "ns/pid").stat().st_ino
        executable = os.readlink(proc / "exe")
        digest = hashlib.sha256()
        with (proc / "exe").open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
    except (OSError, IndexError, ValueError):
        return None
    return {
        "pid": pid,
        "pid_namespace_inode": pid_namespace,
        "start_ticks": start_ticks,
        "executable": executable,
        "executable_sha256": f"sha256:{digest.hexdigest()}",
    }


def pidfd_exited(pidfd: int, timeout: float) -> bool:
    poller = select.poll()
    poller.register(pidfd, select.POLLIN | select.POLLHUP | select.POLLERR)
    return bool(poller.poll(max(1, int(timeout * 1000))))


def current_pid_namespace_inode() -> int | None:
    try:
        return (Path("/proc/self/ns/pid").stat().st_ino)
    except OSError:
        return None


def lease_process_liveness(lease: dict | None) -> tuple[bool | None, str]:
    if not isinstance(lease, dict):
        return False, "no-lease"
    pid = lease.get("pid")
    identity = lease.get("process_identity")
    if not isinstance(pid, int) or pid <= 0:
        return False, "no-leased-pid"
    if not isinstance(identity, dict):
        return None, "process-identity-unrecorded"
    current_namespace = current_pid_namespace_inode()
    leased_namespace = identity.get("pid_namespace_inode")
    if current_namespace is None:
        return None, "current-pid-namespace-unavailable"
    if leased_namespace != current_namespace:
        return None, "outside-current-pid-namespace"
    if not pid_alive(pid):
        return False, "pid-not-live"
    try:
        raw_stat = (Path("/proc") / str(pid) / "stat").read_text()
        fields = raw_stat[raw_stat.rfind(")") + 1:].split()
        start_ticks = int(fields[19])
    except (OSError, IndexError, ValueError):
        return None, "pid-identity-unreadable"
    if identity.get("start_ticks") != start_ticks:
        return False, "pid-reused-or-replaced"
    return True, "same-process-instance"


def host_command() -> list[str]:
    """Resolve only the selected host or this provider checkout's build."""
    override = os.environ.get("MNLS_LANGUAGE_SERVICE_HOST")
    if override:
        return shlex.split(override)
    repo = provider_repo()
    for profile in ("release", "debug"):
        candidate = repo / "target" / profile / HOST_BINARY
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return [str(candidate)]
    return []


def recent_host_log(workspace: Path, limit: int = 4096) -> str:
    """Read a bounded tail of the provider-owned host log for failed starts."""
    path = workspace / ".mncs" / "mnls-language-service.log"
    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - limit))
            value = stream.read(limit).decode("utf-8", errors="replace")
    except OSError:
        return ""
    return "\n".join(line for line in value.splitlines() if line.strip())[-limit:]


def startup_recovery(detail: str, socket: Path) -> dict | None:
    """Classify host socket failures without guessing at other start errors."""
    lines = detail.splitlines()
    listen_indexes = [index for index, line in enumerate(lines)
                      if "resident service listening at" in line.lower()]
    # Only classify the tail after the most recent listen attempt. A later
    # successful start supersedes an older failure still present in the log.
    latest_attempt = ("\n".join(lines[listen_indexes[-1]:])
                      if listen_indexes else detail)
    lowered = latest_attempt.lower()
    socket_context = ("resident service listening at" in lowered
                      or "bind:" in lowered or str(socket).lower() in lowered)
    if not socket_context:
        return None
    if "operation not permitted" in lowered:
        code = "socket-bind-denied"
        action = ("Run the selected Language Service host in a process context "
                  "that permits AF_UNIX bind on the selected socket path.")
    elif "read-only file system" in lowered:
        code = "socket-path-read-only"
        action = "Make the selected workspace socket directory writable to the host process."
    elif "permission denied" in lowered:
        code = "socket-path-permission-denied"
        action = "Grant the selected host permission to bind its provider-owned workspace socket."
    else:
        return None
    return {
        "schema_version": "mncs.language-service.recovery/1",
        "disposition": "operator-action-required",
        "code": code,
        "socket_path": str(socket),
        "automatic_remediation": False,
        "action": action,
    }


def socket_access_recovery(detail: str, socket: Path) -> dict:
    return {
        "schema_version": "mncs.language-service.recovery/1",
        "disposition": "operator-action-required",
        "code": "socket-connect-denied",
        "socket_path": str(socket),
        "automatic_remediation": False,
        "action": ("Run status/reconcile from a process context allowed to connect to the "
                   "selected AF_UNIX socket. Preserve the lease and socket until the "
                   "resident identity can be verified."),
        "detail": detail[-1000:],
    }


def socket_unverified_recovery(detail: str, socket: Path) -> dict:
    return {
        "schema_version": "mncs.language-service.recovery/1",
        "disposition": "operator-action-required",
        "code": "resident-state-unverified",
        "socket_path": str(socket),
        "automatic_remediation": False,
        "action": ("Verify the resident and socket from an allowed process context. "
                   "Do not remove the lease or socket until the resident is proven stale."),
        "detail": detail[-1000:],
    }


def process_identity_recovery(detail: str, socket: Path) -> dict:
    return {
        "schema_version": "mncs.language-service.recovery/1",
        "disposition": "operator-action-required",
        "code": "resident-process-identity-unverified",
        "socket_path": str(socket),
        "automatic_remediation": False,
        "action": ("Verify the provider-owned resident from its PID namespace, then "
                   "reconcile its socket and lease without signaling an unverified PID."),
        "detail": detail[-1000:],
    }


def start_failure_detail(workspace: Path, fallback: str) -> str:
    tail = recent_host_log(workspace)
    return f"{fallback}; host log tail: {tail}" if tail else fallback


def selected_host_identity() -> dict:
    """Return exact selected executable bytes; never infer identity from path."""
    command = host_command()
    if not command:
        return {"command": [], "executable": None, "build_fingerprint": "unknown"}
    raw = command[0]
    candidate = Path(raw).expanduser()
    if not candidate.is_absolute() and os.sep not in raw:
        # A bare executable is accepted only when explicitly selected; resolve
        # it against PATH here so its bytes, rather than its name, are bound.
        resolved = next((Path(directory) / raw for directory in os.environ.get("PATH", "").split(os.pathsep)
                         if (Path(directory) / raw).is_file()), None)
        candidate = resolved if resolved is not None else candidate
    try:
        executable = candidate.resolve(strict=True)
        digest = hashlib.sha256(executable.read_bytes()).hexdigest()
    except OSError:
        return {"command": command, "executable": None, "build_fingerprint": "unknown"}
    return {
        "command": command,
        "executable": str(executable),
        "build_fingerprint": f"sha256:{digest}",
    }


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


def rpc_call_raw(sock: Path, method: str, params: dict, timeout: float):
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
    return response.get("result")


def rpc_call(sock: Path, method: str, params: dict, timeout: float) -> dict:
    result = rpc_call_raw(sock, method, params, timeout)
    if not isinstance(result, dict):
        raise RuntimeError(f"resident {method} returned a malformed result")
    return result


def probe(workspace: Path, timeout: float, repository_roots: list[Path] | None = None) -> dict:
    """Resident probe with identity verification and disk convergence.

    Returns (state, service_status_doc_or_None, detail). The socket is
    trusted only when it reports this exact canonical workspace root.

    The probe first reconciles the resident to disk truth through
    ``refresh_workspace``: shell-made edits (outside LSP) would
    otherwise stay invisible, and a probe reporting stale state as
    current is a correctness bug. Refresh is idempotent and bounded
    (quiet workspaces pay reads only; only actual changes analyze),
    converges state rather than diverging it, and mutates no source,
    so the probe keeps read effects. A refused refresh (a refresh is
    already running) is tolerated; the status read still proceeds.
    """
    expected_roots = repository_roots or [workspace]
    sock = socket_path(workspace)
    if not sock.exists():
        return ("absent", None, "no resident socket at the provider-owned path")
    try:
        rpc_call_raw(sock, "refresh_workspace", {}, timeout)
    except (OSError, RuntimeError):
        pass
    try:
        observed = rpc_call(sock, "service_status", {}, timeout)
    except OSError as error:
        if error.errno in (errno.EPERM, errno.EACCES):
            return ("access-denied", None,
                    f"resident socket access denied: {error}")
        if error.errno in (errno.ECONNREFUSED, errno.ENOENT):
            return ("unreachable", None, f"resident socket did not answer: {error}")
        return ("unverified", None,
                f"resident status could not be verified safely: {error}")
    except RuntimeError as error:
        return ("unverified", None,
                f"resident status returned an invalid or refused response: {error}")
    reported = observed.get("workspace_root")
    if not isinstance(reported, str):
        return ("foreign", observed, "resident reports no workspace root")
    try:
        if Path(reported).resolve() != workspace:
            return ("foreign", observed,
                    f"resident workspace root is not {workspace}")
    except OSError:
        return ("foreign", observed, "resident workspace root does not resolve")
    reported_roots = observed.get("workspace_repository_roots")
    normalized_reported_roots: list[Path] = []
    if isinstance(reported_roots, list) and all(isinstance(item, str) for item in reported_roots):
        try:
            normalized_reported_roots = sorted(Path(item).resolve() for item in reported_roots)
        except OSError:
            normalized_reported_roots = []
    if normalized_reported_roots != expected_roots:
        return ("stale-roots", observed,
                "resident repository-root selection differs from the selected Environment checkouts")
    measured = language_toolchain()
    reported_digest = observed.get("toolchain_digest")
    if (isinstance(reported_digest, str) and reported_digest != measured["digest"]):
        return ("stale-toolchain", observed,
                "resident toolchain binding differs from the measured toolchain")
    selected_build = selected_host_identity()["build_fingerprint"]
    service = observed.get("service", {})
    resident_build = service.get("build_fingerprint") if isinstance(service, dict) else None
    if selected_build == "unknown" or not isinstance(resident_build, str) or resident_build == "unknown":
        return ("build-unverified", observed,
                "selected or resident executable bytes could not be verified")
    if selected_build != resident_build:
        return ("stale-build", observed,
                "resident executable bytes differ from the selected provider build")
    return ("ready", observed, "provider readiness contract satisfied")


def status_document(workspace: Path, timeout: float, repository_roots: list[Path] | None = None) -> dict:
    selected_roots = repository_roots or [workspace]
    state, observed, detail = probe(workspace, timeout, selected_roots)
    measured = language_toolchain()
    selected_host = selected_host_identity()
    lease = read_lease(workspace)
    leased_pid_alive, lease_pid_state = lease_process_liveness(lease)
    document: dict = {
        "schema_version": STATUS_SCHEMA,
        "ready": state == "ready",
        "state": state,
        "detail": detail,
        "observed_at": utcnow(),
        "selected": {
            "workspace_root": str(workspace),
            "repository_roots": [str(path) for path in selected_roots],
            "toolchain": measured,
            "host": selected_host,
        },
        "socket": str(socket_path(workspace)),
        "lease": {
            "path": str(lease_path(workspace)),
            "present": lease is not None,
            "owned": bool(isinstance(lease, dict) and lease.get("owned_by_provider")),
            "pid": lease.get("pid") if isinstance(lease, dict) else None,
            "pid_alive": leased_pid_alive,
            "pid_identity_state": lease_pid_state,
        },
    }
    if state != "ready":
        if state == "access-denied":
            document["recovery"] = socket_access_recovery(detail, socket_path(workspace))
        elif state == "unverified":
            document["recovery"] = socket_unverified_recovery(detail, socket_path(workspace))
        else:
            host_log = recent_host_log(workspace)
            recovery = startup_recovery(host_log, socket_path(workspace))
            if recovery is not None:
                document["detail"] = f"{detail}; last host startup: {host_log[-1000:]}"
                document["recovery"] = recovery
    if observed is not None:
        service = observed.get("service", {}) if isinstance(observed, dict) else {}
        diagnostics = observed.get("diagnostics", {}) if isinstance(observed, dict) else {}
        obligations = observed.get("obligations", {}) if isinstance(observed, dict) else {}
        document["service"] = {
            "instance_id": service.get("instance_id"),
            "pid": service.get("pid"),
            "version": service.get("version"),
            "executable": service.get("executable"),
            "build_fingerprint": service.get("build_fingerprint"),
        }
        # Doctor persists `observed`/`selected` into service observations,
        # so the ambient semantic pass can compare generations without
        # re-invoking the probe on quiet entries.
        document["observed"] = {
            "event_transport": {"identity": "mncs-language-service:" + str(workspace),
                "service_identity": "mncs-language-service:" + str(workspace),
                "provider": "mncs-language-service", "protocol": "mncs.workspace-event-cursor/2",
                "socket": str(socket_path(workspace)), "workspace": str(workspace)},
            "generation": observed.get("generation"),
            "stream_identity": observed.get("stream_identity"),
            "event_cursor": observed.get("event_cursor"),
            "documents": observed.get("documents"),
            "workspace_repository_roots": observed.get("workspace_repository_roots"),
            "analysis_pending": observed.get("analysis_pending"),
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
    roots = resolve_repository_roots(workspace, args.repository_roots_json)
    print(json.dumps(status_document(workspace, args.timeout, roots), indent=2, sort_keys=True))
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
    live_pid = bool(pid and pid_alive(pid))
    if live_pid:
        if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            detail = "this runtime cannot pin the leased process identity safely"
            return {"state": "refused", "detail": detail,
                    "recovery": process_identity_recovery(detail, sock)}
        try:
            pidfd = os.pidfd_open(pid, 0)
        except OSError as error:
            detail = f"cannot pin the leased resident process: {error}"
            return {"state": "refused", "detail": detail,
                    "recovery": process_identity_recovery(detail, sock)}
        try:
            result = _terminate_pinned_owned(workspace, lease, pid, pidfd, timeout)
            if result["state"] != "stopped":
                return result
        finally:
            os.close(pidfd)
    elif pid:
        process_identity = lease.get("process_identity")
        current_namespace = current_pid_namespace_inode()
        if (not isinstance(process_identity, dict)
                or process_identity.get("pid_namespace_inode") is None
                or current_namespace is None
                or process_identity.get("pid_namespace_inode") != current_namespace):
            detail = ("cannot prove the leased PID is gone from this process namespace; "
                      "preserving its lease and socket")
            return {"state": "refused", "detail": detail,
                    "recovery": process_identity_recovery(detail, sock)}
    if not live_pid and sock.exists():
        # A PID outside this process namespace can look dead here. Only an
        # explicit connection-refused/missing-path result proves that an
        # existing socket no longer has a listener; access denial or an
        # unrecognized response must preserve both lease and socket.
        try:
            observed = rpc_call(sock, "service_status", {}, timeout)
        except OSError as error:
            if error.errno not in (errno.ECONNREFUSED, errno.ENOENT):
                return {"state": "refused",
                        "detail": f"cannot establish whether the leased resident is alive: {error}"}
        except RuntimeError as error:
            return {"state": "refused",
                    "detail": f"cannot establish whether the leased resident is alive: {error}"}
        else:
            service = observed.get("service", {})
            reported = observed.get("workspace_root")
            if (not isinstance(reported, str) or Path(reported).resolve() != workspace
                    or (lease.get("instance_id") is not None
                        and service.get("instance_id") != lease.get("instance_id"))):
                return {"state": "refused",
                        "detail": "socket belongs to a different or unverifiable resident"}
            return {"state": "refused",
                    "detail": "resident is reachable but its PID is outside this process namespace"}
    try:
        sock.unlink(missing_ok=True)
    except OSError:
        pass
    try:
        lease_path(workspace).unlink(missing_ok=True)
    except OSError:
        pass
    return {"state": "stopped", "detail": "provider-owned lease stopped"}


def _terminate_pinned_owned(workspace: Path, lease: dict, pid: int,
                             pidfd: int, timeout: float) -> dict:
    sock = socket_path(workspace)
    leased_process = lease.get("process_identity")
    observed_process = current_process_identity(pid)
    if (not isinstance(leased_process, dict) or observed_process is None
            or any(leased_process.get(key) != observed_process.get(key)
                   for key in ("pid", "pid_namespace_inode", "start_ticks",
                               "executable_sha256"))):
        detail = "provider lease does not match the current PID namespace, start time, and executable bytes"
        return {"state": "refused", "detail": detail,
                "recovery": process_identity_recovery(detail, sock)}
    # Confirm the live socket belongs to the leased instance before
    # signaling; a foreign host on our path is never killed. A lease
    # without an instance id (interrupted start) is adopted only when
    # the live socket reports this exact workspace and PID.
    try:
        observed = rpc_call(sock, "service_status", {}, timeout)
        service = observed.get("service", {})
        reported = observed.get("workspace_root")
        if not isinstance(reported, str) or Path(reported).resolve() != workspace:
            return {"state": "refused",
                    "detail": "resident does not report the leased workspace"}
        if service.get("pid") != pid:
            return {"state": "refused",
                    "detail": "resident process ID does not match the provider lease"}
        if (lease.get("instance_id") is not None
                and service.get("instance_id") != lease.get("instance_id")):
            return {"state": "refused",
                    "detail": "live resident instance does not match the owned lease"}
    except (OSError, RuntimeError) as error:
        return {"state": "refused",
                "detail": f"cannot verify the owned resident before stopping it: {error}"}
    try:
        signal.pidfd_send_signal(pidfd, signal.SIGTERM)
    except OSError as error:
        return {"state": "refused",
                "detail": f"cannot signal the verified resident process: {error}"}
    if not pidfd_exited(pidfd, 5.0):
        try:
            signal.pidfd_send_signal(pidfd, signal.SIGKILL)
        except OSError as error:
            return {"state": "refused",
                    "detail": f"verified resident did not stop and cannot be killed safely: {error}"}
        if not pidfd_exited(pidfd, 1.0):
            return {"state": "refused",
                    "detail": "leased resident did not exit; preserving its socket and lease"}
    return {"state": "stopped", "detail": "verified provider-owned resident exited"}


def cmd_stop(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    lease = read_lease(workspace)
    result = terminate_owned(workspace, lease, args.timeout)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["state"] in ("stopped", "not-owned") else 3


def cmd_ensure(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    roots = resolve_repository_roots(workspace, args.repository_roots_json)
    state, observed, detail = probe(workspace, args.timeout, roots)
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
    if state == "access-denied":
        result = {"state": "refused", "detail": detail,
                  "recovery": socket_access_recovery(detail, socket_path(workspace))}
        print(json.dumps(result, indent=2, sort_keys=True))
        return 3
    if state == "unverified":
        result = {"state": "refused", "detail": detail,
                  "recovery": socket_unverified_recovery(detail, socket_path(workspace))}
        print(json.dumps(result, indent=2, sort_keys=True))
        return 3
    if state == "build-unverified":
        print(json.dumps({
            "state": "refused",
            "detail": detail + "; rebuild evidence is required before restart",
        }, indent=2, sort_keys=True))
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
    if state in ("stale-toolchain", "stale-roots", "stale-build"):
        if not (isinstance(lease, dict) and lease.get("owned_by_provider")):
            print(json.dumps({
                "state": "refused",
                "detail": ("resident state is stale but the live host is not "
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
    env["MNLS_WORKSPACE_REPOSITORY_ROOTS_JSON"] = json.dumps(
        [str(path) for path in roots], separators=(",", ":"))
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
        detail = str(error)
        result = {"state": "start-failed", "detail": detail}
        recovery = startup_recovery(detail, socket_path(workspace))
        if recovery is not None:
            result["recovery"] = recovery
        print(json.dumps(result, indent=2, sort_keys=True))
        return 4
    write_lease(workspace, {
        "schema_version": LEASE_SCHEMA,
        "owned_by_provider": True,
        "pid": process.pid,
        "process_identity": current_process_identity(process.pid),
        "instance_id": None,
        "workspace_root": str(workspace),
        "repository_roots": [str(path) for path in roots],
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
            detail = start_failure_detail(workspace, last_detail)
            result = {"state": "start-failed", "detail": detail}
            recovery = startup_recovery(detail, sock)
            if recovery is not None:
                result["recovery"] = recovery
            print(json.dumps(result, indent=2, sort_keys=True))
            return 4
        state, observed, detail = probe(workspace, args.timeout, roots)
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
    detail = start_failure_detail(workspace, last_detail)
    result = {"state": "start-timeout", "detail": detail}
    recovery = startup_recovery(detail, sock)
    if recovery is not None:
        result["recovery"] = recovery
    print(json.dumps(result, indent=2, sort_keys=True))
    return 4


def require_ready(workspace: Path, timeout: float, repository_roots: list[Path] | None = None) -> None:
    state, _, detail = probe(workspace, timeout, repository_roots)
    if state != "ready":
        fail(f"resident service is {state}: {detail}", 6 if state != "foreign" else 3)


def cmd_poll(args: argparse.Namespace) -> int:
    workspace = resolve_workspace(args.workspace)
    require_ready(workspace, args.timeout, resolve_repository_roots(workspace, args.repository_roots_json))
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
    require_ready(workspace, args.timeout, resolve_repository_roots(workspace, args.repository_roots_json))
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
    require_ready(workspace, args.timeout, resolve_repository_roots(workspace, args.repository_roots_json))
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
    status.add_argument("--repository-roots-json", default=None)
    status.set_defaults(func=cmd_status)
    ensure = sub.add_parser("ensure", help="bounded resident reconcile")
    ensure.add_argument("--workspace", required=True)
    ensure.add_argument("--repository-roots-json", default=None)
    ensure.add_argument("--start-timeout", type=float, default=30.0)
    ensure.set_defaults(func=cmd_ensure)
    stop = sub.add_parser("stop", help="stop an owned lease")
    stop.add_argument("--workspace", required=True)
    stop.set_defaults(func=cmd_stop)
    poll = sub.add_parser("poll", help="resume the resident event stream")
    poll.add_argument("--workspace", required=True)
    poll.add_argument("--repository-roots-json", default=None)
    poll.add_argument("--stream", default=None)
    poll.add_argument("--after", type=int, default=0)
    poll.add_argument("--max", type=int, default=32)
    poll.set_defaults(func=cmd_poll)
    capsule = sub.add_parser("capsule", help="fetch the semantic capsule")
    capsule.add_argument("--workspace", required=True)
    capsule.add_argument("--repository-roots-json", default=None)
    capsule.add_argument("--stream", default=None)
    capsule.add_argument("--after", type=int, default=0)
    capsule.set_defaults(func=cmd_capsule)
    query = sub.add_parser("query", help="invoke an allowlisted read-only RPC")
    query.add_argument("--workspace", required=True)
    query.add_argument("--repository-roots-json", default=None)
    query.add_argument("--method", required=True)
    query.add_argument("--params", default="")
    query.set_defaults(func=cmd_query)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
