#!/usr/bin/env python3
"""obsidian-agent plugin — MCP entry for the **REST channel** (entry name `obsidian-rest`).

What this file is
-----------------
A launcher + guard layer around the upstream package `mcp-obsidian` 0.2.2
(`src/mcp_obsidian/`, MIT; author per `pyproject.toml`: Markus Pfundstein
<markus@life-electronic.nl>. The folder arrived without VCS metadata, so the
original repository URL is not verified on this machine — see ACKNOWLEDGEMENTS.md).

The upstream `src/` tree carries **exactly one local patch** as of 1.0.3
(`obsidian.py`: header-based PATCH needs an explicit `Markdown-Patch-Version: 1` on
Local REST API 5.1.0 — measured 400 without it, 200 with it; see `mcp/PLUGIN-NOTES.md`).
Everything else this plugin does happens in this file:

1. **Path landing** — puts `mcp/src` on `sys.path` and `chdir`s to `mcp/`, so the
   plugin works no matter what working directory the host starts it from, and so
   `python-dotenv` picks up `mcp/.env` (API key).
2. **SDK gate** — upstream is written against the mcp SDK 1.x low-level `Server`
   API. Under the workspace's system Python (mcp 2.0.0) the upstream import dies
   with `AttributeError: 'Server' object has no attribute 'list_tools'` (verified
   on this machine 2026-09-21). Instead of that stack trace we exit(2) with a
   remedy pointing at the bundled interpreter `mcp/.venv/Scripts/python.exe`.
   This entry is therefore **venv-only** by design (plugin decision 2026-09-21:
   pin `mcp>=1.1,<2` in the shipped environment rather than patch upstream).
3. **Unconfigured-but-startable** — upstream raises `ValueError` at import when
   `OBSIDIAN_API_KEY` is unset, which would make the host show a dead server with
   no explanation. Here a missing key is replaced by a sentinel before import, the
   server starts and advertises its full tool list, and every call short-circuits
   to `ok:false` + `remedy` (no REST request is attempted, no success is faked).
4. **Error shaping** — handler exceptions become structured `ok:false` answers
   with the concrete remedy for the observed failure class (connection refused,
   401/403, 404, timeout), because "check the plugin settings and paste the key"
   is more useful than a traceback over stdio.

Environment
-----------
`OBSIDIAN_API_KEY` (required for live calls), `OBSIDIAN_HOST` (default
`127.0.0.1`), `OBSIDIAN_PORT` (default `27124` — HTTPS port of the community
plugin *Local REST API*; its HTTP port is `27123`), `OBSIDIAN_PROTOCOL`
(`https`|`http`). Read once at import time by upstream — restart the server
after changing them.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any, Sequence

HERE = Path(__file__).resolve().parent
SERVER_NAME = "obsidian-rest"
PLUGIN_VERSION = "1.1.0"

# Sentinel standing in for a missing API key so that importing upstream cannot
# abort the process. Guarded at dispatch time; never sent as a real credential
# because `_configured()` short-circuits before any handler runs.
API_KEY_SENTINEL = "__OBSIDIAN_API_KEY_NOT_CONFIGURED__"

REMEDY_NOT_CONFIGURED = {
    "step": "Provide an Obsidian Local REST API key",
    "requires": [
        "Obsidian is running (1.12.7 on this machine) with a vault open",
        "Community plugin 'Local REST API' (coddingtonbear/obsidian-local-rest-api) installed and enabled",
    ],
    "how": [
        "Obsidian -> Settings -> Community plugins -> Browse -> search 'Local REST API' -> Install -> Enable",
        "Obsidian -> Settings -> Local REST API -> copy 'API key'",
        "Put it in <plugin>/mcp/.env as OBSIDIAN_API_KEY=... (or in the mcpServers 'env' block of the host manifest)",
        "Keep OBSIDIAN_PORT matching the plugin: 27124 for https (default), 27123 for http (then set OBSIDIAN_PROTOCOL=http)",
        "Restart the MCP server (reload the plugin or restart the host) — env is read once at startup",
    ],
    "verify": "python mcp/check_env.py  ->  the 'obsidian-rest config' and 'obsidian-rest port' rows must be OK",
}


def _fail(message: str, remedy: Any, **extra: Any) -> str:
    payload = {"ok": False, "channel": "obsidian-rest", "error": message, "remedy": remedy}
    payload.update(extra)
    return json.dumps(payload, ensure_ascii=False, indent=2)


def _sdk_guard() -> str:
    """Return the installed mcp SDK version, or exit with a remedy if unusable."""
    try:
        from importlib.metadata import version as _pkg_version

        sdk_version = _pkg_version("mcp")
    except Exception as exc:  # pragma: no cover - mcp absent
        sys.stderr.write(_fail(f"mcp-sdk-missing: {exc}", {
            "step": "Install the MCP Python SDK inside the bundled environment",
            "how": ["Run <plugin>/mcp/setup_env.cmd, then re-check with check_env.py"],
        }) + "\n")
        raise SystemExit(2)

    has_lowlevel_api = False
    try:
        from mcp.server import Server

        has_lowlevel_api = hasattr(Server, "list_tools") and hasattr(Server, "call_tool")
    except Exception:
        has_lowlevel_api = False

    if not has_lowlevel_api:
        sys.stderr.write(_fail(
            f"mcp-sdk-incompatible: interpreter has mcp {sdk_version}, "
            "upstream mcp-obsidian needs the 1.x low-level Server API "
            "(@app.list_tools()/@app.call_tool(), removed in 2.x)",
            {
                "step": "Run this entry with the bundled venv instead of the system interpreter",
                "command": str(HERE / ".venv" / "Scripts" / "python.exe") + " " + str(Path(__file__).resolve()),
                "if_venv_missing": "Run <plugin>/mcp/setup_env.cmd to rebuild it (pins mcp>=1.1,<2)",
                "why_not_patching_system_python": "The workspace's system Python carries mcp 2.0.0; downgrading it globally would break other MCP servers.",
            },
            sdk_version=sdk_version,
        ) + "\n")
        raise SystemExit(2)
    return sdk_version


def _resolve_credentials() -> tuple[bool, dict[str, Any]]:
    """Load `mcp/.env`, apply plugin defaults, and report whether live calls are possible."""
    from dotenv import load_dotenv

    os.chdir(HERE)
    env_file = HERE / ".env"
    load_dotenv(env_file)

    key = (os.environ.get("OBSIDIAN_API_KEY") or "").strip()
    configured = bool(key) and key != API_KEY_SENTINEL
    # Regression hook: OBSIDIAN_AGENT_FORCE_UNCONFIGURED=1 makes the entry behave as if no
    # key existed even when mcp/.env supplies one, so tests/selftest.py can exercise the
    # unconfigured path without moving a real credential file out of the way.
    if os.environ.get("OBSIDIAN_AGENT_FORCE_UNCONFIGURED", "").strip().lower() in {"1", "true", "yes"}:
        configured = False
    if not configured:
        os.environ["OBSIDIAN_API_KEY"] = API_KEY_SENTINEL
        key = API_KEY_SENTINEL

    # Upstream defaults host/port at import time; the plugin sets them explicitly so
    # that a missing .env still yields a coherent target (Local REST API defaults).
    os.environ.setdefault("OBSIDIAN_HOST", "127.0.0.1")
    os.environ.setdefault("OBSIDIAN_PORT", "27124")
    os.environ.setdefault("OBSIDIAN_PROTOCOL", "https")

    masked = "***" if configured and len(key) > 4 else "(unset)"
    info = {
        "api_key": masked,
        "env_file": str(env_file) if env_file.exists() else None,
        "target": f"{os.environ['OBSIDIAN_PROTOCOL']}://{os.environ['OBSIDIAN_HOST']}:{os.environ['OBSIDIAN_PORT']}",
    }
    return configured, info


def _classify_exception(text: str, tool: str = "") -> dict[str, Any]:
    """Map an upstream exception onto a failure class + actionable remedy.

    Two of these classes exist because the *backend* (Obsidian community plugin
    "Local REST API") changed contract while upstream 0.2.2 did not: see
    mcp/PLUGIN-NOTES.md gaps G1/G2. They are reported honestly as failures — never
    downgraded to a success-shaped empty answer.
    """
    lowered = text.lower()
    if tool in {"obsidian_get_periodic_note", "obsidian_get_recent_periodic_notes"} and "40400" in lowered:
        return {"failure_class": "backend-endpoint-missing", "remedy": {
            "step": "This Local REST API build does not expose the /periodic/ endpoints upstream calls",
            "evidence": "GET /periodic/daily/ -> 404 errorCode 40400 on the running plugin (5.1.0, measured 2026-09-21)",
            "how": [
                "Use the CLI channel instead: obsidian_cli_run(args=[\"daily:path\"]) / [\"daily:read\"] / [\"daily:append\", ...]",
                "Or read the note directly: obsidian_get_file_contents(filepath=...) once you know its path (daily:path gives it)",
                "Or file-system access (third channel) — daily notes are ordinary files in the vault",
            ],
            "not_a_bug": "Nothing to fix in this plugin; upstream targets an endpoint this backend does not serve.",
        }}
    if "unknown or invalid content-type" in lowered or "40012" in lowered:
        return {"failure_class": "backend-extension-missing", "remedy": {
            "step": "The Dataview DQL search route this tool uses is not registered in this backend build",
            "evidence": "POST /search/ with Content-Type application/vnd.olrapi.dataview.dql+txt -> 400 errorCode 40012, while application/vnd.olrapi.jsonlogic+json -> 200; GET / self-reports \"apiExtensions\": []",
            "how": [
                "For recent activity: obsidian_get_recent_changes has no working backend here - use the CLI channel (obsidian_cli_run args=[\"files\"]) or list + read",
                "For structured queries: use obsidian_complex_search (JsonLogic works on this build)",
            ],
            "not_a_bug": "Known gap G1 in mcp/PLUGIN-NOTES.md; do not treat an empty result as \"no changes\".",
        }}
    if "maximum read time" in lowered or "timed out" in lowered:
        return {"failure_class": "timeout", "remedy": "The REST call exceeded 3s connect / 6s read. Check that Obsidian is responsive, or retry."}
    if "failed to establish a new connection" in lowered or "refused" in lowered or "10061" in lowered:
        return {"failure_class": "connection-refused", "remedy": {
            "step": "Nothing is listening on the configured port",
            "how": [
                "Open Obsidian and enable the 'Local REST API' community plugin (it owns the port)",
                "In that plugin's settings check 'Use REST over http' -> port 27123, 'https' -> port 27124",
                "Match the server: OBSIDIAN_PORT + OBSIDIAN_PROTOCOL in mcp/.env, then restart the server",
                "If Obsidian runs on another machine, set OBSIDIAN_HOST accordingly",
            ]}}
    if "401" in lowered or "unauthorized" in lowered or "403" in lowered:
        return {"failure_class": "bad-api-key", "remedy": "The key in mcp/.env does not match the one shown by the Local REST API plugin settings. Copy it again and restart the server."}
    if "40084" in lowered or "markdown-patch-version" in lowered:
        return {"failure_class": "backend-contract-change",
                "remedy": "Local REST API 5.x 要求 header-based PATCH 显式声明补丁格式。本插件已在补丁 #1 里发 `Markdown-Patch-Version: 1`（见 mcp/PLUGIN-NOTES.md）；若仍见此错误，说明该 vault 的插件版本行为又变了，按错误原文调整请求头。"}
    if "40012" in lowered or "unknown or invalid content-type" in lowered:
        return {"failure_class": "unsupported-content-type",
                "remedy": "该 vault 的 Local REST API 不接受这个 Content-Type——最典型是 obsidian_get_recent_changes 走的 Dataview DQL 检索在 5.x 上未注册（GET / 的 apiExtensions 为空）。已知缺口 G1，替代路径见 mcp/PLUGIN-NOTES.md；用 GET / 的 apiExtensions 字段可确认该构建支持哪些扩展。"}
    if "404" in lowered or "not found" in lowered:
        return {"failure_class": "path-not-found", "remedy": "The vault path in this call does not exist. Use obsidian_list_files_in_vault / obsidian_list_files_in_dir to confirm the exact path (URL-encoded for spaces is handled by the server)."}
    return {"failure_class": "rest-error", "remedy": "See the raw message; run mcp/check_env.py for the channel status."}


def build_server(configured: bool, conn_info: dict[str, Any]):
    from mcp.server import Server
    from mcp.types import TextContent, Tool

    from mcp_obsidian import server as upstream

    handler_names = list(upstream.tool_handlers)
    app = Server(SERVER_NAME)

    def _text(payload: dict[str, Any]) -> Sequence[TextContent]:
        return [TextContent(type="text", text=json.dumps(payload, ensure_ascii=False, indent=2))]

    @app.list_tools()
    async def list_tools() -> list[Tool]:
        # Reuse upstream's own descriptions so the advertised surface stays identical
        # (including upstream's own `confirm: true` requirement on obsidian_delete_file).
        return [th.get_tool_description() for th in upstream.tool_handlers.values()]

    @app.call_tool()
    async def call_tool(name: str, arguments: Any) -> Sequence[TextContent]:
        if not isinstance(arguments, dict):
            return _text(json.loads(_fail("arguments-not-dict", {
                "step": "Pass an object as `arguments`", "expected": "a JSON object of tool parameters",
            })))

        handler = upstream.get_tool_handler(name)
        if handler is None:
            return _text(json.loads(_fail(f"unknown-tool: {name}", {
                "step": "Call one of the registered tools",
                "known_tools": handler_names,
            }, known_tools=handler_names)))

        if not configured:
            return _text(json.loads(_fail(
                "not-configured: OBSIDIAN_API_KEY is missing",
                REMEDY_NOT_CONFIGURED,
                tool=name,
                attempted_target=conn_info["target"],
                note="The server started on purpose so the tool list stays discoverable; no request was sent to Obsidian.",
            )))

        try:
            result = handler.run_tool(arguments)
        except Exception as exc:  # noqa: BLE001 - shaped below
            shaped = _classify_exception(str(exc), name)
            shaped = _classify_exception(str(exc), name)
            return _text(json.loads(_fail(f"{type(exc).__name__}: {exc}", shaped["remedy"],
                                          tool=name, **{k: v for k, v in shaped.items() if k != "remedy"})))

        # Real REST payload: pass upstream's content through, but mark the channel so a
        # reader can tell which of the two entries answered.
        out: list[TextContent] = []
        for item in result:
            if isinstance(item, TextContent):
                out.append(item)
            else:  # ImageContent / EmbeddedResource from upstream keep their type
                out.append(item)  # type: ignore[arg-type]
        return out

    return app


async def _serve(app: Any) -> None:
    from mcp.server.stdio import stdio_server

    async with stdio_server() as (read_stream, write_stream):
        await app.run(read_stream, write_stream, app.create_initialization_options())


def main() -> None:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass

    sdk_version = _sdk_guard()
    sys.path.insert(0, str(HERE / "src"))

    configured, conn_info = _resolve_credentials()
    # logging goes to stderr: stdout is the JSON-RPC channel.
    import logging

    logging.basicConfig(level=logging.INFO, stream=sys.stderr)

    from mcp_obsidian import server as _upstream_server  # noqa: F401  (import-time UTF-8 stdio + handler registry)

    app = build_server(configured, conn_info)
    logging.getLogger(SERVER_NAME).info(
        "obsidian-rest ready: mcp=%s configured=%s target=%s tools=%d",
        sdk_version, configured, conn_info["target"], len(_upstream_server.tool_handlers),
    )
    import asyncio

    asyncio.run(_serve(app))


if __name__ == "__main__":
    main()
