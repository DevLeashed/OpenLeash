// The in-page half of the browser plugin. Installed into an *isolated world*
// (Page.createIsolatedWorld) of each frame the agent looks at, never into the
// page's own JavaScript context: the page shares the DOM with this code but not
// its globals, so it cannot read, replace or poison `__ol`, the ref registry, or
// the functions below. Everything returns plain JSON (returnByValue).
//
// Called as `(<this file>)("e")` — the argument is the ref prefix for this
// frame (`e` for a tab's main frame, `f2e` for frame 2), so refs from different
// frames can never collide.
(function (PREFIX) {
  if (globalThis.__ol) return true;

  // ── ref registry ──────────────────────────────────────────────────────
  // A WeakMap keyed by element gives the same element the same ref on every
  // snapshot, and the WeakRef side lets a removed element be collected.
  const R = { byEl: new WeakMap(), byRef: new Map(), next: 1 };
  // Closed shadow roots, handed in by the harness via CDP (DOM.resolveNode on
  // a pierced DOM.getDocument): page script can't see them, neither can we.
  const closedRoots = new WeakMap();

  function refOf(el) {
    let r = R.byEl.get(el);
    if (!r) {
      r = PREFIX + R.next++;
      R.byEl.set(el, r);
      R.byRef.set(r, new WeakRef(el));
    }
    return r;
  }
  function get(ref) {
    const w = R.byRef.get(ref);
    const el = w && w.deref();
    return el && el.isConnected ? el : null;
  }

  const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link', 'title', 'base']);
  const INTERACTIVE = new Set([
    'button', 'link', 'checkbox', 'radio', 'switch', 'tab', 'menuitem', 'menuitemcheckbox',
    'menuitemradio', 'option', 'combobox', 'textbox', 'searchbox', 'slider', 'spinbutton',
    'treeitem', 'listbox', 'gridcell',
  ]);
  const STRUCT = new Set([
    'heading', 'list', 'listitem', 'navigation', 'main', 'banner', 'contentinfo', 'complementary',
    'form', 'dialog', 'alertdialog', 'table', 'row', 'cell', 'columnheader', 'rowheader', 'img',
    'region', 'alert', 'status', 'tablist', 'tabpanel', 'menu', 'menubar', 'toolbar', 'tree', 'grid',
    'radiogroup', 'article', 'figure', 'progressbar', 'search',
  ]);
  // Roles whose accessible name comes from their content. Inside one of these
  // the text is already in the name, so it is not repeated as text lines.
  const NAME_FROM_CONTENT = new Set([
    'button', 'link', 'heading', 'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option',
    'cell', 'columnheader', 'rowheader', 'treeitem', 'checkbox', 'radio', 'switch', 'gridcell',
  ]);

  const clip = (s, n) => {
    s = String(s == null ? '' : s).replace(/\s+/g, ' ').trim();
    return s.length > n ? s.slice(0, n - 1) + '…' : s;
  };
  const tagOf = (el) => (el.localName || '').toLowerCase();
  const shadowOf = (el) => el.shadowRoot || closedRoots.get(el) || null;

  function roleOf(el) {
    const explicit = (el.getAttribute('role') || '').trim().split(/\s+/)[0];
    if (explicit) return explicit;
    const t = tagOf(el);
    switch (t) {
      case 'a': case 'area': return el.hasAttribute('href') ? 'link' : 'generic';
      case 'button': case 'summary': return 'button';
      case 'input': {
        const ty = (el.getAttribute('type') || 'text').toLowerCase();
        const m = {
          button: 'button', submit: 'button', reset: 'button', image: 'button', file: 'button',
          color: 'button', checkbox: 'checkbox', radio: 'radio', range: 'slider', number: 'spinbutton',
          search: 'searchbox', hidden: 'none',
        };
        return m[ty] || 'textbox';
      }
      case 'textarea': return 'textbox';
      case 'select': return el.multiple || el.size > 1 ? 'listbox' : 'combobox';
      case 'option': return 'option';
      case 'img': return el.getAttribute('alt') === '' ? 'presentation' : 'img';
      case 'svg': return el.querySelector(':scope > title') || el.getAttribute('aria-label') ? 'img' : 'generic';
      case 'h1': case 'h2': case 'h3': case 'h4': case 'h5': case 'h6': return 'heading';
      case 'ul': case 'ol': case 'menu': return 'list';
      case 'li': return 'listitem';
      case 'nav': return 'navigation';
      case 'main': return 'main';
      case 'header': return el.closest('article,aside,main,nav,section') ? 'generic' : 'banner';
      case 'footer': return el.closest('article,aside,main,nav,section') ? 'generic' : 'contentinfo';
      case 'aside': return 'complementary';
      case 'form': return 'form';
      case 'search': return 'search';
      case 'section': return el.getAttribute('aria-label') || el.getAttribute('aria-labelledby') ? 'region' : 'generic';
      case 'table': return 'table';
      case 'tr': return 'row';
      case 'td': return 'cell';
      case 'th': return el.getAttribute('scope') === 'row' ? 'rowheader' : 'columnheader';
      case 'dialog': return 'dialog';
      case 'article': return 'article';
      case 'figure': return 'figure';
      case 'progress': return 'progressbar';
      case 'iframe': case 'frame': return 'iframe';
    }
    if (el.isContentEditable) return 'textbox';
    return 'generic';
  }

  function textOf(el) {
    let s = '';
    try { s = el.innerText; } catch (e) { s = ''; }
    if (!s || !s.trim()) {
      const sh = shadowOf(el);
      s = (sh && sh.textContent) || el.textContent || '';
    }
    if (!s.trim()) {
      // An icon-only link or button: its picture's alt or title is the name.
      const img = el.querySelector && el.querySelector('img[alt], [aria-label], svg title');
      if (img) s = img.getAttribute('alt') || img.getAttribute('aria-label') || img.textContent || '';
    }
    return s;
  }

  function byIdIn(el, id) {
    const root = el.getRootNode();
    return (root.getElementById && root.getElementById(id)) || el.ownerDocument.getElementById(id);
  }

  function nameOf(el, role) {
    const lb = el.getAttribute('aria-labelledby');
    if (lb) {
      const s = lb.split(/\s+/).map((id) => { const n = byIdIn(el, id); return n ? n.textContent : ''; }).join(' ');
      if (s.trim()) return clip(s, 100);
    }
    const al = el.getAttribute('aria-label');
    if (al && al.trim()) return clip(al, 100);
    const t = tagOf(el);
    if (t === 'input' || t === 'textarea' || t === 'select') {
      const ty = (el.type || '').toLowerCase();
      if (ty === 'button' || ty === 'submit' || ty === 'reset') {
        return clip(el.value || (ty === 'submit' ? 'Submit' : ty === 'reset' ? 'Reset' : ''), 100);
      }
      if (ty === 'image') return clip(el.alt || el.value || 'Submit', 100);
      if (el.labels && el.labels.length) return clip([...el.labels].map((l) => l.textContent).join(' '), 100);
      const wrap = el.closest('label');
      if (wrap) return clip(wrap.textContent, 100);
      if (el.title) return clip(el.title, 100);
      return '';
    }
    if (t === 'img') return clip(el.alt || el.title, 100);
    if (t === 'svg') {
      const ti = el.querySelector(':scope > title');
      return clip(ti ? ti.textContent : '', 100);
    }
    if (t === 'iframe' || t === 'frame') return clip(el.title || el.name || el.src, 100);
    if (t === 'fieldset') { const lg = el.querySelector(':scope > legend'); if (lg) return clip(lg.textContent, 100); }
    if (t === 'table') { const c = el.querySelector(':scope > caption'); if (c) return clip(c.textContent, 100); }
    if (t === 'figure') { const c = el.querySelector(':scope > figcaption'); if (c) return clip(c.textContent, 100); }
    if (NAME_FROM_CONTENT.has(role) || isInteractive(el, role)) return clip(textOf(el), 100);
    if (el.title) return clip(el.title, 100);
    return '';
  }

  function isInteractive(el, role) {
    if (INTERACTIVE.has(role)) return true;
    const t = tagOf(el);
    if (t === 'a' && el.hasAttribute('href')) return true;
    if (t === 'button' || t === 'select' || t === 'textarea' || t === 'summary') return true;
    if (t === 'input') return (el.type || '').toLowerCase() !== 'hidden';
    if (el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)) return true;
    if (el.hasAttribute('onclick')) return true;
    const ti = el.getAttribute('tabindex');
    if (ti !== null && Number(ti) >= 0) return true;
    return false;
  }

  function isVisible(el) {
    if (el.checkVisibility) {
      return el.checkVisibility({ checkVisibilityCSS: true, visibilityProperty: true });
    }
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    const cs = getComputedStyle(el);
    return cs.visibility !== 'hidden' && cs.display !== 'none';
  }

  function deepActive() {
    let a = document.activeElement;
    for (;;) {
      if (a && a.shadowRoot && a.shadowRoot.activeElement) { a = a.shadowRoot.activeElement; continue; }
      if (a && (a.localName === 'iframe' || a.localName === 'frame')) {
        let d = null;
        try { d = a.contentDocument; } catch (e) { d = null; }
        if (d && d.activeElement) { a = d.activeElement; continue; }
      }
      return a;
    }
  }

  function childDoc(el) {
    try { return el.contentDocument || null; } catch (e) { return null; }
  }

  function shortHref(el) {
    try {
      const u = new URL(el.href, location.href);
      const s = u.origin === location.origin ? u.pathname + u.search + u.hash : u.href;
      return clip(s, 120);
    } catch (e) {
      return clip(el.getAttribute('href'), 120);
    }
  }

  function valueOf(el, role) {
    const t = tagOf(el);
    if (t === 'input' || t === 'textarea') {
      if ((el.type || '').toLowerCase() === 'password') return el.value ? '•'.repeat(Math.min(el.value.length, 12)) : '';
      if (['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file'].includes((el.type || '').toLowerCase())) {
        if ((el.type || '').toLowerCase() === 'file') return el.files && el.files.length ? [...el.files].map((f) => f.name).join(', ') : '';
        return null;
      }
      return clip(el.value, 200);
    }
    if (t === 'select') {
      return clip([...el.selectedOptions].map((o) => o.label || o.text).join(', '), 200);
    }
    if (role === 'textbox' && el.isContentEditable) return clip(el.innerText, 200);
    const vn = el.getAttribute('aria-valuenow');
    if (vn !== null) return clip(el.getAttribute('aria-valuetext') || vn, 60);
    return null;
  }

  function checkedOf(el, role) {
    if (!['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'].includes(role)) return null;
    const a = el.getAttribute('aria-checked');
    if (a === 'mixed') return 'mixed';
    if (a === 'true' || a === 'false') return a === 'true';
    if ('indeterminate' in el && el.indeterminate) return 'mixed';
    if ('checked' in el) return !!el.checked;
    return false;
  }

  function disabledOf(el) {
    if (el.disabled) return true;
    if (el.getAttribute('aria-disabled') === 'true') return true;
    const fs = el.closest && el.closest('fieldset[disabled]');
    return !!fs && !fs.querySelector(':scope > legend')?.contains(el);
  }

  // ── snapshot ──────────────────────────────────────────────────────────
  function pushText(out, t) {
    const last = out[out.length - 1];
    if (last && last.role === 'text') {
      last.text = clip(last.text + ' ' + t, 300);
    } else {
      out.push({ role: 'text', text: t });
    }
  }

  function walkChildren(parent, out, ctx) {
    const sh = parent.nodeType === 1 ? shadowOf(parent) : null;
    if (sh) { walkList(sh.childNodes, out, ctx); return; }
    if (parent.nodeType === 1 && parent.localName === 'slot') {
      const assigned = parent.assignedNodes({ flatten: true });
      walkList(assigned.length ? assigned : parent.childNodes, out, ctx);
      return;
    }
    walkList(parent.childNodes, out, ctx);
  }

  function walkList(list, out, ctx) {
    for (const c of list) {
      if (ctx.count >= ctx.max) { ctx.truncated = true; return; }
      if (c.nodeType === 3) {
        if (!ctx.interactiveOnly && ctx.noText === 0) {
          const t = clip(c.data, 300);
          if (t) { pushText(out, t); ctx.count++; }
        }
        continue;
      }
      if (c.nodeType === 1) visit(c, out, ctx);
    }
  }

  function visit(el, out, ctx) {
    const t = tagOf(el);
    if (SKIP.has(t)) return;
    if (t === 'input' && (el.type || '').toLowerCase() === 'hidden') return;
    if (!isVisible(el)) {
      // display:contents has no box of its own, but its children do.
      let cs = null;
      try { cs = getComputedStyle(el); } catch (e) { cs = null; }
      if (cs && cs.display === 'contents') walkChildren(el, out, ctx);
      return;
    }
    if (t === 'iframe' || t === 'frame') {
      const node = { role: 'iframe', name: nameOf(el, 'iframe'), ref: refOf(el) };
      ctx.count++;
      const d = childDoc(el);
      if (d && d.documentElement && ctx.depth < 4) {
        node.children = [];
        ctx.depth++;
        walkChildren(d.body || d.documentElement, node.children, ctx);
        ctx.depth--;
      } else {
        // Cross-origin (or out-of-process): the harness snapshots it in its own
        // frame and splices the result in under this node.
        node.frame = 'remote';
      }
      out.push(node);
      return;
    }
    const role = roleOf(el);
    const inter = isInteractive(el, role);
    if (t === 'label' && el.control && !inter) {
      // Its words are already the control's name; don't repeat them as text.
      ctx.noText++;
      walkChildren(el, out, ctx);
      ctx.noText--;
      return;
    }
    if ((role === 'none' || role === 'presentation' || role === 'generic') && !inter) {
      walkChildren(el, out, ctx);
      return;
    }
    const name = nameOf(el, role);
    const struct = STRUCT.has(role) && !(role === 'img' && !name);
    if (!inter && !struct) { walkChildren(el, out, ctx); return; }
    if (ctx.interactiveOnly && !inter) { walkChildren(el, out, ctx); return; }
    const node = { role };
    if (name) node.name = name;
    if (inter) node.ref = refOf(el);
    const v = valueOf(el, role);
    if (v !== null && v !== '' && v !== undefined) node.value = v;
    const ch = checkedOf(el, role);
    if (ch !== null) node.checked = ch;
    if (inter && disabledOf(el)) node.disabled = true;
    const ex = el.getAttribute('aria-expanded');
    if (ex === 'true' || ex === 'false') node.expanded = ex === 'true';
    else if (t === 'details') node.expanded = el.open;
    if (el.getAttribute('aria-selected') === 'true') node.selected = true;
    if (el.required || el.getAttribute('aria-required') === 'true') node.required = true;
    if (role === 'heading') {
      const lv = Number(el.getAttribute('aria-level')) || Number((t.match(/^h([1-6])$/) || [])[1]) || 2;
      node.level = lv;
    }
    if (role === 'link' && el.href) node.href = shortHref(el);
    if ((role === 'textbox' || role === 'searchbox' || role === 'combobox') && el.placeholder && !name) {
      node.placeholder = clip(el.placeholder, 80);
    }
    if (ctx.focused === el) node.focused = true;
    ctx.count++;
    out.push(node);
    if (t === 'select') {
      const opts = [...el.options];
      if (!ctx.interactiveOnly) {
        node.children = opts.slice(0, 25).map((o) => {
          const n = { role: 'option', name: clip(o.label || o.text, 80) };
          if (o.selected) n.selected = true;
          if (o.disabled) n.disabled = true;
          return n;
        });
        if (opts.length > 25) node.more = opts.length - 25;
      }
      return;
    }
    if (t === 'input' || t === 'textarea' || t === 'img') return;
    const kids = [];
    const quiet = NAME_FROM_CONTENT.has(role) && !!name;
    if (quiet) ctx.noText++;
    walkChildren(el, ctx.interactiveOnly ? out : kids, ctx);
    if (quiet) ctx.noText--;
    if (!ctx.interactiveOnly && kids.length) node.children = kids;
  }

  function rootFor(selector) {
    if (!selector) return document.body || document.documentElement;
    const el = query(selector)[0];
    if (!el) throw new Error('no element matches selector ' + JSON.stringify(selector));
    return el;
  }

  function snapshot(opts) {
    opts = opts || {};
    const ctx = {
      count: 0, max: opts.maxNodes || 4000, truncated: false, depth: 0, noText: 0,
      interactiveOnly: !!opts.interactiveOnly, focused: deepActive(),
    };
    let root;
    try { root = rootFor(opts.selector); } catch (e) { return { error: String(e.message || e) }; }
    const nodes = [];
    if (opts.selector) visit(root, nodes, ctx); else walkChildren(root, nodes, ctx);
    return { url: location.href, title: document.title, nodes, truncated: ctx.truncated, count: ctx.count };
  }

  // ── selectors and text matching ──────────────────────────────────────
  // `a >>> b` pierces one shadow boundary per `>>>`; a plain selector that
  // matches nothing in the light DOM is retried inside every open (and
  // registered closed) shadow root, so web components work without syntax.
  function allRoots(start, acc) {
    acc.push(start);
    const it = (start.ownerDocument || start).createTreeWalker(start, NodeFilter.SHOW_ELEMENT);
    for (let n = it.currentNode; n; n = it.nextNode()) {
      const sh = n.nodeType === 1 ? shadowOf(n) : null;
      if (sh) allRoots(sh, acc);
      if (n.localName === 'iframe' || n.localName === 'frame') {
        const d = childDoc(n);
        if (d && d.documentElement) allRoots(d, acc);
      }
    }
    return acc;
  }

  function query(selector) {
    const parts = String(selector).split('>>>').map((s) => s.trim()).filter(Boolean);
    if (parts.length > 1) {
      let scopes = [document];
      parts.forEach((p, i) => {
        const next = [];
        for (const s of scopes) for (const el of s.querySelectorAll(p)) next.push(el);
        scopes = i < parts.length - 1 ? next.map((el) => shadowOf(el) || el) : next;
      });
      return scopes.filter((x) => x.nodeType === 1);
    }
    const out = [...document.querySelectorAll(selector)];
    if (out.length) return out;
    for (const r of allRoots(document, []).slice(1)) {
      for (const el of r.querySelectorAll(selector)) out.push(el);
    }
    return out;
  }

  function norm(s) { return clip(s, 400).toLowerCase(); }

  function candidates(limit) {
    const out = [];
    for (const r of allRoots(document, [])) {
      for (const el of r.querySelectorAll('*')) {
        const role = roleOf(el);
        if (!isInteractive(el, role) || !isVisible(el)) continue;
        out.push({ ref: refOf(el), role, name: nameOf(el, role) });
        if (out.length >= limit) return out;
      }
    }
    return out;
  }

  function findByText(text) {
    const q = norm(text);
    if (!q) return [];
    const scored = [];
    for (const r of allRoots(document, [])) {
      for (const el of r.querySelectorAll('*')) {
        if (SKIP.has(tagOf(el))) continue;
        const role = roleOf(el);
        const inter = isInteractive(el, role);
        let name = inter ? nameOf(el, role) : '';
        if (!name && !inter) {
          // Non-interactive: only its own text, so the innermost element that
          // carries the words wins over every ancestor that contains them.
          let own = '';
          for (const c of el.childNodes) if (c.nodeType === 3) own += c.data;
          name = own;
        }
        const n = norm(name);
        if (!n) continue;
        let score = 0;
        if (n === q) score = 100;
        else if (n.startsWith(q)) score = 60;
        else if (n.includes(q)) score = 40;
        if (!score) continue;
        if (!isVisible(el)) continue;
        if (inter) score += 20;
        scored.push({ el, score });
      }
    }
    scored.sort((a, b) => b.score - a.score);
    return scored;
  }

  function describe(el) {
    if (!el || el.nodeType !== 1) return { ref: '', role: '', name: '', tag: '' };
    const role = roleOf(el);
    return { ref: refOf(el), role, name: nameOf(el, role) || clip(textOf(el), 60), tag: tagOf(el) };
  }

  // Resolve a target ({ref} | {selector} | {text}) to one element.
  function resolve(t) {
    let el = null;
    let matches = 1;
    if (t.ref) {
      el = get(t.ref);
      if (!el) {
        const known = R.byRef.has(t.ref);
        return {
          error: known
            ? `ref ${t.ref} is gone (that element was removed or the page changed) — take a new snapshot`
            : `there is no ref ${t.ref} on this page — take a snapshot and use a ref from it`,
          candidates: candidates(300),
        };
      }
    } else if (t.selector) {
      let all;
      try { all = query(t.selector); } catch (e) { return { error: `invalid selector ${JSON.stringify(t.selector)}: ${e.message}` }; }
      matches = all.length;
      el = all.find((x) => isVisible(x)) || all[0] || null;
      if (!el) return { error: 'nomatch', candidates: candidates(300) };
    } else if (t.text) {
      const found = findByText(t.text);
      if (!found.length) return { error: 'nomatch', candidates: candidates(300) };
      const best = found[0].score;
      matches = found.filter((f) => f.score === best).length;
      el = found[0].el;
    } else {
      return { error: 'no target: give a ref, selector or text' };
    }
    return Object.assign(describe(el), { matches });
  }

  function frameOffset(el) {
    // Viewport coordinates of `el`'s document inside this world's top frame:
    // walk out through same-origin iframes, adding each one's content box.
    let x = 0, y = 0;
    let w = el.ownerDocument.defaultView;
    while (w && w !== window && w.frameElement) {
      const fe = w.frameElement;
      const r = fe.getBoundingClientRect();
      const cs = getComputedStyle(fe);
      x += r.left + fe.clientLeft + parseFloat(cs.paddingLeft || '0');
      y += r.top + fe.clientTop + parseFloat(cs.paddingTop || '0');
      w = w.parent;
    }
    return { x, y };
  }

  function deepHit(doc, x, y) {
    let h = doc.elementFromPoint(x, y);
    while (h && shadowOf(h)) {
      const inner = shadowOf(h).elementFromPoint(x, y);
      if (!inner || inner === h) break;
      h = inner;
    }
    return h;
  }

  function composedContains(a, b) {
    for (let n = b; n; n = n.parentNode || (n instanceof ShadowRoot ? n.host : null)) {
      if (n === a) return true;
    }
    return false;
  }

  const raf = () => new Promise((r) => requestAnimationFrame(() => r()));
  const later = (ms) => new Promise((r) => setTimeout(r, ms));

  // Is `ref` ready to receive a pointer event? Scrolls it into view, waits for
  // its box to stop moving, and hit-tests its click point.
  async function actionable(ref, opts) {
    opts = opts || {};
    const el = get(ref);
    if (!el) return { state: 'detached' };
    if (!isVisible(el)) {
      // A styled-away checkbox / file input is usually driven through its label.
      if (opts.allowLabel && el.labels && el.labels.length) {
        const lab = [...el.labels].find((l) => isVisible(l));
        if (lab) return actionable(refOf(lab), {});
      }
      return { state: 'hidden', desc: describe(el) };
    }
    if (opts.needEnabled !== false && disabledOf(el)) return { state: 'disabled', desc: describe(el) };
    const vr = el.getBoundingClientRect();
    const vw = el.ownerDocument.defaultView;
    if (vr.top < 0 || vr.left < 0 || vr.bottom > vw.innerHeight || vr.right > vw.innerWidth) {
      el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    }
    let r1 = el.getBoundingClientRect();
    await Promise.race([raf().then(raf), later(100)]);
    let r2 = el.getBoundingClientRect();
    if (Math.abs(r1.x - r2.x) > 1 || Math.abs(r1.y - r2.y) > 1 || Math.abs(r1.width - r2.width) > 1) {
      return { state: 'moving', desc: describe(el) };
    }
    if (r2.width <= 0 || r2.height <= 0) return { state: 'hidden', desc: describe(el) };
    // The visible part of the box, so a half-offscreen element is clicked
    // where it can actually be hit.
    const left = Math.max(r2.left, 0), top = Math.max(r2.top, 0);
    const right = Math.min(r2.right, vw.innerWidth), bottom = Math.min(r2.bottom, vw.innerHeight);
    if (right <= left || bottom <= top) return { state: 'offscreen', desc: describe(el) };
    let cx = (left + right) / 2, cy = (top + bottom) / 2;
    if (opts.position) { cx = r2.left + opts.position.x; cy = r2.top + opts.position.y; }
    const hit = deepHit(el.ownerDocument, cx, cy);
    const off = frameOffset(el);
    const res = {
      state: 'ok', ref: refOf(el), x: cx + off.x, y: cy + off.y,
      rect: { x: r2.left + off.x, y: r2.top + off.y, width: r2.width, height: r2.height },
      desc: describe(el),
    };
    if (opts.hitTest !== false && hit && hit !== el && !composedContains(el, hit) && !composedContains(hit, el)) {
      // A label for this control is fine to click through.
      const forThis = hit.localName === 'label' && hit.control === el;
      if (!forThis) { res.state = 'covered'; res.by = describe(hit); }
    }
    return res;
  }

  function rectOf(ref) {
    const el = get(ref);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const off = frameOffset(el);
    return { x: r.left + off.x, y: r.top + off.y, width: r.width, height: r.height };
  }

  // ── facts for the sensitivity check ──────────────────────────────────
  function facts(ref) {
    const el = get(ref);
    if (!el) return null;
    const role = roleOf(el);
    const form = el.form || (el.closest && el.closest('form'));
    const has = (sel) => !!(form && form.querySelector(sel));
    return {
      tag: tagOf(el),
      role,
      input_type: (el.getAttribute('type') || '').toLowerCase(),
      text: clip([nameOf(el, role), el.getAttribute('aria-label') || '', el.getAttribute('title') || ''].filter(Boolean).join(' '), 200),
      attrs: clip([el.id, el.getAttribute('name') || '', typeof el.className === 'string' ? el.className : ''].join(' '), 200),
      autocomplete: (el.getAttribute('autocomplete') || '').toLowerCase(),
      in_form: !!form,
      form_action: form ? String(form.action || '') : '',
      // The raw attribute: the DOM property defaults to "get" when absent.
      form_method: form ? (form.getAttribute('method') || '').toLowerCase() : '',
      form_has_password: has('input[type=password]'),
      form_has_payment: has('[autocomplete^="cc-"], input[name*="card" i], input[name*="cvc" i], input[name*="cvv" i], input[id*="card" i]'),
    };
  }

  function focusedRef() {
    const a = deepActive();
    if (!a || a === document.body || a === document.documentElement) return null;
    return refOf(a);
  }

  // ── form helpers ─────────────────────────────────────────────────────
  function focus(ref, selectAll) {
    const el = get(ref);
    if (!el) return { error: 'gone' };
    el.focus({ preventScroll: false });
    if (selectAll) {
      if (typeof el.select === 'function' && /^(input|textarea)$/.test(tagOf(el))) {
        try { el.select(); } catch (e) { /* number inputs refuse select() */ }
      } else if (el.isContentEditable) {
        const range = el.ownerDocument.createRange();
        range.selectNodeContents(el);
        const sel = el.ownerDocument.getSelection();
        sel.removeAllRanges();
        sel.addRange(range);
      }
    } else if (typeof el.setSelectionRange === 'function') {
      try { const n = (el.value || '').length; el.setSelectionRange(n, n); } catch (e) { /* not a text input */ }
    }
    const active = deepActive();
    return { focused: active === el || composedContains(el, active), editable: isEditable(el) };
  }

  function isEditable(el) {
    const t = tagOf(el);
    if (t === 'textarea') return !el.readOnly && !el.disabled;
    if (t === 'input') {
      const ty = (el.type || 'text').toLowerCase();
      return !['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file', 'range', 'color', 'hidden'].includes(ty) && !el.readOnly && !el.disabled;
    }
    return !!el.isContentEditable;
  }

  function valueNow(ref) {
    const el = get(ref);
    if (!el) return null;
    if ('value' in el && tagOf(el) !== 'button') return (el.type || '') === 'password' ? '•'.repeat(Math.min((el.value || '').length, 12)) : String(el.value);
    return clip(el.innerText, 500);
  }

  function selectOption(ref, wanted) {
    let el = get(ref);
    if (!el) return { error: 'gone' };
    if (tagOf(el) !== 'select') {
      const inner = el.querySelector && el.querySelector('select');
      if (inner) el = inner;
      else return { error: `${describe(el).role} "${describe(el).name}" is not a <select>; click it to open it, then click the option` };
    }
    if (el.disabled) return { error: 'that select is disabled' };
    const opts = [...el.options];
    const picked = [];
    const want = (Array.isArray(wanted) ? wanted : [wanted]).map(String);
    for (const w of want) {
      const lw = w.trim().toLowerCase();
      const o = opts.find((o) => o.value === w) || opts.find((o) => (o.label || o.text).trim() === w.trim())
        || opts.find((o) => (o.label || o.text).trim().toLowerCase() === lw)
        || opts.find((o) => (o.label || o.text).trim().toLowerCase().includes(lw));
      if (!o) {
        return { error: `no option ${JSON.stringify(w)}; options are: ${opts.slice(0, 40).map((o) => JSON.stringify(o.label || o.text)).join(', ')}` };
      }
      if (o.disabled) return { error: `option ${JSON.stringify(o.label || o.text)} is disabled` };
      picked.push(o);
    }
    if (!el.multiple && picked.length > 1) return { error: 'that select takes one option' };
    el.focus();
    if (el.multiple) for (const o of opts) o.selected = picked.includes(o);
    else el.value = picked[0].value;
    el.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return { selected: picked.map((o) => o.label || o.text) };
  }

  function checkedNow(ref) {
    const el = get(ref);
    if (!el) return null;
    const role = roleOf(el);
    const c = checkedOf(el, role);
    if (c === null) {
      if ('checked' in el) return !!el.checked;
      return null;
    }
    return c;
  }

  function fileInput(ref) {
    let el = get(ref);
    if (!el) return { error: 'gone' };
    if (tagOf(el) === 'label' && el.control) el = el.control;
    if (!(tagOf(el) === 'input' && (el.type || '').toLowerCase() === 'file')) {
      const inner = el.querySelector && el.querySelector('input[type=file]');
      if (inner) el = inner;
      else return { error: `${describe(el).role} "${describe(el).name}" is not a file input` };
    }
    return { ref: refOf(el), multiple: !!el.multiple };
  }

  function el(ref) { return get(ref); }

  // ── reading ──────────────────────────────────────────────────────────
  function readText(max, selector) {
    let root = document.body || document.documentElement;
    if (selector) {
      const m = query(selector)[0];
      if (!m) return { error: 'no element matches selector ' + JSON.stringify(selector) };
      root = m;
    }
    const t = (root && root.innerText) || '';
    return { url: location.href, title: document.title, text: t.slice(0, max || 40000), length: t.length };
  }

  function queryAll(selector, limit) {
    let all;
    try { all = query(selector); } catch (e) { return { error: `invalid selector: ${e.message}` }; }
    const out = all.slice(0, limit || 50).map((e) => {
      const attrs = {};
      for (const a of [...e.attributes].slice(0, 12)) {
        if (a.name === 'value' && (e.type || '') === 'password') continue;
        attrs[a.name] = clip(a.value, 200);
      }
      const role = roleOf(e);
      const o = { tag: tagOf(e), text: clip(textOf(e), 300), attrs, visible: isVisible(e) };
      if (isInteractive(e, role)) o.ref = refOf(e);
      return o;
    });
    return { count: all.length, matches: out };
  }

  function info() {
    const hs = [...document.querySelectorAll('h1,h2,h3')].slice(0, 25).map((h) => clip(h.innerText, 120)).filter(Boolean);
    return {
      url: location.href, title: document.title, readyState: document.readyState,
      viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio },
      scroll: { x: Math.round(scrollX), y: Math.round(scrollY), height: document.documentElement.scrollHeight },
      headings: hs, links: document.links.length, forms: document.forms.length,
      lang: document.documentElement.lang || '',
    };
  }

  function stable() {
    return new Promise((resolve) => {
      const t0 = Date.now(); let last = -1, same = 0;
      const tick = () => {
        const n = document.body ? document.body.innerHTML.length : 0;
        if (n === last) { if (++same >= 3) return resolve(true); } else { same = 0; last = n; }
        if (Date.now() - t0 > 1500) return resolve(false);
        setTimeout(tick, 100);
      };
      tick();
    });
  }

  function textPresent(text) {
    const q = norm(text);
    if (!q) return false;
    for (const r of allRoots(document, [])) {
      const t = r.body ? r.body.innerText : r.textContent;
      if (norm(t || '').includes(q)) return true;
    }
    return false;
  }

  function selectorState(selector) {
    let all;
    try { all = query(selector); } catch (e) { return { error: e.message }; }
    return { attached: all.length > 0, visible: all.some((e) => isVisible(e)) };
  }

  // ── panel inspect ────────────────────────────────────────────────────
  function cssPath(el) {
    if (el.id && el.ownerDocument.querySelectorAll('#' + CSS.escape(el.id)).length === 1) return '#' + CSS.escape(el.id);
    const parts = [];
    for (let n = el; n && n.nodeType === 1 && parts.length < 8; n = n.parentElement) {
      let p = n.localName;
      if (n.id) { parts.unshift(p + '#' + CSS.escape(n.id)); break; }
      const sib = n.parentElement ? [...n.parentElement.children].filter((c) => c.localName === n.localName) : [];
      if (sib.length > 1) p += `:nth-of-type(${sib.indexOf(n) + 1})`;
      parts.unshift(p);
    }
    return parts.join(' > ');
  }

  const STYLE_KEYS = ['color', 'background-color', 'font-size', 'font-weight', 'font-family', 'line-height', 'letter-spacing',
    'margin', 'padding', 'border', 'border-radius', 'width', 'height', 'display', 'gap', 'opacity'];

  function elementAt(x, y) {
    const h = deepHit(document, x, y);
    if (!h) return null;
    const d = describe(h);
    const r = h.getBoundingClientRect();
    const cs = getComputedStyle(h);
    const styles = {};
    for (const k of STYLE_KEYS) styles[k] = cs.getPropertyValue(k);
    return {
      ref: d.ref, selector: cssPath(h), tag: d.tag, role: d.role, name: d.name,
      bbox: { x: r.left, y: r.top, width: r.width, height: r.height }, styles,
    };
  }

  // ── set-of-marks overlay ─────────────────────────────────────────────
  const MARK_ID = '__openleash_marks__';
  function marks(show) {
    const old = document.getElementById(MARK_ID);
    if (old) old.remove();
    if (!show) return 0;
    const host = document.createElement('div');
    host.id = MARK_ID;
    host.setAttribute('style', 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;');
    const root = host.attachShadow({ mode: 'closed' });
    let n = 0;
    for (const c of candidates(400)) {
      const e = get(c.ref);
      if (!e) continue;
      const r = e.getBoundingClientRect();
      const off = frameOffset(e);
      const x = r.left + off.x, y = r.top + off.y;
      if (r.width < 2 || r.height < 2 || y > innerHeight || x > innerWidth || y + r.height < 0 || x + r.width < 0) continue;
      const hue = (n * 47) % 360;
      const box = document.createElement('div');
      box.setAttribute('style', `position:fixed;left:${x}px;top:${y}px;width:${r.width}px;height:${r.height}px;outline:2px solid hsl(${hue} 90% 45%);box-sizing:border-box;`);
      const tag = document.createElement('div');
      tag.textContent = c.ref;
      tag.setAttribute('style', `position:fixed;left:${Math.max(x, 0)}px;top:${Math.max(y - 15, 0)}px;font:bold 11px/14px monospace;color:#fff;background:hsl(${hue} 90% 35%);padding:0 3px;border-radius:2px;`);
      root.append(box, tag);
      n++;
    }
    document.documentElement.appendChild(host);
    return n;
  }

  function registerClosed(root) {
    if (root && root.host) closedRoots.set(root.host, root);
    return true;
  }

  globalThis.__ol = {
    snapshot, resolve, actionable, rectOf, facts, focusedRef, focus, valueNow, selectOption,
    checkedNow, fileInput, el, readText, queryAll, info, stable, textPresent, selectorState,
    elementAt, marks, registerClosed, candidates,
    count: () => document.getElementsByTagName('*').length,
  };
  return true;
})
