// Pairing form for OpenLeash remote control. Exchanges a six-digit code shown
// on the computer for a session cookie, then loads the app.
(function () {
  "use strict";
  var form = document.getElementById("f");
  var input = document.getElementById("code");
  var btn = document.getElementById("go");
  var msg = document.getElementById("msg");
  form.addEventListener("submit", function (e) {
    e.preventDefault();
    var code = input.value.replace(/\D/g, "");
    if (code.length !== 6) { msg.textContent = "Enter the six digits shown on the computer."; return; }
    btn.disabled = true;
    msg.textContent = "";
    fetch("/__openleash/login", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code: code }),
    })
      .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
      .then(function (res) {
        if (res.ok) { location.replace("/"); return; }
        msg.textContent = (res.j && res.j.error) || "Pairing failed.";
        btn.disabled = false;
        input.select();
      })
      .catch(function () {
        msg.textContent = "Couldn't reach the computer.";
        btn.disabled = false;
      });
  });
})();
