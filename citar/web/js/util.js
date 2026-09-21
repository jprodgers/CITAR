// DOM helpers, toasts and modals.

// Node.append() turns null into the text "null"; skip empty children the way el() does, so conditional parts
// (`cond ? el(...) : null`) can be passed to append() as well.
const nativeAppend = Element.prototype.append;
Element.prototype.append = function (...nodes) { return nativeAppend.apply(this, nodes.filter((n) => n != null && n !== false)); };

export function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") node.className = v;
    else if (k === "style" && typeof v === "object") Object.assign(node.style, v);
    else if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2), v);
    else if (k === "html") node.innerHTML = v;
    else if (k in node && k !== "list") { try { node[k] = v; } catch (e) { node.setAttribute(k, v); } }
    else node.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    node.appendChild(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return node;
}

export function clear(node) { while (node.firstChild) node.removeChild(node.firstChild); return node; }

export function toast(msg, kind = "info", ms = 3500) {
  const box = document.getElementById("toasts");
  const t = el("div", { class: `toast ${kind}` }, msg);
  box.appendChild(t);
  // keep the pile short: the oldest messages make way (they are all in the event log too)
  while (box.children.length > 4) box.firstChild.remove();
  setTimeout(() => t.remove(), ms);
}

export function modal({ title, content, footer, narrow = false, onClose }) {
  const root = document.getElementById("modal-root");
  let back;
  const close = () => { back.remove(); document.removeEventListener("keydown", onKey); if (onClose) onClose(); };
  const onKey = (e) => { if (e.key === "Escape") close(); };
  back = el("div", { class: "modal-back", onmousedown: (e) => { if (e.target === back) close(); } },
    el("div", { class: `modal ${narrow ? "narrow" : ""}` },
      el("header", {}, el("h2", {}, title), el("button", { onclick: close }, "✕")),
      el("div", { class: "content" }, content),
      footer ? el("footer", {}, footer) : null));
  root.appendChild(back);
  document.addEventListener("keydown", onKey);
  return { close, node: back };
}

export function prompt(title, label, initial = "") {
  return new Promise((resolve) => {
    const input = el("input", { value: initial, style: { width: "100%" } });
    let done = false;
    const m = modal({
      title, narrow: true,
      content: el("div", { class: "col" }, el("label", { class: "muted" }, label), input),
      footer: [el("button", { onclick: () => { m.close(); } }, "Cancel"),
               el("button", { class: "primary", onclick: () => { done = true; m.close(); resolve(input.value); } }, "OK")],
      onClose: () => { if (!done) resolve(null); },
    });
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") { done = true; m.close(); resolve(input.value); } });
    setTimeout(() => input.focus(), 30);
  });
}

export function confirmBox(title, text) {
  return new Promise((resolve) => {
    let done = false;
    const m = modal({
      title, narrow: true, content: el("p", {}, text),
      footer: [el("button", { onclick: () => m.close() }, "Cancel"),
               el("button", { class: "primary", onclick: () => { done = true; m.close(); resolve(true); } }, "Confirm")],
      onClose: () => { if (!done) resolve(false); },
    });
  });
}

export function fmt(n, digits = 0) {
  if (n == null || Number.isNaN(n)) return "–";
  return Number(n).toFixed(digits).replace(/\.0+$/, "");
}

export function signed(n) {
  if (n == null) return "–";
  const v = Math.round(n * 10) / 10;
  return (v >= 0 ? "+" : "") + v;
}

// Ruleset objects are keyed by their display names (UnCiv style), so an id is already a name.
export function nameOf(rules, kind, id) {
  return id || "";
}

// Redraw part of a page without losing the reader's place: the window's scroll position (both directions) and the
// scroll position of every scrollable box inside `card` are restored. A box marked data-follow="1" keeps following
// its newest text only if the reader was already at its bottom. Nothing is redrawn while text in the card is selected.
export function keepPlace(card, fn) {
  const sel = window.getSelection();
  if (sel && !sel.isCollapsed && sel.anchorNode && card.contains(sel.anchorNode)) return false;
  const boxes = () => [...card.querySelectorAll("pre, .keep-scroll")];
  const before = boxes().map((e) => ({ top: e.scrollTop, left: e.scrollLeft, atEnd: e.scrollTop + e.clientHeight >= e.scrollHeight - 4 }));
  const x = window.scrollX, y = window.scrollY;
  fn();
  boxes().forEach((e, i) => {
    const s = before[i];
    if (e.dataset.follow === "1" && (!s || s.atEnd)) { e.scrollTop = e.scrollHeight; return; }
    if (s) { e.scrollTop = s.top; e.scrollLeft = s.left; }
  });
  window.scrollTo(x, y);
  return true;
}
