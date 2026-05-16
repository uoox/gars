async function init() {
  const r = await chrome.storage.local.get(["base", "token"]);
  (document.getElementById("base") as HTMLInputElement).value =
    (r.base as string) || "ws://127.0.0.1:9221";
  (document.getElementById("token") as HTMLInputElement).value = (r.token as string) || "";
  document.getElementById("save")!.addEventListener("click", async () => {
    const base = (document.getElementById("base") as HTMLInputElement).value.trim();
    const token = (document.getElementById("token") as HTMLInputElement).value.trim();
    await chrome.storage.local.set({ base, token });
    document.getElementById("status")!.textContent = "saved; reconnecting…";
    await chrome.runtime.sendMessage({ kind: "reconnect" }).catch(() => {});
    // Force service worker to reload by toggling alarm
    chrome.alarms.create("gars-keepalive", { periodInMinutes: 0.4 });
  });
}
init();
