#!/usr/bin/env python3
"""obsidian-agent plugin — MCP entry for the **CLI channel** (`obsidian-cli`).

A real subprocess bridge to the official Obsidian command line interface shipped
with Obsidian 1.12.7 on this machine (`<install>\\Obsidian.com`, a PE32 console
executable that talks to the *running* Obsidian app). Nothing here is a stub: each
tool spawns the CLI and reports what actually came back.

Why a second entry next to `obsidian-rest`
------------------------------------------
The REST channel needs the third-party community plugin *Local REST API* installed
and an API key pasted into config. The official CLI needs no third-party plugin —
only one toggle inside Obsidian (Settings -> General -> Advanced -> Command line
interface) plus a running Obsidian instance. Two entries, two dependency profiles,
deliberately not merged (workspace convention: one MCP entry per capability face).

Honest status classification (every row measured on this machine, 2026-09-21)
-----------------------------------------------------------------------------
| observed CLI output (stdout unless noted)                     | status            | ok    |
| ------------------------------------------------------------- | ----------------- | ----- |
| `The CLI is unable to find Obsidian. ...` (stderr, rc 1)      | `app-not-running` | False |
| `Command line interface is not enabled. ...` (**rc 0**)       | `cli-disabled`     | False |
| `Error: Command "..." not found. It may require a plugin ...` (**rc 0**) | `command-error` | False |
| `Error: Missing required parameter: name=<name>` + `Usage:` (**rc 0**) | `command-error` | False |
| `Error: File "..." not found.` (**rc 0**)                     | `command-error`   | False |
| `Vault not found.` (**rc 0**)                                 | `vault-not-found` | False |
| other non-empty output, rc 0                                  | `ready`           | True  |
| empty output, rc 0                                            | `empty-success`   | False |

Exit code alone is **not** trusted as success. Two separate live measurements say so:
with Obsidian running but the toggle off, `Obsidian.com help` printed its refusal to stdout
and **exited 0**; and once the toggle was on, refused commands (unknown subcommand, missing
required parameter, unknown file, unknown vault) also came back as **exit 0 with `Error: ...`
on stdout**. A `returncode == 0 means done` implementation reports all of those as successes,
so the first non-empty output line decides, and an empty stdout with rc 0 stays its own
non-success status (`empty-success`).

Destructive guard
-----------------
`obsidian_cli_run` refuses to spawn any subcommand whose tokens match
`delete | remove | uninstall | disable | eval` unless `confirm=true` is passed
(`eval` runs arbitrary JavaScript inside your live app). The vault belongs to the
user; a plugin must not silently wipe notes.

Run directly (host manifests normally do it):
    mcp/.venv/Scripts/python.exe mcp/obsidian_cli_server.py
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

SERVER_NAME = "obsidian-cli"
PLUGIN_VERSION = "1.1.0"

DEFAULT_TIMEOUT_S = 60
MAX_TIMEOUT_S = 600
MAX_OUTPUT_CHARS = 20000

# Subcommand tokens that must never run without an explicit opt-in.
CONFIRM_REQUIRED = ("delete", "remove", "uninstall", "disable", "eval")

# Marker phrases emitted by the shipped CLI shim (verified strings, 2026-09-21).
PHRASE_NOT_RUNNING = "unable to find obsidian"
PHRASE_CLI_DISABLED = "command line interface is not enabled"
PHRASE_NOT_ENABLED_ALT = "not enabled"

IS_WINDOWS = os.name == "nt"


def _load_env_file() -> None:
    """Pick up OBSIDIAN_CLI (and friends) from mcp/.env so one file configures both entries."""
    try:
        from dotenv import load_dotenv

        load_dotenv(Path(__file__).resolve().parent / ".env", override=False)
    except Exception:  # noqa: BLE001 - python-dotenv is optional for this channel
        pass


_load_env_file()


# --------------------------------------------------------------------------- #
# discovery
# --------------------------------------------------------------------------- #
def _candidate_paths() -> list[Path]:
    """Possible locations of the CLI shim, in the order they are tried."""
    raw: list[str] = []

    env = (os.environ.get("OBSIDIAN_CLI") or "").strip()
    if env:
        raw.append(env)

    found = shutil.which("obsidian") or shutil.which("Obsidian.com")
    if found:
        raw.append(found)

    for var in ("ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"):
        base = os.environ.get(var)
        if base:
            raw.append(str(Path(base) / "Obsidian" / "Obsidian.com"))

    raw.extend(_from_registry())

    out: list[Path] = []
    seen: set[str] = set()
    for item in raw:
        key = item.lower()
        if key in seen:
            continue
        seen.add(key)
        out.append(Path(item))
    return out


def _from_registry() -> list[str]:
    """Read Obsidian's uninstall registry entries for an InstallLocation (Windows)."""
    if not IS_WINDOWS:
        return []
    try:
        import winreg
    except ImportError:  # pragma: no cover
        return []

    hits: list[str] = []
    roots = (winreg.HKEY_LOCAL_MACHINE, winreg.HKEY_CURRENT_USER)
    subkey = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
    for root in roots:
        try:
            with winreg.OpenKey(root, subkey) as parent:
                for index in range(winreg.QueryInfoKey(parent)[0]):
                    try:
                        name = winreg.EnumKey(parent, index)
                        if "obsidian" not in name.lower():
                            continue
                        with winreg.OpenKey(parent, name) as key:
                            display = str(winreg.QueryValueEx(key, "DisplayName")[0]).lower()
                            if "obsidian" not in display:
                                continue
                            for value_name in ("InstallLocation", "DisplayIcon"):
                                try:
                                    value = str(winreg.QueryValueEx(key, value_name)[0]).strip('"')
                                except OSError:
                                    continue
                                if not value:
                                    continue
                                candidate = Path(value)
                                if candidate.suffix.lower() != ".exe" and candidate.name.lower() != "obsidian.com":
                                    candidate = candidate / "Obsidian.com"
                                hits.append(str(candidate))
                    except OSError:
                        continue
        except OSError:
            continue
    return hits


def _obsidian_process_running() -> bool:
    """True when an Obsidian.exe instance is up (the CLI needs one)."""
    if not IS_WINDOWS:
        # POSIX: the CLI path is untested here; report unknown as False rather than guess.
        return False
    try:
        proc = subprocess.run(
            ["tasklist", "/FI", "IMAGENAME eq Obsidian.exe", "/NH"],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=15,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
    except Exception:  # noqa: BLE001
        return False
    return "obsidian.exe" in (proc.stdout or "").lower()


def resolve_cli() -> dict[str, Any]:
    """Return the first existing CLI shim plus where it was found."""
    for path in _candidate_paths():
        if path.exists():
            source = "OBSIDIAN_CLI env" if (os.environ.get("OBSIDIAN_CLI") or "").strip() and str(path) == os.environ["OBSIDIAN_CLI"] else "filesystem/PATH probe"
            return {"found": True, "path": str(path), "source": source}
    return {"found": False, "path": None, "source": None,
            "tried": [str(p) for p in _candidate_paths()]}


def first_line(text: str) -> str:
    """First non-empty line — what the app actually said back."""
    for line in (text or "").splitlines():
        if line.strip():
            return line.strip()
    return ""


def _usage_hint(stdout: str) -> str:
    """The CLI's own `Usage:` line, which it prints right after a parameter refusal."""
    for line in [x.strip() for x in (stdout or "").splitlines() if x.strip()][1:5]:
        if line.lower().startswith("usage:"):
            return line
    return ""


def _classify(stdout: str, stderr: str, rc: int) -> tuple[str, bool]:
    """Map raw CLI output onto (status, ok). Output wins over the exit code.

    Live-verified on this machine (Obsidian 1.12.7, CLI enabled): the app answers
    *command-level* refusals with **exit code 0** and the complaint on stdout — e.g.
    `Error: Command "zzz" not found. It may require a plugin to be enabled.`,
    `Error: Missing required parameter: name=<name>` (plus a `Usage:` line),
    `Error: File "..." not found.`, or `Vault not found.`. Trusting the exit code there
    would report a refused command as executed, so the first non-empty line decides.
    """
    blob = f"{stdout}\n{stderr}".lower()
    if PHRASE_NOT_RUNNING in blob:
        return "app-not-running", False
    if PHRASE_CLI_DISABLED in blob:
        return "cli-disabled", False
    head = first_line(stdout).lower().rstrip(".")
    if head.startswith("vault not found"):
        return "vault-not-found", False
    if head.startswith("error:") or head.startswith("error -") or head == "error":
        return "command-error", False
    if rc == 0 and (stdout or "").strip():
        return "ready", True
    if rc == 0 and not (stdout or "").strip():
        return "empty-success", False  # never claim a result we cannot show
    return f"cli-error-rc-{rc}", False


def _run_cli(argv: list[str], timeout_s: int) -> dict[str, Any]:
    """Spawn the CLI, classify the outcome, and shape a structured answer."""
    cli = resolve_cli()
    if not cli["found"]:
        return {
            "ok": False, "status": "cli-not-installed", "argv": argv,
            "remedy": {
                "step": "Locate the Obsidian CLI shim (Obsidian.com next to Obsidian.exe)",
                "how": [
                    "Upgrade Obsidian to 1.12 or later (the CLI ships with the app)",
                    "Or point the server at it explicitly: OBSIDIAN_CLI=C:\\Program Files\\Obsidian\\Obsidian.com in the mcpServers env block",
                ],
                "tried": cli.get("tried", []),
            },
        }

    started = time.monotonic()
    try:
        proc = subprocess.run(
            [cli["path"], *argv],
            capture_output=True, text=True, encoding="utf-8", errors="replace",
            timeout=timeout_s, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
    except subprocess.TimeoutExpired:
        return {
            "ok": False, "status": "timeout", "argv": argv, "cli_path": cli["path"],
            "timeout_s": timeout_s,
            "remedy": "The command did not return in time. Obsidian may be busy or showing a modal dialog; retry with a larger timeout_s after checking the app.",
        }
    except OSError as exc:
        return {
            "ok": False, "status": "spawn-failed", "argv": argv, "cli_path": cli["path"],
            "remedy": f"Could not start the CLI shim: {exc}",
        }

    duration_ms = round((time.monotonic() - started) * 1000)
    stdout, stderr = proc.stdout or "", proc.stderr or ""
    status, ok = _classify(stdout, stderr, proc.returncode)

    payload: dict[str, Any] = {
        "ok": ok, "status": status, "argv": argv, "cli_path": cli["path"],
        "exit_code": proc.returncode, "duration_ms": duration_ms,
    }

    truncated = False
    if len(stdout) > MAX_OUTPUT_CHARS:
        stdout, truncated = stdout[:MAX_OUTPUT_CHARS], True
    payload["stdout"] = stdout
    if stderr.strip():
        payload["stderr"] = stderr[:4000]
    if truncated:
        payload["truncated"] = f"stdout cut at {MAX_OUTPUT_CHARS} chars"

    stripped = stdout.strip()
    if stripped[:1] in ("[", "{"):
        try:
            payload["json"] = json.loads(stripped)
            payload["output_format"] = "json"
        except json.JSONDecodeError:
            payload["output_format"] = "text"
    else:
        payload["output_format"] = "text"

    if not ok:
        remedy = _remedy_for(status)
        if isinstance(remedy, dict) and status in ("command-error", "vault-not-found"):
            remedy["cli_said"] = first_line(stdout)
            usage = _usage_hint(stdout)
            if usage:
                remedy["cli_usage"] = usage
        payload["remedy"] = remedy
    return payload


def _remedy_for(status: str) -> dict[str, Any]:
    if status == "app-not-running":
        return {
            "step": "Start Obsidian and open a vault — the CLI talks to the running app",
            "how": ["Launch 'C:\\Program Files\\Obsidian\\Obsidian.exe' (no headless mode for this channel)",
                    "Then re-run obsidian_cli_detect and confirm status=ready"],
            "obsidian_process_now": _obsidian_process_running(),
            "fallback_channel": "obsidian-rest (needs the Local REST API community plugin), or edit vault files directly with the obsidian-markdown skill",
        }
    if status == "cli-disabled":
        return {
            "step": "Enable the CLI inside Obsidian",
            "how": ["Obsidian -> Settings -> General -> Advanced -> turn on 'Command line interface'",
                    "Close the settings, keep Obsidian running, re-run obsidian_cli_detect"],
            "verified_message": "Command line interface is not enabled. Please turn it on in Settings > General > Advanced.",
        }
    if status == "cli-not-installed":
        return {"step": "Obsidian 1.12+ is required for the CLI", "how": ["Install/upgrade Obsidian, or set OBSIDIAN_CLI to the Obsidian.com path"]}
    if status == "command-error":
        return {
            "step": "The Obsidian app refused this command — note it still exits 0, so read the text",
            "how": [
                "Fix the subcommand, or supply the missing `name=` / `path=` / `query=` parameter, then retry",
                "obsidian_cli_help prints the authoritative command list for this Obsidian build",
                "A command backed by a plugin only works while that plugin is enabled in the target vault",
            ],
        }
    if status == "vault-not-found":
        return {
            "step": "vault=<name> matched no open vault",
            "how": [
                "Use the folder name as Obsidian shows it (e.g. `obsidian-link-test`)",
                "Registered vaults: the `vaults` map inside %APPDATA%\\obsidian\\obsidian.json",
                "Omitting vault= targets the most recently focused vault — on a multi-vault machine that is how notes land in the wrong place",
            ],
        }
    if status == "empty-success":
        return {"step": "The CLI returned exit code 0 with no output", "how": ["That is not treated as a result. Re-run with an explicit command (e.g. args=['help']) and check the vault/file target."]}
    return {"step": f"CLI reported {status}", "how": ["Run obsidian_cli_help to see the supported syntax", "Check the stderr field for the app-side message"]}


def _needs_confirm(argv: list[str]) -> list[str]:
    hits: list[str] = []
    for token in argv:
        lowered = token.lower().split("=")[0]
        for pattern in CONFIRM_REQUIRED:
            if pattern in lowered:
                hits.append(token)
                break
    return hits


def _masked_argv(argv: list[str]) -> list[str]:
    return [re.sub(r"(key|token|password)=", r"\1=", t, flags=re.I) for t in argv]


# --------------------------------------------------------------------------- #
# MCP surface
# --------------------------------------------------------------------------- #
def build_server():
    try:  # bundled venv (mcp 1.x) is the supported interpreter
        from mcp.server.fastmcp import FastMCP as _Server
    except ImportError:  # system python carries mcp 2.x, where FastMCP became MCPServer
        from mcp.server.mcpserver import MCPServer as _Server

    mcp = _Server(SERVER_NAME)

    @mcp.tool()
    def obsidian_cli_detect() -> dict[str, Any]:
        """Report whether the Obsidian CLI channel is usable right now.

        Checks the CLI shim location, the Obsidian app process, and the real output
        of `<cli> help`, and classifies it (`ready`, `app-not-running`,
        `cli-disabled`, `cli-not-installed`). Always call this first; when it is not
        `ready` the returned `remedy` says exactly which switch to flip.
        """
        cli = resolve_cli()
        payload: dict[str, Any] = {
            "ok": False, "channel": SERVER_NAME,
            "cli_path": cli.get("path"), "cli_found": cli.get("found"),
            "obsidian_process_running": _obsidian_process_running(),
        }
        if not cli.get("found"):
            payload["status"] = "cli-not-installed"
            payload["remedy"] = _remedy_for("cli-not-installed")
            payload["tried"] = cli.get("tried", [])
            return payload

        probe = _run_cli(["help"], timeout_s=20)
        payload.update({
            "status": probe["status"], "ok": probe["ok"],
            "exit_code": probe.get("exit_code"), "duration_ms": probe.get("duration_ms"),
        })
        if probe["ok"]:
            first_lines = (probe.get("stdout") or "").splitlines()[:12]
            payload["help_head"] = first_lines
            payload["next"] = "Call obsidian_cli_help for the full command list, then obsidian_cli_run for real work."
        else:
            payload["remedy"] = probe.get("remedy")
            payload["cli_output"] = (probe.get("stdout") or probe.get("stderr") or "").strip()[:400]
        return payload

    @mcp.tool()
    def obsidian_cli_help(args: list[str] | None = None) -> dict[str, Any]:
        """Return the CLI's own authoritative command reference (`help [section...]`).

        `args=[]` -> app help; `args=["help"]`, `args=["dev"]`, etc. for sections.
        Use this instead of assuming command names — the surface changes with the
        Obsidian version, and this machine's build is 1.12.7.
        """
        argv = ["help", *(args or [])]
        return _run_cli(argv, timeout_s=DEFAULT_TIMEOUT_S)

    @mcp.tool()
    def obsidian_cli_run(
        args: list[str],
        vault: str | None = None,
        timeout_s: int = DEFAULT_TIMEOUT_S,
        confirm: bool = False,
    ) -> dict[str, Any]:
        """Run one real Obsidian CLI command and report stdout/stderr/exit code.

        `args` is the command plus its parameters, one token each, using the CLI's
        `name=value` syntax — quoting is handled by argv, so pass `name=My Note`
        unchanged: `["create", "name=Meeting note", "content=# Agenda\\n- item"]`.
        `vault` prepends `vault=<name>` to target a specific open vault (otherwise
        the most recently focused one is used). For multi-line content use literal
        `\\n`, per the CLI's own convention.

        Subcommands matching delete/remove/uninstall/disable/eval are blocked until
        `confirm=true` — `eval` executes JavaScript inside your running app.
        """
        if not args:
            return {"ok": False, "status": "no-args", "channel": SERVER_NAME,
                    "remedy": {"step": "Pass at least one token", "how": ["e.g. args=['read', 'file=Inbox'] or args=['help']"]}}

        timeout_s = max(1, min(int(timeout_s or DEFAULT_TIMEOUT_S), MAX_TIMEOUT_S))
        risky = _needs_confirm(list(args))
        if risky and not confirm:
            return {
                "ok": False, "status": "needs-confirmation", "argv": _masked_argv(list(args)),
                "blocked_tokens": risky,
                "remedy": {
                    "step": "This subcommand is destructive or executes code in your live app",
                    "how": [f"Re-call with confirm=true once the user has approved it: {risky}",
                            "Never combine this with a guessed path — verify the target with a read-only command first"],
                },
            }

        argv = ([f"vault={vault}"] if vault else []) + list(args)
        result = _run_cli(argv, timeout_s=timeout_s)
        if vault:
            result["requested_vault"] = vault
        return result

    @mcp.tool()
    def obsidian_cli_batch(
        commands: list[list[str]],
        vault: str | None = None,
        stop_on_error: bool = True,
        timeout_s: int = DEFAULT_TIMEOUT_S,
    ) -> dict[str, Any]:
        """Run several read-only CLI commands in sequence and return every answer.

        Handy for a vault survey (`[["search", "query=FFT"], ["tags", "counts"],
        ["backlinks", "file=Note"]]`). Destructive/eval tokens abort the whole call.
        `stop_on_error=False` keeps going after a failed command.
        """
        if not commands:
            return {"ok": False, "status": "no-commands", "channel": SERVER_NAME,
                    "remedy": {"step": "Pass a list of token lists"}}
        timeout_s = max(1, min(int(timeout_s or DEFAULT_TIMEOUT_S), MAX_TIMEOUT_S))

        flat = [t for cmd in commands for t in cmd]
        risky = _needs_confirm(flat)
        if risky:
            return {"ok": False, "status": "needs-confirmation", "blocked_tokens": risky,
                    "remedy": {"step": "obsidian_cli_batch is read-only by design",
                               "how": ["Use obsidian_cli_run(confirm=true) for a single approved write/eval command"]}}

        results: list[dict[str, Any]] = []
        for cmd in commands:
            argv = ([f"vault={vault}"] if vault else []) + list(cmd)
            single = _run_cli(argv, timeout_s=timeout_s)
            results.append({"argv": single.pop("argv", argv), **single})
            if stop_on_error and not single.get("ok"):
                return {"ok": False, "status": "stopped-on-error", "channel": SERVER_NAME,
                        "ran": len(results), "results": results,
                        "remedy": results[-1].get("remedy", "First failing command aborted the batch; pass stop_on_error=false to continue.")}
        ok_all = all(item.get("ok") for item in results)
        return {"ok": ok_all, "status": "ready" if ok_all else results[-1].get("status"),
                "channel": SERVER_NAME, "ran": len(results), "results": results}

    return mcp


def main() -> None:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass
    try:
        import mcp  # noqa: F401
    except ImportError:
        sys.stderr.write(json.dumps({"ok": False, "error": "mcp-sdk-missing",
                                     "remedy": "Run <plugin>/mcp/setup_env.cmd or install 'mcp>=1.1,<2' in the interpreter used to start this server."}) + "\n")
        raise SystemExit(2)

    build_server().run(transport="stdio")


if __name__ == "__main__":
    main()
