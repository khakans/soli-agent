"use strict";
let credential = "", pending = null, reviewed = null;
const el = id => document.getElementById(id);
async function call(path, body) {
  const response = await fetch(path, {
    method: body ? "POST" : "GET", cache: "no-store", redirect: "error",
    headers: {Authorization: "Bearer " + credential, "Content-Type": "application/json"},
    body: body ? JSON.stringify(body) : undefined
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || "Request rejected");
  return result;
}
function disable() { el("execute").disabled = true; el("confirm").checked = false; }
el("load").onclick = async () => {
  disable(); pending = null;
  credential = el("token").value || credential; el("token").value = "";
  try {
    const result = await call("/v1/live/approval");
    reviewed = result;
    el("mint").replaceChildren(...Object.keys(result.tracked_positions || {}).map(mint => new Option(mint, mint)));
    el("exit-confirm").checked = false; el("arm").disabled = true;
    el("disarm").disabled = !el("mint").value;
    el("details").textContent = JSON.stringify(result, null, 2);
    if (result.pending && !result.pending.expired && result.pending.state === "preview") pending = result.pending;
    el("result").textContent = pending ? "Review the order before confirming." : "No executable preview. Request a new preview if expired.";
  } catch (e) { el("result").textContent = e.message; }
};
el("confirm").onchange = () => { el("execute").disabled = !pending || !el("confirm").checked; };
el("execute").onclick = async () => {
  if (!pending || !el("confirm").checked) return;
  const order = pending; pending = null; disable();
  try {
    const result = await call("/v1/live/execute", {order_id: order.order_id, digest: order.digest, confirm: true});
    el("result").textContent = JSON.stringify(result);
  } catch (e) { el("result").textContent = e.message + ". Check status before retrying; never assume a timeout means failure."; }
};
el("forget").onclick = () => { credential = ""; pending = null; reviewed = null; el("token").value = ""; disable(); el("arm").disabled = true; el("disarm").disabled = true; el("details").textContent = "Cleared."; };
el("exit-confirm").onchange = () => { el("arm").disabled = !reviewed || !el("mint").value || !el("exit-confirm").checked; };
async function authorizeExits(enabled) {
  if (!reviewed || !el("mint").value || (enabled && !el("exit-confirm").checked)) return;
  const hours = Number(el("hours").value);
  if (enabled && (!Number.isInteger(hours) || hours < 1 || hours > 168)) return;
  try {
    const result = await call("/v1/live/exits", {mint: el("mint").value, enabled,
      expires_at: enabled ? Math.floor(Date.now() / 1000) + hours * 3600 : 0, strategy_hash: reviewed.strategy_hash});
    el("result").textContent = JSON.stringify(result); el("arm").disabled = true; el("exit-confirm").checked = false;
  } catch (e) { el("result").textContent = e.message; }
}
el("arm").onclick = () => authorizeExits(true);
el("disarm").onclick = () => authorizeExits(false);
