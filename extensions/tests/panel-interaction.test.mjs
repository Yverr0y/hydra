// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const source = readFileSync("extensions/chrome/content.js", "utf8");
const functions = ["buildPanel", "renderRows", "showPanel", "hidePanel", "schedulePanelHide"]
  .map((name) => {
    const start = source.indexOf(`function ${name}(`);
    assert.ok(start >= 0, `missing ${name}`);
    return source.slice(start, source.indexOf("\n}\n", start) + 3);
  }).join("\n");

class Node {
  children = [];
  listeners = [];
  style = {};
  className = "";
  classList = {
    contains: (name) => this.className.split(/\s+/).includes(name),
    add: (name) => { if (!this.classList.contains(name)) this.className += ` ${name}`; },
    remove: (name) => { this.className = this.className.split(/\s+/).filter((n) => n !== name).join(" "); },
    toggle: (name) => {
      if (this.classList.contains(name)) this.classList.remove(name);
      else this.classList.add(name);
    },
  };
  append(...nodes) {
    for (const node of nodes) {
      node.parent = this;
      this.children.push(node);
    }
  }
  set textContent(value) { this.text = value; this.children = []; }
  get textContent() { return this.text || ""; }
  attachShadow() { this.shadow = new Node(); this.shadow.parent = this; return this.shadow; }
  addEventListener(type, handler, capture = false) { this.listeners.push({ type, handler, capture }); }
  setPointerCapture() {}
  fire(type, props = {}) {
    const path = [];
    for (let node = this; node; node = node.parent) path.push(node);
    let stopped = false;
    const event = {
      target: this, button: 0, pointerId: 1, clientX: 0, clientY: 0,
      composedPath: () => path,
      preventDefault() {},
      stopPropagation() { stopped = true; },
      ...props,
    };
    for (const capture of [true, false]) {
      for (const node of capture ? [...path].reverse() : path) {
        for (const listener of node.listeners) {
          if (listener.type === type && listener.capture === capture) listener.handler(event);
        }
        if (stopped) return;
      }
    }
  }
}

function panel(tagName = "VIDEO") {
  const document = new Node();
  document.createElement = () => new Node();
  document.documentElement = new Node();
  document.append(document.documentElement);
  const timers = new Map();
  let nextTimer = 0;
  const load = new Function("document", "setTimeout", "clearTimeout", `
    let panelHost, panelEl, panelTitle, panelCaret, panelList, panelTarget;
    let panelSingle = false, panelRows = [], panelTimer, panelDrag = null;
    let panelDragged = false, panelEnabled = true, panelLingerMs = 10000;
    let panelOffset = { x: 10, y: 10 };
    const panelDismissed = new WeakSet(), DRAG_SLOP = 5, DRM_WHY = "Protected";
    const pageItems = { streams: [] };
    let rows = [{ label: "360p" }, { label: "720p" }], downloads = 0;
    const buildRows = () => rows, playingSrc = () => "", pageTitle = () => "Clip";
    const brandIcon = () => document.createElement("img");
    const placePanel = () => {}, savePanelOffset = () => {};
    const sendSingle = () => { downloads++; };
    const sendRow = (row, done) => { downloads++; done({ ok: true }); };
    ${functions}
    return {
      showPanel, hidePanel, setRows: (next) => { rows = next; },
      setTimeout: (ms) => { panelLingerMs = ms; },
      get host() { return panelHost; }, get wrap() { return panelEl; },
      get menu() { return panelList; }, get downloads() { return downloads; },
      get target() { return panelTarget; }
    };
  `);
  const api = load(document, (callback) => {
    timers.set(++nextTimer, callback);
    return nextTimer;
  }, (id) => timers.delete(id));
  const player = { tagName };
  api.showPanel(player);
  const bar = api.wrap.children[0];
  const outside = new Node();
  document.documentElement.append(outside);
  return { api, document, player, bar, outside, timers };
}

for (const tagName of ["VIDEO", "AUDIO"]) {
  test(`${tagName}: detection and accidental hover leave the menu closed`, () => {
    const { api, bar } = panel(tagName);
    assert.equal(api.wrap.classList.contains("open"), false);
    bar.fire("pointerover");
    bar.children[0].fire("pointerover");
    assert.equal(api.wrap.classList.contains("open"), false);
    assert.equal(api.downloads, 0);
    assert.equal(bar.children[1].textContent, `Download this ${tagName.toLowerCase()}`);
  });
  test(`${tagName}: a click toggles the menu and its rows remain usable`, () => {
    const { api, bar } = panel(tagName);
    bar.fire("pointerdown");
    bar.fire("pointerup");
    bar.fire("click");
    assert.equal(api.wrap.classList.contains("open"), true);
    const row = api.menu.children.at(-1);
    row.fire("pointerdown");
    assert.equal(api.wrap.classList.contains("open"), true);
    row.fire("click");
    assert.equal(api.downloads, 1);
    bar.fire("click");
    assert.equal(api.wrap.classList.contains("open"), false);
  });
}

test("outside press and Escape collapse the menu even with auto-hide disabled", () => {
  const { api, bar, outside, document, timers } = panel();
  api.setTimeout(0);
  bar.fire("click");
  api.wrap.fire("pointerleave");
  assert.equal(timers.size, 0);
  outside.fire("pointerdown");
  assert.equal(api.wrap.classList.contains("open"), false);
  assert.equal(api.wrap.classList.contains("on"), true);
  bar.fire("click");
  document.fire("keydown", { key: "Enter" });
  assert.equal(api.wrap.classList.contains("open"), true);
  api.menu.fire("keydown", { key: "Escape" });
  assert.equal(api.wrap.classList.contains("open"), false);
});

test("leaving starts auto-hide, entering the menu cancels it", () => {
  const { api, bar, timers } = panel();
  bar.fire("click");
  api.wrap.fire("pointerleave");
  assert.equal(timers.size, 1);
  api.menu.fire("pointerover");
  assert.equal(timers.size, 0);
  assert.equal(api.wrap.classList.contains("open"), true);
  api.wrap.fire("pointerleave");
  [...timers.values()][0]();
  assert.equal(api.wrap.classList.contains("open"), false);
  assert.equal(api.wrap.classList.contains("on"), false);
});

test("dragging collapses the menu without downloading or reopening it", () => {
  const { api, bar, timers } = panel();
  bar.fire("click");
  bar.fire("pointerdown");
  api.wrap.fire("pointerleave");
  assert.equal(timers.size, 0);
  bar.fire("pointermove", { clientX: 20 });
  assert.equal(api.wrap.classList.contains("open"), false);
  bar.fire("pointerup", { clientX: 20 });
  bar.fire("click");
  assert.equal(api.wrap.classList.contains("open"), false);
  assert.equal(api.downloads, 0);
  bar.fire("click");
  assert.equal(api.wrap.classList.contains("open"), true);
});

test("refresh preserves a chosen menu, switching players closes it", () => {
  const { api, bar, player } = panel();
  bar.fire("click");
  api.showPanel(player);
  assert.equal(api.wrap.classList.contains("open"), true);
  api.showPanel({ tagName: "AUDIO" });
  assert.equal(api.wrap.classList.contains("open"), false);
});

test("a refresh to a single file closes the menu and click downloads directly", () => {
  const { api, bar, player } = panel();
  bar.fire("click");
  api.setRows([{ label: "clip.mp4" }]);
  api.showPanel(player);
  assert.equal(api.wrap.classList.contains("open"), false);
  bar.fire("pointerover");
  bar.fire("click");
  assert.equal(api.wrap.classList.contains("open"), false);
  assert.equal(api.downloads, 1);
});

test("the close button dismisses this player without toggling the menu", () => {
  const { api, bar, player } = panel();
  bar.fire("click");
  bar.children.at(-1).fire("pointerdown");
  bar.children.at(-1).fire("click");
  assert.equal(api.target, null);
  assert.equal(api.wrap.classList.contains("on"), false);
  api.showPanel(player);
  assert.equal(api.wrap.classList.contains("on"), false);
});
