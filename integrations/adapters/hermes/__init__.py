"""Optional Hermes registration shim. All Soli behavior runs in the Rust binary."""
import json
import os
import shlex
import subprocess


def _env(name):
    # Multiplexed Hermes profiles keep their values outside process-wide environ.
    from hermes_cli.config import get_env_value
    return get_env_value(name)


def _run(args, payload=None):
    binary = _env("SOLI_BINARY")
    if not binary:
        return "Soli belum dikonfigurasi di profil agen ini."
    env = {k:v for k,v in os.environ.items() if k in ("PATH", "SYSTEMROOT", "LANG", "LC_ALL", "TMP", "TEMP")}
    for name in ("SOLI_CREDENTIAL_FILE", "SOLI_BRIDGE_SECRETS_JSON", "SOLI_OWNERS_JSON"):
        value = _env(name)
        if value is not None:
            env[name] = value
    result = subprocess.run([binary, *args], env=env,
        input=None if payload is None else json.dumps(payload),
        text=True, capture_output=True, timeout=200)
    return result.stdout.strip() if result.returncode == 0 else "Soli rejected the request; check its configuration."


def _display(text):
    try:
        return _run(["format"], json.loads(text))
    except (ValueError, TypeError):
        return text


def _private(source, platform):
    chat = str(getattr(source, "chat_id", ""))
    user = str(getattr(source, "user_id", ""))
    if platform == "slack":
        return chat.startswith("D")
    if platform == "telegram":
        return chat == user
    if platform in ("whatsapp", "whatsapp_cloud"):
        return chat == user and not chat.endswith("@g.us")
    return platform == "discord" and getattr(source, "chat_type", "") == "dm"


def _menu(ctx, raw_args="", *, source=None, platform="", **kwargs):
    fallback = _run(["menu"])
    if source is None or not callable(getattr(ctx, "ask_choice", None)):
        return fallback
    platform = getattr(platform or getattr(source, "platform", ""), "value", platform or getattr(source, "platform", ""))
    catalogue = json.loads(_run(["menu-json"]))
    groups = catalogue["groups"]
    picked = ctx.ask_choice("Soli — pilih menu", [{"label":g["label"], "value":str(i)} for i,g in enumerate(groups)])
    if picked not in [str(i) for i in range(len(groups))]:
        return fallback
    items = groups[int(picked)]["items"]
    selected = ctx.ask_choice(groups[int(picked)]["label"], [{"label":a["label"],"value":str(i)} for i,a in enumerate(items)])
    if selected not in [str(i) for i in range(len(items))]:
        return fallback
    action = items[int(selected)]
    if action.get("input") == "tracked-position":
        positions = json.loads(_run(["position-menu"]))["positions"]
        if not positions:
            return "Soli belum punya posisi yang dikelola."
        picked = ctx.ask_choice("Pilih posisi Soli", [{"label":p["label"],"value":str(i)} for i,p in enumerate(positions)])
        if picked not in [str(i) for i in range(len(positions))]:
            return "Dibatalkan."
        action = {**action, "command":positions[int(picked)]["command"]}
    if action["write"]:
        owners = json.loads(_env("SOLI_OWNERS_JSON") or "{}")
        if str(getattr(source,"user_id","")) not in owners.get(platform, []):
            return "Kontrol ini hanya untuk owner Soli."
        # A shared card does not prove who tapped it; require a fresh native owner command there.
        if not _private(source, platform):
            return "Kirim command owner ini di chat yang sama: /soli " + action["command"]
        if ctx.ask_choice("Jalankan " + action["label"] + "?", [{"label":"Jalankan","value":"yes"},{"label":"Batal","value":"no"}]) != "yes":
            return "Dibatalkan."
    return _command(action["command"], source=source, platform=platform)


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
    if not words or words[0] in ("status", "lifecycle", "report", "wallet", "positions", "pnl", "position", "learning", "candidates") or words == ["settings"]:
        if words and words[0] == "candidates":
            words[0] = "live-candidates"
        return _display(_run(words or ["status"]))
    return _display(_run(["connector"], {"event_id":"native", "platform":getattr(platform,"value",platform),
        "user_id":str(getattr(source,"user_id","") or ""), "conversation_id":str(getattr(source,"chat_id","") or ""),
        "thread_id":str(thread) if thread is not None else None,
        "command":words[0] if words else "status", "arguments":words[1:]}))


def _read(args, **kwargs):
    action = args.get("action", "status")
    return _run([action]) if action in ("status", "report") else "Unsupported read action."


def register(ctx):
    def menu(args="", **kwargs):
        return _menu(ctx, args, **kwargs)
    def command(args="", **kwargs):
        return menu(args, **kwargs) if args.strip() in ("", "menu") else _command(args, **kwargs)
    ctx.register_command("soli-menu", handler=menu, description="Open Soli menu")
    ctx.register_command("soli_menu", handler=menu, description="Open Soli menu (Telegram)")
    ctx.register_command("soli", handler=command, description="Soli native Rust connector", args_hint="<command and arguments>")
    ctx.register_tool(name="soli_read", toolset="soli", handler=_read,
        schema={"name":"soli_read","description":"Read Soli status/report; cannot authorize trades.",
            "parameters":{"type":"object","properties":{"action":{"type":"string","enum":["status","report"]}},"additionalProperties":False}})
