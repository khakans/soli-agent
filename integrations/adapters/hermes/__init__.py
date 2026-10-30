"""Optional Hermes registration shim. All Soli behavior runs in the Rust binary."""
import json
import os
import shlex
import subprocess


def _run(args, payload=None):
    result = subprocess.run([os.environ["SOLI_BINARY"], *args],
        input=None if payload is None else json.dumps(payload),
        text=True, capture_output=True, timeout=200)
    return result.stdout.strip() if result.returncode == 0 else "Soli rejected the request; check its configuration."


def _command(raw_args, *, source=None, platform="", **kwargs):
    if source is None:
        return "Soli requires an authenticated native transport session."
    platform = platform or getattr(source, "platform", "")
    thread = getattr(source, "thread_id", None)
    try:
        words = shlex.split(raw_args)
    except ValueError:
        return "Invalid command quoting; use /soli menu."
    if words and words[0] == "menu":
        return _run(["menu"])
    if not words or words[0] in ("status", "lifecycle", "report", "wallet", "positions", "pnl", "position", "learning", "candidates"):
        if words and words[0] == "candidates":
            words[0] = "live-candidates"
        return _run(words or ["status"])
    return _run(["connector"], {"event_id":"native", "platform":getattr(platform,"value",platform),
        "user_id":str(getattr(source,"user_id","") or ""), "conversation_id":str(getattr(source,"chat_id","") or ""),
        "thread_id":str(thread) if thread is not None else None,
        "command":words[0] if words else "status", "arguments":words[1:]})


def _read(args, **kwargs):
    action = args.get("action", "status")
    return _run([action]) if action in ("status", "report") else "Unsupported read action."


def register(ctx):
    ctx.register_command("soli-menu", handler=lambda *args, **kwargs: _run(["menu"]), description="List Soli commands")
    ctx.register_command("soli_menu", handler=lambda *args, **kwargs: _run(["menu"]), description="List Soli commands (Telegram)")
    ctx.register_command("soli", handler=_command, description="Soli native Rust connector", args_hint="<command and arguments>")
    ctx.register_tool(name="soli_read", toolset="soli", handler=_read,
        schema={"name":"soli_read","description":"Read Soli status/report; cannot authorize trades.",
            "parameters":{"type":"object","properties":{"action":{"type":"string","enum":["status","report"]}},"additionalProperties":False}})
