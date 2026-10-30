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

    def test_headless_uses_plain_menu(self):
        with patch.object(adapter,"_run",return_value="commands"):
            self.assertEqual(adapter._menu(SimpleNamespace()),"commands")


if __name__ == "__main__":
    unittest.main()
