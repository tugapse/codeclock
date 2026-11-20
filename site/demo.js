// Demo mode for the codeclock dashboard on GitHub Pages: answers the dashboard's /api/* calls in
// the browser with sample data, so the real page runs without the service. Loaded before the
// dashboard's own script by the Pages workflow; nothing here ships in the codeclock binary.
"use strict";
(() => {
  const PROJECTS = [
    ["weather-app", 5], ["pixel-editor", 4], ["home-api", 3], ["blog", 2],
    ["chess-engine", 2], ["recipe-book", 1], ["dotfiles", 1],
  ];
  const MACHINES = ["laptop-linux", "desktop-linux"];
  const EDITOR = "VS Code";

  // A fixed seed: every visitor sees the same history, always ending today.
  let seed = 7;
  const rand = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
  const between = (lo, hi) => lo + Math.floor(rand() * (hi - lo + 1));
  const pick = () => {
    let r = rand() * PROJECTS.reduce((n, [, w]) => n + w, 0);
    for (const [name, w] of PROJECTS) if ((r -= w) < 0) return name;
    return PROJECTS[0][0];
  };
  const nowSec = () => Math.floor(Date.now() / 1000);
  const sec = (d) => Math.floor(d.getTime() / 1000);

  const blocks = [];
  let nextId = 1;
  const add = (machine, project, start, end) =>
    blocks.push({ id: nextId++, uuid: Math.random().toString(16).slice(2), machine, project, editor: EDITOR, start, end });

  const today = new Date();
  const first = new Date(today.getFullYear(), today.getMonth() - 7, 1);
  const startOfToday = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  for (let day = first; day < startOfToday; day = new Date(day.getFullYear(), day.getMonth(), day.getDate() + 1)) {
    const weekend = day.getDay() === 0 || day.getDay() === 6;
    if (rand() > (weekend ? 0.35 : 0.9)) continue;
    const machine = weekend || rand() < 0.45 ? MACHINES[0] : MACHINES[1];
    let t = new Date(day.getFullYear(), day.getMonth(), day.getDate(), weekend ? 14 : 9, between(0, 40));
    const focus = pick();
    for (let n = weekend ? between(1, 3) : between(3, 6); n > 0; n--) {
      const project = rand() < 0.6 ? focus : pick();
      const end = new Date(t.getTime() + between(20, 110) * 60000);
      add(machine, project, sec(t), sec(end));
      t = new Date(end.getTime() + (between(5, 70) + (rand() < 0.25 ? 50 : 0)) * 60000);
    }
  }
  // Today: a couple of finished blocks and one being counted right now.
  const t0 = nowSec();
  add(MACHINES[0], "home-api", t0 - 3 * 3600, t0 - 2 * 3600 - 600);
  add(MACHINES[0], "weather-app", t0 - 2 * 3600, t0 - 3600 - 300);
  add(MACHINES[0], "weather-app", t0 - 47 * 60, t0);
  const liveId = nextId - 1;

  let lastSync = t0 - 240;
  const live = () => {
    const b = blocks.find((x) => x.id === liveId);
    if (b) b.end = nowSec();
    return {
      project: b ? b.project : null,
      block: b ? liveId : null,
      sync: { enabled: true, last: lastSync, last_online: lastSync, error: null },
    };
  };

  const reply = (status, body, delay = 60) =>
    new Promise((ok) => setTimeout(() => ok(new Response(JSON.stringify(body), {
      status, headers: { "Content-Type": "application/json" },
    })), delay));
  const error = (status, msg) => reply(status, { error: msg });

  function handle(method, url, body) {
    const q = url.searchParams;
    const path = url.pathname.replace(/^.*\/api\//, "/api/");
    if (method === "GET" && path === "/api/live") return reply(200, live());
    if (method === "GET" && path === "/api/projects") {
      live();
      const totals = new Map();
      for (const b of blocks) totals.set(b.project, (totals.get(b.project) || 0) + b.end - b.start);
      const projects = [...totals].sort((a, b) => b[1] - a[1]).map(([name, total]) => ({ name, total }));
      return reply(200, {
        projects,
        folders: [...PROJECTS.map(([n]) => n), "notes"],
        machines: [...new Set(blocks.map((b) => b.machine))].sort(),
        first: Math.min(...blocks.map((b) => b.start)),
      });
    }
    if (method === "GET" && path === "/api/blocks") {
      const from = Number(q.get("from")), to = Number(q.get("to"));
      if (!(from < to)) return error(400, "from and to must be numbers with from < to");
      const l = live();
      const list = blocks.filter((b) => b.end > from && b.start < to).sort((a, b) => a.start - b.start);
      return reply(200, { blocks: list.map((b) => ({ ...b })), now: nowSec(), live: l });
    }
    if (method === "POST" && path === "/api/sync") {
      lastSync = nowSec();
      return reply(200, { outcome: { at: lastSync, sent: 0, received: 0, online: true, error: null }, sync: live().sync }, 700);
    }
    const m = /^\/api\/blocks\/(\d+)$/.exec(path);
    if (m && (method === "PATCH" || method === "DELETE")) {
      const id = Number(m[1]);
      const i = blocks.findIndex((b) => b.id === id);
      if (i < 0) return error(404, "no such block");
      if (id === liveId) return error(409, "this block is still being counted; edit it once it ends");
      if (method === "DELETE") { blocks.splice(i, 1); return reply(200, { ok: true }); }
      let e;
      try { e = JSON.parse(body || ""); } catch { return error(400, "expected {project, start, end}"); }
      const project = String(e.project || "").trim();
      if (!project || project.length > 200 || /[/\n]/.test(project)) {
        return error(400, "the project name must be 1 to 200 characters, without / or line breaks");
      }
      if (!(e.start < e.end)) return error(400, "the end must be after the start");
      if (e.end > nowSec() + 60) return error(400, "a block cannot end in the future");
      if (blocks.some((b) => b.id !== id && b.project === project && b.start < e.end && e.start < b.end)) {
        return error(409, "these times overlap another block of this project");
      }
      Object.assign(blocks[i], { project, start: e.start, end: e.end });
      return reply(200, { ok: true });
    }
    return error(404, "not found");
  }

  const realFetch = window.fetch.bind(window);
  window.fetch = (input, opts = {}) => {
    const url = new URL(typeof input === "string" ? input : input.url, location.href);
    if (!/\/api\//.test(url.pathname)) return realFetch(input, opts);
    return handle((opts.method || "GET").toUpperCase(), url, opts.body);
  };

  // Export CSV: the dashboard navigates to the service's CSV; build the same file here instead.
  document.addEventListener("click", (ev) => {
    if (!ev.target.closest || !ev.target.closest("#csv")) return;
    ev.stopImmediatePropagation();
    ev.preventDefault();
    const m = /d=(\d{4}-\d{2}-\d{2})/.exec(location.hash);
    const field = (v) => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
    const pad = (n) => String(n).padStart(2, "0");
    const local = (s) => { const d = new Date(s * 1000); return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`; };
    live();
    // `frame` is the dashboard's own period (a global function); clip to it like the service does.
    const f = typeof window.frame === "function" ? window.frame() : null;
    const from = f ? sec(f.from) : -Infinity, to = f ? sec(f.to) : Infinity;
    const rows = ["id,project,machine,editor,start,end,duration_secs"];
    for (const b of [...blocks].sort((a, c) => a.start - c.start)) {
      if (b.end <= from || b.start >= to) continue;
      const s = Math.max(b.start, from), e = Math.min(b.end, to);
      rows.push([b.id, field(b.project), b.machine, b.editor, local(s), local(e), e - s].join(","));
    }
    const a = Object.assign(document.createElement("a"), {
      href: URL.createObjectURL(new Blob([rows.join("\n") + "\n"], { type: "text/csv" })),
      download: `codeclock-demo${m ? "-" + m[1] : ""}.csv`,
    });
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  }, true);

  // A banner, so nobody mistakes the demo for their own data.
  document.addEventListener("DOMContentLoaded", () => {
    const bar = document.createElement("div");
    bar.setAttribute("role", "note");
    bar.style.cssText = "margin:0 0 14px;padding:10px 14px;border-radius:10px;border:1px solid var(--border);" +
      "background:var(--surface);color:var(--text-2);font-size:13px;line-height:1.45";
    bar.innerHTML = "<b style='color:var(--text)'>Demo with sample data.</b> This is the codeclock dashboard " +
      "running in your browser: edits and deletions stay on this page and reset when you reload. " +
      "<a href='https://github.com/tugapse/codeclock' style='color:var(--accent)'>Get codeclock on GitHub</a>";
    const main = document.querySelector("main");
    main.insertBefore(bar, main.firstChild);
  });
})();
