// OpenLeash remote-control bridge. Served by the desktop app ahead of the UI
// bundle when the UI is opened from another device's browser.
//
// It stands in for the `window.__TAURI_INTERNALS__` object Tauri injects into
// its own webview, so the unmodified frontend runs in a plain browser:
// `invoke` becomes a WebSocket message the desktop app executes as its main
// window would, and `listen` becomes a subscription on the desktop side.
//
// A handful of calls only make sense on the desktop itself (native dialogs,
// window buttons, opening a URL) and are answered here instead: see `LOCAL`.
(function () {
  "use strict";
  if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.__openleashRemote) return;

  // ── callbacks (Tauri's transformCallback contract) ──
  var callbacks = new Map();
  var nextCallback = 1;
  function transformCallback(cb, once) {
    var id = nextCallback++;
    callbacks.set(id, function (data) {
      if (once) callbacks.delete(id);
      if (cb) cb(data);
    });
    return id;
  }
  function unregisterCallback(id) { callbacks.delete(id); }
  function runCallback(id, data) {
    var f = callbacks.get(id);
    if (f) f(data);
  }

  // ── connection ──
  var ws = null;
  var open = false;
  var everOpened = false;
  var revoked = false;
  var nextId = 1;
  var pending = new Map(); // id -> {resolve, reject}
  var outbox = [];         // messages waiting for the socket
  var retry = 0;

  function url() {
    return (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/__openleash/ws";
  }

  function connect() {
    if (revoked) return;
    try { ws = new WebSocket(url()); } catch (e) { schedule(); return; }
    ws.onopen = function () {
      if (everOpened) {
        // Events fired while we were away were missed, and the desktop side
        // dropped this page's subscriptions with the old socket. Rebuilding
        // all of that piecemeal is where stale-state bugs live; a reload gets
        // a coherent snapshot from the store's normal startup path.
        location.reload();
        return;
      }
      open = true;
      everOpened = true;
      retry = 0;
      overlay(null);
      var q = outbox; outbox = [];
      q.forEach(function (m) { ws.send(m); });
    };
    ws.onmessage = function (e) {
      var m;
      try { m = JSON.parse(e.data); } catch (_) { return; }
      if (m.t === "result") {
        var p = pending.get(m.id);
        if (!p) return;
        pending.delete(m.id);
        if (m.ok) p.resolve(m.data); else p.reject(m.error);
      } else if (m.t === "event") {
        runCallback(m.handler, m.event);
      } else if (m.t === "bye") {
        revoked = true;
        overlay("This device was signed out of OpenLeash. Pair it again from Settings → Remote on the computer.", true);
      }
    };
    ws.onclose = function () {
      open = false;
      pending.forEach(function (p) { p.reject("Lost the connection to OpenLeash."); });
      pending.clear();
      if (revoked) return;
      overlay(everOpened ? "Releashing the OpenLeash…" : "Connecting to OpenLeash…");
      schedule();
    };
  }

  function schedule() {
    retry = Math.min(retry + 1, 6);
    setTimeout(function () {
      if (!everOpened) { connect(); return; }
      // Before reloading, make sure we are still paired: a 401 here means the
      // session was revoked while we were disconnected.
      fetch("/__openleash/remote.js", { method: "GET", cache: "no-store", credentials: "same-origin" })
        .then(function (r) {
          if (r.status === 401) { location.href = "/"; return; }
          connect();
        })
        .catch(function () { schedule(); });
    }, Math.min(500 * Math.pow(2, retry - 1), 15000));
  }

  function send(cmd, args) {
    return new Promise(function (resolve, reject) {
      if (revoked) { reject("This device is no longer paired."); return; }
      var id = nextId++;
      pending.set(id, { resolve: resolve, reject: reject });
      var msg = JSON.stringify({ id: id, cmd: cmd, args: args === undefined ? {} : args });
      if (open) ws.send(msg); else outbox.push(msg);
    });
  }

  // ── calls answered in the browser ──
  function picker() {
    var r = window.__OPENLEASH_REMOTE__;
    return r && typeof r.pick === "function" ? r.pick : null;
  }

  var LOCAL = {
    // Native file dialogs would open on the desktop, where nobody is looking.
    // The app registers an in-page browser of the host's filesystem instead.
    "plugin:dialog|open": function (args) {
      var o = (args && args.options) || {};
      var p = picker();
      if (p) return p({ mode: "open", directory: !!o.directory, multiple: !!o.multiple, title: o.title || "", defaultPath: o.defaultPath || "" });
      var v = window.prompt((o.title || "Path on the computer") + ":", o.defaultPath || "");
      return Promise.resolve(v ? (o.multiple ? [v] : v) : null);
    },
    "plugin:dialog|save": function (args) {
      var o = (args && args.options) || {};
      var p = picker();
      if (p) return p({ mode: "save", directory: false, multiple: false, title: o.title || "", defaultPath: o.defaultPath || "" });
      var v = window.prompt((o.title || "Save to path on the computer") + ":", o.defaultPath || "");
      return Promise.resolve(v || null);
    },
    "plugin:dialog|message": function (args) { window.alert((args && args.message) || ""); return Promise.resolve(true); },
    "plugin:dialog|ask": function (args) { return Promise.resolve(window.confirm((args && args.message) || "")); },
    "plugin:dialog|confirm": function (args) { return Promise.resolve(window.confirm((args && args.message) || "")); },
    // Links open on the device the user is holding.
    "plugin:opener|open_url": function (args) {
      var u = String((args && args.url) || "");
      if (/^(https?:|mailto:)/i.test(u)) window.open(u, "_blank", "noopener,noreferrer");
      return Promise.resolve(null);
    },
    "plugin:opener|open_path": function () { return Promise.reject("Opening files on the computer isn't available remotely."); },
    "plugin:opener|reveal_item_in_dir": function () { return Promise.reject("Showing files on the computer isn't available remotely."); },
    "plugin:notification|is_permission_granted": function () {
      return Promise.resolve(typeof Notification !== "undefined" && Notification.permission === "granted");
    },
    "plugin:notification|request_permission": function () {
      if (typeof Notification === "undefined") return Promise.resolve("denied");
      return Promise.resolve(Notification.requestPermission());
    },
    "plugin:notification|notify": function (args) {
      var o = (args && args.options) || {};
      try { if (typeof Notification !== "undefined" && Notification.permission === "granted") new Notification(o.title || "OpenLeash", { body: o.body || "" }); } catch (_) {}
      return Promise.resolve(null);
    },
  };

  function windowCall(cmd) {
    // Window chrome belongs to the desktop window: a remote "close" must not
    // quit the app out from under the person sitting at it.
    var name = cmd.split("|")[1] || "";
    if (/^is_/.test(name)) return Promise.resolve(name === "is_visible" || name === "is_focused" ? document.hasFocus() : false);
    if (name === "scale_factor") return Promise.resolve(window.devicePixelRatio || 1);
    if (name === "inner_size" || name === "outer_size") return Promise.resolve({ width: window.innerWidth, height: window.innerHeight });
    if (name === "inner_position" || name === "outer_position") return Promise.resolve({ x: 0, y: 0 });
    if (name === "theme") return Promise.resolve(window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
    if (name === "title") return Promise.resolve(document.title);
    return Promise.resolve(null);
  }

  // Window-system events never arrive from the server (they describe the
  // desktop window); hand out local ids so listen/unlisten stay balanced.
  var localListener = 1 << 30;

  function invoke(cmd, args) {
    if (Object.prototype.hasOwnProperty.call(LOCAL, cmd)) return LOCAL[cmd](args);
    if (cmd.indexOf("plugin:window|") === 0 || cmd.indexOf("plugin:webview|") === 0) return windowCall(cmd);
    if (cmd === "plugin:event|listen" && args && typeof args.event === "string" && args.event.indexOf("tauri://") === 0) {
      return Promise.resolve(localListener++);
    }
    if (cmd === "plugin:event|unlisten" && args && args.eventId >= 1 << 30) return Promise.resolve(null);
    return send(cmd, args);
  }

  function convertFileSrc(path, protocol) {
    if (protocol === "openleash-viz") return "/__openleash/viz/" + encodeURIComponent(path);
    return path;
  }

  // ── status overlay ──
  var overlayEl = null;
  function overlay(text, final) {
    if (!text) {
      if (overlayEl) { overlayEl.remove(); overlayEl = null; }
      return;
    }
    if (!document.body) {
      document.addEventListener("DOMContentLoaded", function () { overlay(text, final); }, { once: true });
      return;
    }
    if (!overlayEl) {
      overlayEl = document.createElement("div");
      overlayEl.setAttribute("role", "status");
      overlayEl.setAttribute("aria-live", "polite");
      overlayEl.style.cssText = "position:fixed;inset:0;z-index:2147483647;display:flex;align-items:center;justify-content:center;padding:24px;background:rgba(15,15,17,.82);color:#eee;font:14px/1.5 system-ui,sans-serif;text-align:center;backdrop-filter:blur(2px)";
      document.body.appendChild(overlayEl);
    }
    overlayEl.textContent = text;
    if (final) {
      var a = document.createElement("a");
      a.href = "/";
      a.textContent = "Pair again";
      a.style.cssText = "display:block;margin-top:12px;color:#ff6363";
      var wrap = document.createElement("div");
      wrap.textContent = text;
      wrap.appendChild(a);
      overlayEl.textContent = "";
      overlayEl.appendChild(wrap);
    }
  }

  window.__TAURI_INTERNALS__ = {
    __openleashRemote: true,
    invoke: invoke,
    transformCallback: transformCallback,
    unregisterCallback: unregisterCallback,
    runCallback: runCallback,
    callbacks: callbacks,
    convertFileSrc: convertFileSrc,
    metadata: {
      currentWindow: { label: "main" },
      currentWebview: { windowLabel: "main", label: "main" },
    },
    plugins: { path: { sep: "/", delimiter: ":" } },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: function () {} };
  window.isTauri = true;
  var api = window.__OPENLEASH_REMOTE__ || {};
  api.remote = true;
  api.host = location.host;
  api.logout = function () {
    return fetch("/__openleash/logout", { method: "POST", credentials: "same-origin" }).then(function () {
      revoked = true;
      location.href = "/";
    });
  };
  window.__OPENLEASH_REMOTE__ = api;
  connect();
})();
