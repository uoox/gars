// gars browser bridge — service worker.
// Connects to ws://<base>/v1/extension with the user's admin token, receives
// commands from gars and dispatches against chrome.tabs / chrome.scripting.

interface Settings {
  base: string;
  token: string;
}

const DEFAULT_BASE = "ws://127.0.0.1:9221";

async function loadSettings(): Promise<Settings> {
  const r = await chrome.storage.local.get(["base", "token"]);
  return {
    base: (r.base as string) || DEFAULT_BASE,
    token: (r.token as string) || "",
  };
}

let socket: WebSocket | null = null;
let backoff = 1000;
let reconnectTimer: any = null;

async function connect() {
  const { base, token } = await loadSettings();
  if (!token) {
    console.log("[gars] no token configured; not connecting");
    return;
  }
  const url = `${base.replace(/^http/, "ws").replace(/\/$/, "")}/v1/extension`;
  try {
    socket = new WebSocket(url, [`Bearer.${token}`]);
  } catch (e) {
    console.log("[gars] connect failed", e);
    schedule();
    return;
  }
  socket.addEventListener("open", async () => {
    backoff = 1000;
    const hello = {
      browser: navigator.userAgent.includes("Edg/") ? "edge" : "chrome",
      version: chrome.runtime.getManifest().version,
    };
    socket?.send(JSON.stringify(hello));
    chrome.action.setBadgeText({ text: "ON" });
    chrome.action.setBadgeBackgroundColor({ color: "#34d399" });
  });
  socket.addEventListener("message", async (e) => {
    let frame: any;
    try {
      frame = JSON.parse(e.data);
    } catch {
      return;
    }
    if (!frame.id || !frame.op) return;
    try {
      const data = await handleOp(frame.op, frame.params ?? {});
      socket?.send(JSON.stringify({ id: frame.id, ok: true, data }));
    } catch (err: any) {
      socket?.send(
        JSON.stringify({ id: frame.id, ok: false, error: String(err?.message ?? err) }),
      );
    }
  });
  socket.addEventListener("close", () => {
    chrome.action.setBadgeText({ text: "" });
    schedule();
  });
  socket.addEventListener("error", () => socket?.close());
}

function schedule() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, Math.min(backoff, 30_000));
  backoff = Math.min(backoff * 2, 30_000);
}

async function handleOp(op: string, params: any): Promise<any> {
  switch (op) {
    case "list_tabs": {
      const tabs = await chrome.tabs.query({});
      return tabs.map((t) => ({
        id: t.id,
        url: t.url,
        title: t.title,
        active: t.active,
        windowId: t.windowId,
      }));
    }
    case "scan_page": {
      const tab = await resolveTab(params.tab_id);
      const [{ result }] = await chrome.scripting.executeScript({
        target: { tabId: tab.id! },
        func: scanPageFn,
        args: [
          {
            text_only: !!params.text_only,
            max_len: Number(params.max_len ?? 35000),
          },
        ],
      });
      return result;
    }
    case "execute_js": {
      const tab = await resolveTab(params.tab_id);
      const [{ result }] = await chrome.scripting.executeScript({
        target: { tabId: tab.id! },
        func: (code: string) => {
          // eslint-disable-next-line no-new-func
          return new Function(code)();
        },
        args: [String(params.script ?? "")],
      });
      return { js_return: result };
    }
    case "click": {
      const tab = await resolveTab(params.tab_id);
      const [{ result }] = await chrome.scripting.executeScript({
        target: { tabId: tab.id! },
        func: (selector: string) => {
          const el = document.querySelector(selector) as HTMLElement | null;
          if (!el) return { ok: false, reason: "selector not found" };
          el.click();
          return { ok: true };
        },
        args: [String(params.selector ?? "")],
      });
      return result;
    }
    case "type": {
      const tab = await resolveTab(params.tab_id);
      const [{ result }] = await chrome.scripting.executeScript({
        target: { tabId: tab.id! },
        func: (selector: string, text: string) => {
          const el = document.querySelector(selector) as HTMLInputElement | null;
          if (!el) return { ok: false, reason: "selector not found" };
          el.focus();
          el.value = text;
          el.dispatchEvent(new Event("input", { bubbles: true }));
          el.dispatchEvent(new Event("change", { bubbles: true }));
          return { ok: true };
        },
        args: [String(params.selector ?? ""), String(params.text ?? "")],
      });
      return result;
    }
    case "navigate": {
      const tab = await resolveTab(params.tab_id);
      await chrome.tabs.update(tab.id!, { url: String(params.url) });
      return { ok: true };
    }
    case "screenshot": {
      const tab = await resolveTab(params.tab_id);
      const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId!, {
        format: "png",
      });
      return { data_url: dataUrl };
    }
    default:
      throw new Error(`unknown op: ${op}`);
  }
}

async function resolveTab(tabId: any) {
  if (tabId && tabId !== "active") {
    const tab = await chrome.tabs.get(Number(tabId));
    return tab;
  }
  const [active] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!active) throw new Error("no active tab");
  return active;
}

function scanPageFn(opts: { text_only: boolean; max_len: number }) {
  const text = document.body?.innerText ?? "";
  const html = opts.text_only ? "" : document.body?.innerHTML ?? "";
  const title = document.title;
  const url = location.href;
  const truncated_text =
    text.length > opts.max_len ? text.slice(0, opts.max_len) + "…[truncated]" : text;
  return { url, title, text: truncated_text, html: html.slice(0, opts.max_len) };
}

chrome.runtime.onInstalled.addListener(connect);
chrome.runtime.onStartup.addListener(connect);

// Keep service worker alive via a recurring alarm so the WS doesn't get torn down.
chrome.alarms.create("gars-keepalive", { periodInMinutes: 0.4 });
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === "gars-keepalive") {
    if (!socket || socket.readyState !== WebSocket.OPEN) connect();
  }
});

connect();
