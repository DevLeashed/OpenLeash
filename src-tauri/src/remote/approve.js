// Confirm page for an approval sent to a phone as an ntfy notification.
// The token is in the URL fragment only (never sent in query strings), and is
// POSTed with the paired session cookie. All server-controlled text uses textContent.
(function () {
  "use strict";
  var $ = function (id) { return document.getElementById(id); };
  var head = $("head"), chat = $("chat"), req = $("req"), kind = $("kind"), title = $("title"), detail = $("detail");
  var actions = $("actions"), once = $("once"), always = $("always"), deny = $("deny"), msg = $("msg"), open = $("open");
  var params = new URLSearchParams(location.hash.slice(1));
  var task = params.get("task") || "", item = params.get("item") || "", token = params.get("token") || "";
  var DONE = { once: "Approved once", always: "Always allowed", deny: "Denied" };
  function show(text, ok) { msg.textContent = text; msg.className = ok ? "msg ok" : "msg"; }
  function finish(text, ok) { show(text, ok); actions.hidden = true; open.hidden = false; }
  function busy(on) { [once, always, deny].forEach(function (b) { b.disabled = on; }); }
  function call(path, body) {
    return fetch("/__openleash/push/" + path, { method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) })
      .then(function (r) { return r.json().catch(function () { return {}; }).then(function (j) { return { ok: r.ok, status: r.status, j: j || {} }; }); });
  }
  function failure(r) { finish(r.j.error || (r.status === 401 ? "Pair this browser first." : r.status === 410 ? "This notification expired." : r.status === 409 ? "Already answered." : "Couldn't answer this request."), false); }
  if (!task || !item || !token) { finish("This link is incomplete. Open OpenLeash to answer.", false); return; }
  function answer(decision) {
    busy(true);
    call("act", { task: task, item: item, token: token, decision: decision }).then(function (r) {
      if (r.ok) finish(DONE[decision], true); else failure(r);
    }).catch(function () { show("Couldn't reach the computer. Try again.", false); busy(false); });
  }
  once.addEventListener("click", function () { answer("once"); });
  always.addEventListener("click", function () { answer("always"); });
  deny.addEventListener("click", function () { answer("deny"); });
  busy(true);
  call("approval", { task: task, item: item, token: token }).then(function (r) {
    if (!r.ok) return failure(r);
    var a = r.j;
    chat.textContent = a.chat || ""; kind.textContent = a.kind || ""; title.textContent = a.title || "";
    if (a.detail) { detail.textContent = a.detail; detail.hidden = false; }
    if (a.always) { always.textContent = a.always; always.hidden = false; }
    req.hidden = false;
    if (a.status === "answered") return finish("Already answered.", false);
    if (a.status === "expired") return finish("This notification expired.", false);
    busy(false);
  }).catch(function () { finish("Couldn't reach the computer.", false); });
})();
