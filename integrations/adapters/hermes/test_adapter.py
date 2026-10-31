"""Stdlib checks for the optional host glue; trading behavior is tested in Rust."""
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("soli_adapter", Path(__file__).with_name("__init__.py"))
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)


class MenuTest(unittest.TestCase):
    def setUp(self):
        resolver = patch.object(adapter, "_env", side_effect=os.environ.get)
        resolver.start()
        self.addCleanup(resolver.stop)

    def test_profile_values_override_process_environment(self):
        profile = {"SOLI_BINARY":"profile-binary", "SOLI_CREDENTIAL_FILE":"profile-credential", "SOLI_OWNERS_JSON":'{"slack":["owner"]}'}
        with patch.object(adapter, "_env", side_effect=profile.get), patch.dict(os.environ, {"SOLI_CREDENTIAL_FILE":"wrong-profile", "SOLI_PRIVATE_KEY":"must-not-inherit"}), patch.object(adapter.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout="commands")) as run:
            self.assertEqual(adapter._run(["menu"]), "commands")
        self.assertEqual(run.call_args.args[0], ["profile-binary", "menu"])
        child = run.call_args.kwargs["env"]
        self.assertEqual(child["SOLI_CREDENTIAL_FILE"], "profile-credential")
        self.assertNotIn("SOLI_PRIVATE_KEY", child)

    def check_menu(self, platform, chat, user, private, owner=True, confirm="yes"):
        source = SimpleNamespace(platform=platform, chat_id=chat, user_id=user, chat_type="dm")
        choices = iter(["0", "0", confirm])
        ctx = SimpleNamespace(ask_choice=lambda *args: next(choices))
        calls = []
        def run(args, payload=None):
            calls.append((args, payload))
            if args == ["menu"]:
                return "commands"
            if args == ["menu-json"]:
                return json.dumps({"groups":[{"label":"Controls","items":[{"label":"Pause","command":"pause all","write":True}]}]})
            if args == ["format"]:
                return "done"
            return json.dumps({"virtual":True})
        with patch.object(adapter, "_run", side_effect=run), patch.dict(os.environ, {"SOLI_OWNERS_JSON":json.dumps({platform:[user] if owner else []})}):
            result = adapter._menu(ctx, source=source)
        submits = [p for args,p in calls if args == ["connector"]]
        self.assertEqual(bool(submits), private and owner and confirm == "yes")
        if submits:
            self.assertEqual(submits[0]["user_id"], user)
            self.assertEqual(submits[0]["conversation_id"], chat)
            self.assertEqual(submits[0]["platform"], platform)
        return result

    def test_verified_private_context_all_connected_host_types(self):
        for platform,chat,user in [("slack","D1","U1"),("telegram","42","42"),("discord","d1","u1"),("whatsapp","user@s.whatsapp.net","user@s.whatsapp.net"),("whatsapp_cloud","123","123")]:
            self.check_menu(platform,chat,user,True)

    def test_shared_card_is_not_owner_authorization(self):
        result=self.check_menu("slack","C1","U1",False)
        self.assertIn("/soli pause all",result)

    def test_nonowner_and_cancel_do_not_submit(self):
        self.check_menu("slack","D1","U1",True,owner=False)
        self.check_menu("slack","D1","U1",True,confirm="no")

    def test_force_entry_requires_private_chat_and_marks_it(self):
        shared = SimpleNamespace(platform="slack", chat_id="C1", user_id="U1", chat_type="channel")
        with patch.object(adapter, "_run", return_value="{}") as run:
            result = adapter._command("force-entry So11111111111111111111111111111111111111112", source=shared, platform="slack")
        self.assertIn("DM pribadi", result)
        run.assert_not_called()

        private = SimpleNamespace(platform="slack", chat_id="D1", user_id="U1", chat_type="dm")
        submitted = []
        def run_private(args, payload=None):
            if args == ["connector"]:
                submitted.append(payload)
                return "{}"
            return "done"
        with patch.object(adapter, "_run", side_effect=run_private):
            adapter._command("force-entry So11111111111111111111111111111111111111112", source=private, platform="slack")
        self.assertTrue(submitted[0]["is_private"])

    def test_headless_uses_plain_menu(self):
        with patch.object(adapter,"_run",return_value="commands"):
            self.assertEqual(adapter._menu(SimpleNamespace()),"commands")

    def test_single_close_uses_the_tracked_position(self):
        mint="So11111111111111111111111111111111111111112"
        choices=iter(["0","0","0","yes"])
        ctx=SimpleNamespace(ask_choice=lambda *args:next(choices))
        source=SimpleNamespace(platform="slack",chat_id="D1",user_id="U1")
        submitted=[]
        def run(args,payload=None):
            if args==["menu"]:return "commands"
            if args==["menu-json"]:return json.dumps({"groups":[{"label":"Close","items":[{"label":"One","command":"force-close {mint} CONFIRM","write":True,"input":"tracked-position"}]}]})
            if args==["position-menu"]:return json.dumps({"positions":[{"label":mint,"command":f"force-close {mint} CONFIRM"}]})
            if args==["connector"]:submitted.append(payload);return "{}"
            return "done"
        with patch.object(adapter,"_run",side_effect=run),patch.dict(os.environ,{"SOLI_OWNERS_JSON":'{"slack":["U1"]}'}):
            adapter._menu(ctx,source=source)
        self.assertEqual(submitted[0]["command"],"force-close")
        self.assertEqual(submitted[0]["arguments"],[mint,"CONFIRM"])


if __name__ == "__main__":
    unittest.main()
