// util.modal's Escape (citar/web/js/util.js): it closes the topmost dialog only, so a dialog opened from another (the
// Servers page's API key from a machine's Connection & models, a confirmation, a prompt) leaves the one under it, and
// that one's unsaved edits, alone. util.js is a browser module; the few parts of the DOM it uses are stood in for
// below (the repository has no DOM library). Run with `node --test "citar/web/tests/*.test.mjs"`.
import { test } from "node:test";
import assert from "node:assert/strict";

class FakeNode {
  constructor() { this.parentNode = null; this.childNodes = []; }
  appendChild(child) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.childNodes.push(child);
    return child;
  }
  removeChild(child) { this.childNodes.splice(this.childNodes.indexOf(child), 1); child.parentNode = null; return child; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  get firstChild() { return this.childNodes[0] || null; }
  get isConnected() { let n = this; while (n.parentNode) n = n.parentNode; return n === documentRoot; }
}

class FakeText extends FakeNode {
  constructor(text) { super(); this.textContent = text; }
}

// Listeners in the order they were added; one removed while an event is being dispatched is not called (as in the DOM).
class Listeners {
  constructor() { this.list = []; }
  add(type, fn) { if (!this.list.some((l) => l.type === type && l.fn === fn)) this.list.push({ type, fn }); }
  remove(type, fn) { this.list = this.list.filter((l) => !(l.type === type && l.fn === fn)); }
  dispatch(type, event) {
    for (const l of [...this.list]) if (l.type === type && this.list.includes(l)) l.fn(event);
  }
  count(type) { return this.list.filter((l) => l.type === type).length; }
}

class FakeElement extends FakeNode {
  constructor(tag) { super(); this.tagName = tag.toUpperCase(); this.className = ""; this.style = {}; this.attributes = {}; this.events = new Listeners(); }
  append(...nodes) { for (const n of nodes) this.appendChild(n instanceof FakeNode ? n : new FakeText(String(n))); }
  setAttribute(k, v) { this.attributes[k] = String(v); }
  focus() {}
  addEventListener(type, fn) { this.events.add(type, fn); }
  removeEventListener(type, fn) { this.events.remove(type, fn); }
  get children() { return this.childNodes.filter((n) => n instanceof FakeElement); }
  get lastElementChild() { const c = this.children; return c.length ? c[c.length - 1] : null; }
}

const documentRoot = new FakeElement("html");
const modalRoot = documentRoot.appendChild(new FakeElement("div"));
const documentEvents = new Listeners();
globalThis.Node = FakeNode;
globalThis.Element = FakeElement;
globalThis.document = {
  createElement: (tag) => new FakeElement(tag),
  createTextNode: (text) => new FakeText(text),
  getElementById: (id) => (id === "modal-root" ? modalRoot : null),
  addEventListener: (type, fn) => documentEvents.add(type, fn),
  removeEventListener: (type, fn) => documentEvents.remove(type, fn),
};

// imported after the stand-ins exist: util.js patches Element.prototype.append when it loads
const { modal, confirmBox, prompt, el, clear } = await import("../js/util.js");

function press(key) {
  const event = { key, stopped: false, stopPropagation() { this.stopped = true; } };
  documentEvents.dispatch("keydown", event);
  return event;
}

function reset() {
  clear(modalRoot);
  for (const l of [...documentEvents.list]) documentEvents.remove(l.type, l.fn);
}

test("Escape closes the topmost dialog only, then the one under it", () => {
  reset();
  const closed = [];
  const outer = modal({ title: "Connection & models — Box", content: el("p", {}, "the editor"), onClose: () => closed.push("outer") });
  const inner = modal({ title: "API key — Box", content: el("p", {}, "the key"), narrow: true, onClose: () => closed.push("inner") });
  assert.equal(modalRoot.lastElementChild, inner.node);

  const first = press("Escape");
  assert.deepEqual(closed, ["inner"]);
  assert.equal(first.stopped, true, "the page's own Escape (an editor's, say) is not also told");
  assert.deepEqual(modalRoot.children, [outer.node]);
  assert.equal(documentEvents.count("keydown"), 1);

  press("Escape");
  assert.deepEqual(closed, ["inner", "outer"]);
  assert.deepEqual(modalRoot.children, []);
  assert.equal(documentEvents.count("keydown"), 0, "a closed dialog stops listening");
});

test("other keys close nothing, and closing the inner dialog leaves Escape to the outer one", () => {
  reset();
  const closed = [];
  modal({ title: "Costs — Box", content: el("p", {}, "periods"), onClose: () => closed.push("outer") });
  const inner = modal({ title: "Electricity plans", content: el("p", {}, "plans"), onClose: () => closed.push("inner") });
  for (const key of ["Enter", "a", "Esc ", "Tab"]) assert.equal(press(key).stopped, false, key);
  assert.deepEqual(closed, []);
  inner.close();
  assert.deepEqual(closed, ["inner"]);
  press("Escape");
  assert.deepEqual(closed, ["inner", "outer"]);
  assert.equal(modalRoot.children.length, 0);
});

test("Escape in a confirmation or a prompt answers it and keeps the dialog it was asked from", async () => {
  reset();
  let outerClosed = false;
  const outer = modal({ title: "API key — Box", content: el("p", {}, "the key"), onClose: () => { outerClosed = true; } });
  const asked = confirmBox("Remove the key?", "It is deleted from the key store.");
  press("Escape");
  assert.equal(await asked, false);
  assert.equal(outerClosed, false);
  assert.deepEqual(modalRoot.children, [outer.node]);

  const named = prompt("A new cost period", "From (YYYY-MM-DD)", "2026-10-01");
  press("Escape");
  assert.equal(await named, null);
  assert.equal(outerClosed, false);
  assert.deepEqual(modalRoot.children, [outer.node]);

  press("Escape");
  assert.equal(outerClosed, true);
});

test("a dialog the router cleared away stops listening, and blocks no later dialog's Escape", () => {
  reset();
  const closed = [];
  modal({ title: "Hardware & power — Box", content: el("p", {}, "x"), onClose: () => closed.push("cleared") });
  clear(modalRoot);  // what app.js does on navigation
  modal({ title: "Quiet hours — Box", content: el("p", {}, "y"), onClose: () => closed.push("later") });
  press("Escape");
  assert.deepEqual(closed, ["later"]);
  assert.equal(documentEvents.count("keydown"), 0);
  assert.equal(modalRoot.children.length, 0);
});
