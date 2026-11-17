"use strict";

const MAX_RENDERED_PARTS = 20, ui = {
 token: "",
 stream: null,
 refreshAgain: !1,
 spacesPort: 0,
 channel: "",
 audit: "",
 tab: "/admin/audit" === window.location.pathname ? "audit" : "chat",
 epoch: 0,
 refreshing: !1,
 transcript: "",
 channels: [],
 connectionError: !1,
 pending: new Set,
 identitySha: null,
 importSha: null,
 importText: "",
 forgetPlan: null,
 discretionSha: null,
 activeSpaceSlug: null,
 activeSpaceUrl: null
}, $ = id => document.getElementById(id);

function node(tag, text, className) {
 const el = document.createElement(tag);
 return void 0 !== text && (el.textContent = text), className && (el.className = className),
 el;
}

function notice(message = "") {
 $("notice").textContent = message;
}

function status(message, ok) {
 $("status-pill").textContent = message, $("status-pill").className = "badge " + (ok ? "permitted" : "failed");
}

async function api(path, options = {}) {
 const headers = new Headers(options.headers);
 ui.token && headers.set("Authorization", `Bearer ${ui.token}`), options.body && !headers.has("Content-Type") && headers.set("Content-Type", "application/json");
 const response = await fetch(path, {
  ...options,
  headers: headers,
  cache: "no-store",
  signal: AbortSignal.timeout(1e4)
 });
 if (!response.ok) {
  let payload = null;
  try {
   payload = await response.json();
  } catch (_) {
   payload = null;
  }
  if (payload && "string" == typeof payload.error && payload.error.trim()) {
   const err = new Error(payload.error);
   throw err.status = response.status, void 0 !== payload.current_sha256 && (err.current_sha256 = payload.current_sha256),
   err;
  }
  const err = new Error({
   401: "Authentication required. Enter the server token and connect.",
   400: "Request rejected. Check inputs or channel state.",
   403: "Action blocked by safety policy or confirmation gate.",
   404: "Requested resource no longer exists. Refresh and try again.",
   409: "Optimistic concurrency or uniqueness conflict."
  }[response.status] || `Server returned HTTP ${response.status}. Try again.`);
  throw err.status = response.status, err;
 }
 return (response.headers.get("content-type") || "").includes("application/json") ? response.json() : response.text();
}

function splitCappedSegments(text) {
 const segments = [];
 let restStart = 0, i = 0;
 for (;i < text.length; ) {
  if ("<" === text[i]) {
   let matchedLen = 0;
   if (text.startsWith("<message_break/>", i) ? matchedLen = 16 : text.startsWith("<message_break />", i) && (matchedLen = 17),
   matchedLen > 0) {
    const seg = text.slice(restStart, i).trim();
    seg && segments.push(seg), i += matchedLen, restStart = i;
    continue;
   }
  }
  i += 1;
 }
 const tail = text.slice(restStart).trim();
 if (tail && segments.push(tail), segments.length > 20) {
  const merged = segments.slice(19).join("\n\n");
  segments.length = 19, segments.push(merged);
 }
 return segments;
}

function tryScanSandboxAttachment(s, start) {
 if (!s.startsWith("![", start)) return null;
 const altStart = start + 2;
 let depth = 1, closeBracket = -1;
 for (let i = altStart; i < s.length; i += 1) {
  const ch = s[i];
  if ("\n" === ch || "\r" === ch) return null;
  if ("[" === ch) depth += 1; else if ("]" === ch && (depth -= 1, 0 === depth)) {
   closeBracket = i;
   break;
  }
 }
 if (closeBracket < 0) return null;
 const afterBracket = closeBracket + 1;
 if (!s.startsWith("(sandbox://", afterBracket)) return null;
 const uriStart = afterBracket + 1;
 let pDepth = 1, closeParen = -1;
 for (let i = uriStart; i < s.length; i += 1) {
  const ch = s[i];
  if ("\n" === ch || "\r" === ch || "\0" === ch) return null;
  if ("(" === ch) pDepth += 1; else if (")" === ch && (pDepth -= 1, 0 === pDepth)) {
   closeParen = i;
   break;
  }
 }
 if (closeParen < 0) return null;
 const uri = s.slice(uriStart, closeParen).trim();
 if ("sandbox://" === uri || /\s/.test(uri)) return null;
 return [ {
  alt: s.slice(altStart, closeBracket).trim(),
  uri: uri
 }, closeParen + 1 ];
}

function tryScanCitation(s, start) {
 if ("【" !== s[start]) return null;
 const innerStart = start + 1;
 let depth = 1;
 for (let i = innerStart; i < s.length; i += 1) {
  const ch = s[i];
  if ("【" === ch) depth += 1; else if ("】" === ch && (depth -= 1, 0 === depth)) {
   const raw = s.slice(innerStart, i).trim();
   return raw ? [ raw, i + 1 ] : null;
  }
 }
 return null;
}

function tryScanOptionsBlock(s, start) {
 if (!s.startsWith("<options>", start)) return null;
 const innerStart = start + 9, closeIdx = s.indexOf("</options>", innerStart);
 if (closeIdx < 0) return null;
 const inner = s.slice(innerStart, closeIdx), endIdx = closeIdx + 10, extracted = [];
 let pos = 0;
 for (;;) {
  const optStart = inner.indexOf("<option>", pos);
  if (optStart < 0) break;
  const itemStart = optStart + 8, optEnd = inner.indexOf("</option>", itemStart);
  if (optEnd < 0) break;
  const label = inner.slice(itemStart, optEnd).trim();
  label && extracted.push(label), pos = optEnd + 9;
 }
 if (!extracted.length) for (const rawLine of inner.split(/\r?\n/)) {
  const trimmed = rawLine.trim();
  if (!trimmed) continue;
  let cleaned = trimmed;
  if (trimmed.startsWith("- ") || trimmed.startsWith("* ")) cleaned = trimmed.slice(2).trim(); else {
   const match = trimmed.match(/^\d+\.\s+(.*)$/);
   match && (cleaned = match[1].trim());
  }
  cleaned && extracted.push(cleaned);
 }
 return [ extracted, endIdx ];
}

function cleanPartText(raw) {
 const lines = [];
 let prevBlank = !0;
 for (const line of raw.split(/\r?\n/)) {
  const trimmedEnd = line.replace(/\s+$/, "");
  trimmedEnd.trim() ? (lines.push(trimmedEnd), prevBlank = !1) : prevBlank || (lines.push(""),
  prevBlank = !0);
 }
 for (;lines.length && !lines[lines.length - 1]; ) lines.pop();
 return lines.join("\n").trim();
}

function scanSegment(segment) {
 let textBuf = "";
 const attachments = [], citations = [], citationIndexMap = new Map, options = [];
 let i = 0;
 for (;i < segment.length; ) {
  const ch = segment[i];
  if ("!" === ch) {
   const scanned = tryScanSandboxAttachment(segment, i);
   if (scanned) {
    attachments.push(scanned[0]), i = scanned[1];
    continue;
   }
  }
  if ("【" === ch) {
   const scanned = tryScanCitation(segment, i);
   if (scanned) {
    const rawCite = scanned[0];
    let idx = citationIndexMap.get(rawCite);
    idx || (idx = citations.length + 1, citationIndexMap.set(rawCite, idx), citations.push({
     index: idx,
     raw: rawCite
    })), textBuf += `[${idx}]`, i = scanned[1];
    continue;
   }
  }
  if ("<" === ch) {
   const scanned = tryScanOptionsBlock(segment, i);
   if (scanned) {
    options.push(...scanned[0]), i = scanned[1];
    continue;
   }
  }
  textBuf += ch, i += 1;
 }
 return {
  text: cleanPartText(textBuf),
  attachments: attachments,
  citations: citations,
  options: options
 };
}

function parseDirectives(text) {
 return text && text.trim() ? splitCappedSegments(text).map(scanSegment).filter(p => p.text || p.attachments.length || p.citations.length || p.options.length) : [];
}

function setTab(tabName) {
 ui.tab = tabName, document.querySelectorAll("[data-tab]").forEach(btn => {
  btn.setAttribute("aria-selected", String(btn.dataset.tab === tabName));
 }), document.querySelectorAll(".tab-panel").forEach(panel => {
  panel.hidden = panel.id !== `panel-${tabName}`;
 });
 const grid = $("workspace-grid"), auditPanel = $("audit-panel");
 "audit" === tabName ? (grid.classList.add("audit-focus"), auditPanel.hidden = !1) : "chat" === tabName ? (grid.classList.remove("audit-focus"),
 auditPanel.hidden = !1) : (grid.classList.add("audit-focus"), auditPanel.hidden = !0),
 refreshTabData(tabName);
}

function fillBots(id, bots, includeNone = !1) {
 const select = $(id), values = new Set([ ...select.selectedOptions ].map(o => o.value)), options = bots.map(b => {
  const option = node("option", `@${b.id}`);
  return option.value = b.id, option.selected = values.has(b.id), option;
 });
 if (includeNone) {
  const none = node("option", "No default responder");
  none.value = "", none.selected = values.has(""), options.unshift(none);
 }
 select.replaceChildren(...options);
}

function renderBots(bots) {
 const view = JSON.stringify(bots);
 if ($("bots-list").dataset.view === view) return;
 $("bots-list").dataset.view = view, $("bots-list").replaceChildren(...bots.map(b => {
  const item = node("div", void 0, "item");
  return item.append(node("strong", `${b.avatar_emoji} @${b.id} · ${b.display_name}`), node("div", b.role_title, "muted")),
  item;
 }));
 const signature = JSON.stringify(bots.map(b => b.id));
 $("task-bot").dataset.bots !== signature && (fillBots("task-bot", bots), fillBots("channel-owner", bots, !0),
 fillBots("channel-members", bots), fillBots("identity-bot", bots), $("task-bot").dataset.bots = signature);
}

function renderChannels(channels) {
 ui.channels = channels, channels.some(c => c.id === ui.channel) || (ui.channel = channels[0]?.id || "");
 const view = JSON.stringify([ channels, ui.channel ]);
 if ($("channels-list").dataset.view === view) return;
 $("channels-list").dataset.view = view, $("channels-list").replaceChildren(...channels.map(c => {
  const button = node("button", `${c.name}${c.paused ? " · paused" : ""}`, "channel-button");
  return button.type = "button", button.dataset.channel = c.id, button.setAttribute("aria-pressed", String(c.id === ui.channel)),
  button;
 }));
 const current = channels.find(c => c.id === ui.channel);
 $("channel-heading").textContent = current?.name || "No channels", $("channel-description").textContent = current ? `${current.topic} · Default responder: ${current.owner_bot_id ? "@" + current.owner_bot_id : "none"}${current.paused ? " · Paused: messages are saved without starting work." : ""}` : "Create a channel to start a conversation.",
 $("message-form").querySelector("button").disabled = !current;
}

function renderMessages(messages, channel) {
 const signature = JSON.stringify([ channel, messages ]);
 if (signature === ui.transcript) return;
 const list = $("messages-list"), follow = list.scrollHeight - list.scrollTop - list.clientHeight < 50 || channel !== list.dataset.channel;
 list.replaceChildren(...messages.map(m => {
  const item = node("div", void 0, "item");
  item.append(node("strong", m.bot_id ? `@${m.bot_id}` : m.author));
  const time = node("time", ` · ${new Date(m.timestamp).toLocaleString()}`, "muted");
  time.dateTime = m.timestamp, item.append(time);
  const parts = parseDirectives(m.content || "");
  if (parts.length) for (const part of parts) {
   const bubble = node("div", void 0, parts.length > 1 ? "bubble" : "");
   if (part.text && bubble.append(node("div", part.text, "content")), part.attachments.length) {
    const attRow = node("div", void 0, "chips");
    for (const att of part.attachments) {
     const label = att.alt ? `${att.alt} (${att.uri})` : att.uri;
     attRow.append(node("span", `📎 ${label}`, "chip"));
    }
    bubble.append(attRow);
   }
   if (part.options.length) {
    const optRow = node("div", void 0, "chips");
    for (const label of part.options) {
     const btn = node("button", label, "secondary");
     btn.type = "button", btn.dataset.optionLabel = label, optRow.append(btn);
    }
    bubble.append(optRow);
   }
   if (part.citations.length) {
    const det = node("details");
    det.append(node("summary", `Sources (${part.citations.length})`));
    for (const cite of part.citations) det.append(node("div", `[${cite.index}] ${cite.raw}`, "muted"));
    bubble.append(det);
   }
   item.append(bubble);
  } else item.append(node("div", m.content || "", "content"));
  const rxnRow = node("div", void 0, "chips"), rxnList = Array.isArray(m.reactions) ? m.reactions : [];
  for (const r of rxnList) {
   const btn = node("button", `${r.emoji} ${r.count}`, "secondary");
   btn.type = "button", btn.dataset.reactMessage = m.id, btn.dataset.reactEmoji = r.emoji,
   rxnRow.append(btn);
  }
  for (const quick of [ "👍", "❤️", "🔥" ]) if (!rxnList.some(r => r.emoji === quick)) {
   const qbtn = node("button", quick, "secondary");
   qbtn.type = "button", qbtn.dataset.reactMessage = m.id, qbtn.dataset.reactEmoji = quick,
   rxnRow.append(qbtn);
  }
  return item.append(rxnRow), item;
 })), messages.length || list.append(node("p", "No messages yet. Send the first one below.", "muted")),
 list.dataset.channel = channel, ui.transcript = signature, follow && (list.scrollTop = list.scrollHeight);
}

function actionButton(id, action, label) {
 const button = node("button", label, "secondary");
 return button.type = "button", button.dataset.task = id, button.dataset.action = action,
 button;
}

function renderQueue(tasks) {
 const view = JSON.stringify(tasks);
 $("queue-list").dataset.view !== view && ($("queue-list").dataset.view = view, $("queue-list").replaceChildren(...tasks.map(t => {
  const item = node("div", void 0, "item");
  item.append(node("strong", `@${t.bot_id} · ${t.status.replaceAll("_", " ").toUpperCase()}`), node("div", t.instruction, "content")),
  t.result_summary && item.append(node("div", t.result_summary, "content"));
  const actions = node("div", void 0, "row");
  return ("waiting_for_human" === t.status || t.mutating_in_flight && [ "failed", "cancelled" ].includes(t.status)) && (item.append(node("div", `Take the Wheel: ${t.blocker_note || "Human intervention needed"}`, "content")),
  actions.append(actionButton(t.id, "wheel-resume", "Hand Back to Bot"))), [ "queued", "running" ].includes(t.status) && actions.append(actionButton(t.id, "pause", "Pause")),
  "paused" === t.status && actions.append(actionButton(t.id, "wheel-resume", "Review / Resume")),
  [ "failed", "cancelled" ].includes(t.status) && !t.mutating_in_flight && actions.append(actionButton(t.id, "retry", "Retry")),
  [ "queued", "running", "pause_requested", "paused", "waiting_for_human" ].includes(t.status) && actions.append(actionButton(t.id, "cancel", "Cancel")),
  item.append(actions), item;
 })), tasks.length || $("queue-list").append(node("p", "No queued tasks.", "muted")));
}

function renderAudit(records) {
 const view = JSON.stringify(records);
 $("audit-list").dataset.view !== view && ($("audit-list").dataset.view = view, $("audit-list").replaceChildren(...records.map(r => {
  const item = node("div", void 0, "item"), category = [ "permitted", "refused", "failed" ].includes(r.category) ? r.category : "";
  return item.append(node("span", r.category.toUpperCase(), `badge ${category}`), node("code", ` ${r.rule_id}`), node("div", r.action_summary, "content"), node("div", r.citation, "muted")),
  item;
 })), records.length || $("audit-list").append(node("p", "No matching audit entries.", "muted")));
}

function renderFeed(data) {
 const posts = Array.isArray(data?.posts) ? data.posts : [], promptInput = $("feed-brief-prompt");
 document.activeElement !== promptInput && null != data?.brief_prompt && (promptInput.value = data.brief_prompt || "");
 const view = JSON.stringify(posts);
 $("feed-list").dataset.view !== view && ($("feed-list").dataset.view = view, $("feed-list").replaceChildren(...posts.map(p => {
  const item = node("div", void 0, "item"), head = node("div", void 0, "row");
  head.append(node("span", p.kicker || "Update", "badge"), node("strong", p.title)),
  item.append(head, node("div", p.body, "content")), Array.isArray(p.signal_citations) && p.signal_citations.length && item.append(node("div", `Citations: ${p.signal_citations.join(" · ")}`, "muted"));
  const actions = node("div", void 0, "row");
  actions.append(node("span", null != p.rating ? `Rating: ${p.rating}` : "Rate:", "muted"));
  for (const [val, label] of [ [ 5, "Helpful (5)" ], [ 1, "👍 +1" ], [ -1, "👎 -1" ] ]) {
   const btn = node("button", label, "secondary");
   btn.type = "button", btn.dataset.ratePost = p.id, btn.dataset.rateVal = String(val),
   actions.append(btn);
  }
  return item.append(actions), item;
 })), posts.length || $("feed-list").append(node("p", "No feed posts yet.", "muted")));
}

function renderIdeas(ideas) {
 const list = Array.isArray(ideas) ? ideas : [], view = JSON.stringify(list);
 $("ideas-list").dataset.view !== view && ($("ideas-list").dataset.view = view, $("ideas-list").replaceChildren(...list.map(idea => {
  const item = node("div", void 0, "item");
  item.append(node("strong", idea.title), node("div", `${idea.source || "hybrid"} · confidence ${(100 * Number(idea.confidence || 0)).toFixed(0)}%`, "muted"), node("div", idea.description || "", "content"));
  const actions = node("div", void 0, "row"), runBtn = node("button", "Run", void 0);
  runBtn.type = "button", runBtn.dataset.ideaRun = idea.id;
  const dismissBtn = node("button", "Dismiss once", "secondary");
  return dismissBtn.type = "button", dismissBtn.dataset.ideaDismiss = idea.id, actions.append(runBtn, dismissBtn),
  item.append(actions), item;
 })), list.length || $("ideas-list").append(node("p", "No proactive ideas available right now.", "muted")));
}

function renderGoals(goals) {
 const items = Array.isArray(goals) ? goals : [], topLevel = items.filter(g => !g.parent_goal_id), subParentSel = $("goal-parent"), prevParent = subParentSel.value, pOpts = [ node("option", "(Top-level Goal)") ];
 pOpts[0].value = "";
 for (const g of topLevel) {
  const opt = node("option", `${g.title} (${g.slug})`);
  opt.value = g.slug, pOpts.push(opt);
 }
 subParentSel.replaceChildren(...pOpts), subParentSel.value = prevParent;
 const view = JSON.stringify(items);
 if ($("goals-list").dataset.view === view) return;
 $("goals-list").dataset.view = view;
 const byParent = new Map;
 for (const g of items) if (g.parent_goal_id) {
  const arr = byParent.get(g.parent_goal_id) || [];
  arr.push(g), byParent.set(g.parent_goal_id, arr);
 }
 function renderGoalCard(g, isSubgoal) {
  const card = node("div", void 0, isSubgoal ? "bubble" : "item"), row = node("div", void 0, "row");
  row.append(node("strong", `${isSubgoal ? "↳ " : ""}${g.title}`), node("span", g.status || "active", "badge"), node("code", g.slug)),
  card.append(row), g.description && card.append(node("div", g.description, "content")),
  g.pause_notes && card.append(node("div", `Note: ${g.pause_notes}`, "muted")), Array.isArray(g.stop_conditions) && g.stop_conditions.length && card.append(node("div", `Stop conditions: ${g.stop_conditions.join(", ")}`, "muted"));
  const files = Array.isArray(g.files) ? g.files : [];
  if (files.length) {
   const fileRow = node("div", void 0, "chips");
   for (const rel of files) {
    const fbtn = node("button", `📄 ${rel}`, "secondary");
    fbtn.type = "button", fbtn.dataset.goalSlug = g.slug, fbtn.dataset.goalFile = rel,
    fileRow.append(fbtn);
   }
   card.append(fileRow);
  }
  const actions = node("div", void 0, "row");
  for (const [act, label] of [ [ "complete", "Complete" ], [ "abandon", "Abandon" ], [ "delete", "Delete" ] ]) {
   const btn = node("button", label, "delete" === act ? "danger" : "secondary");
   btn.type = "button", btn.dataset.goalAction = act, btn.dataset.goalSlug = g.slug,
   actions.append(btn);
  }
  if (card.append(actions), !isSubgoal) {
   const children = byParent.get(g.id) || [];
   for (const sub of children) card.append(renderGoalCard(sub, !0));
  }
  return card;
 }
 $("goals-list").replaceChildren(...topLevel.map(g => renderGoalCard(g, !1))), items.length || $("goals-list").append(node("p", "No goals created yet.", "muted"));
}

function renderSpaces(spaces) {
 const list = Array.isArray(spaces) ? spaces : [], view = JSON.stringify(list);
 $("spaces-list").dataset.view !== view && ($("spaces-list").dataset.view = view,
 $("spaces-list").replaceChildren(...list.map(sp => {
  const item = node("div", void 0, "item"), row = node("div", void 0, "row");
  row.append(node("strong", sp.title || sp.slug), node("code", sp.slug), node("span", sp.published ? "PUBLISHED" : "LOCAL ONLY", sp.published ? "badge permitted" : "badge")),
  item.append(row);
  const openBtn = node("button", "Open Live Space", void 0);
  openBtn.type = "button", openBtn.dataset.spaceOpen = sp.slug;
  const pubBtn = node("button", "Publish…", "secondary");
  pubBtn.type = "button", pubBtn.dataset.spacePub = sp.slug;
  const btns = node("div", void 0, "row");
  return btns.append(openBtn, pubBtn), item.append(btns), item;
 })), list.length || $("spaces-list").append(node("p", "No static Spaces created yet.", "muted")));
}

async function openSpace(slug) {
 notice();
 try {
  const res = await api(`/api/spaces/${encodeURIComponent(slug)}/url`), url = new URL(res.url);
  if (url.origin !== `http://127.0.0.1:${ui.spacesPort}` || url.pathname !== `/spaces/v2/${encodeURIComponent(slug)}/` || !url.searchParams.get("token") || url.username || url.password) throw new Error("Rejected Space URL outside the isolated Space server");
  ui.activeSpaceSlug = slug, ui.activeSpaceUrl = res.url, $("space-stage").hidden = !1,
  $("space-publish-confirm").hidden = !0, $("space-active-label").textContent = `Space: ${slug}`,
  $("space-active-origin").textContent = `Served on isolated origin: ${new URL(res.url).origin} (port ${res.spaces_port})`,
  $("space-iframe").src = res.url;
 } catch (err) {
  notice(err.message);
 }
}

function postSpaceTheme(theme) {
 if (!ui.activeSpaceUrl) return;
 const iframe = $("space-iframe");
 iframe.contentWindow && iframe.contentWindow.postMessage({
  type: "hatch:space:theme",
  theme: theme
 }, "*");
}

function renderSkillsAndConnectors(skills, connectors) {
 const sList = Array.isArray(skills) ? skills : [];
 $("skills-list").replaceChildren(...sList.map(s => {
  const item = node("div", void 0, "item"), row = node("div", void 0, "row");
  row.append(node("strong", s.title || s.name), node("code", s.name), node("span", s.category || "skill", "badge"));
  const btn = node("button", "Inspect SKILL.md", "secondary");
  return btn.type = "button", btn.dataset.loadSkill = s.name, row.append(btn), item.append(row, node("div", s.description || "", "muted")),
  item;
 })), sList.length || $("skills-list").append(node("p", "No matching skills.", "muted"));
 const cList = Array.isArray(connectors) ? connectors : [];
 $("connectors-list").replaceChildren(...cList.map(c => {
  const item = node("div", void 0, "item"), row = node("div", void 0, "row");
  row.append(node("strong", c.title || c.id), node("code", c.id), node("span", c.require_auth ? "auth required" : "no auth", "badge"));
  const btn = node("button", "Inspect Connector", "secondary");
  return btn.type = "button", btn.dataset.loadConnector = c.id, row.append(btn), item.append(row, node("div", c.description || "", "muted")),
  item;
 })), cList.length || $("connectors-list").append(node("p", "No matching connectors.", "muted"));
}

async function refreshTabData(tabName = ui.tab) {
 try {
  if ("feed" === tabName) renderFeed(await api("/api/activity")); else if ("ideas" === tabName) renderIdeas(await api("/api/ideas")); else if ("goals" === tabName) renderGoals(await api("/api/goals")); else if ("spaces" === tabName) renderSpaces(await api("/api/spaces")); else if ("skills" === tabName) {
   const q = $("skill-search-q").value.trim(), qs = q ? `?q=${encodeURIComponent(q)}` : "", [skills, connectors] = await Promise.all([ api(`/api/skills${qs}`), api(`/api/connectors${qs}`) ]);
   renderSkillsAndConnectors(skills, connectors);
  } else if ("identity" === tabName) {
   const prefs = await api("/api/proactive-prefs");
   $("prefs-summary").textContent = JSON.stringify(prefs, null, 2);
  }
 } catch (err) {
  401 !== err.status && notice(err.message);
 }
}

async function refreshAll() {
 if (ui.refreshing) return void (ui.refreshAgain = !0);
 ui.refreshing = !0;
 const epoch = ui.epoch, audit = ui.audit, channel = ui.channel;
 try {
  const snapshot = await api("/api/bootstrap");
  if (epoch !== ui.epoch) return;
  if (ui.spacesPort = snapshot.status?.spaces_port || 0, renderBots(snapshot.bots || []),
  renderChannels(snapshot.channels || []), renderQueue(snapshot.recent_tasks || []),
  "audit" === ui.tab) {
   const records = await api("/api/audit" + (audit ? "?category=" + encodeURIComponent(audit) : ""));
   epoch === ui.epoch && audit === ui.audit && renderAudit(records);
  }
  renderFeed(snapshot.feed || {}), renderIdeas(snapshot.feed?.ideas || []), renderGoals((snapshot.goals || []).map(g => g.goal || g)),
  renderSpaces((snapshot.spaces || []).map(s => s.meta || s));
  const selected = ui.channel;
  if (selected) {
   const messages = await api(`/api/channels/${encodeURIComponent(selected)}/messages`);
   epoch === ui.epoch && selected === ui.channel && renderMessages(messages, selected);
  } else renderMessages([], "");
  epoch === ui.epoch && (status(snapshot.status?.paused ? "Connected · paused" : "Connected", !0),
  $("runtime-status").textContent = `Grok-Muse ${snapshot.version || ""} · Host desktop: ${snapshot.status?.allow_host_desktop ? "enabled" : "disabled"} · Spaces port: ${ui.spacesPort}`,
  ui.connectionError && notice(), ui.connectionError = !1);
 } catch (error) {
  epoch === ui.epoch && (ui.connectionError = !0, status("Connection unavailable", !1),
  notice(error.message));
 } finally {
  ui.refreshing = !1, (ui.refreshAgain || epoch !== ui.epoch || audit !== ui.audit || channel !== ui.channel) && (ui.refreshAgain = !1,
  refreshAll());
 }
}

async function mutate(button, path, body, onSuccess, method = "POST") {
 const key = `${method}:${path}`;
 if (!ui.pending.has(key)) {
  ui.pending.add(key), button && (button.disabled = !0), ui.connectionError = !1,
  notice();
  try {
   const result = await api(path, {
    method: method,
    body: void 0 === body ? void 0 : JSON.stringify(body)
   });
   if (result && !1 === result.ok) throw new Error("The operation did not complete. Refresh and try again.");
   onSuccess?.(result), ui.epoch++, refreshAll();
  } catch (error) {
   notice(error.message);
  } finally {
   ui.pending.delete(key), button && (button.disabled = !1);
  }
 }
}

function parseSseFrames(buffer, dispatch) {
 let match;
 for (;match = /\r?\n\r?\n/.exec(buffer); ) {
  const frame = buffer.slice(0, match.index);
  buffer = buffer.slice(match.index + match[0].length);
  let type = "message";
  const data = [];
  for (const line of frame.split(/\r?\n/)) {
   const colon = line.indexOf(":"), field = colon < 0 ? line : line.slice(0, colon), value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
   "event" === field && (type = value), "data" === field && data.push(value);
  }
  data.length && dispatch(type, data.join("\n"));
 }
 if (buffer.length > 1048576) throw new Error("SSE frame exceeds limit");
 return buffer;
}

async function connectEvents() {
 ui.stream?.abort();
 const controller = new AbortController;
 ui.stream = controller;
 const signal = controller.signal;
 let attempt = 0;
 for (await refreshAll(); !signal.aborted; ) {
  let reader, received = !1;
  try {
   const response = await fetch("/ag-ui/events", {
    headers: {
     Authorization: "Bearer " + ui.token
    },
    signal: signal,
    cache: "no-store"
   });
   if (401 === response.status) return void status("Authentication required", !1);
   if (!response.ok || !response.body) throw new Error("Event stream unavailable");
   reader = response.body.getReader();
   const decoder = new TextDecoder;
   let buffer = "";
   for (attempt && await refreshAll(); !signal.aborted; ) {
    const {value: value, done: done} = await reader.read();
    if (done) throw new Error("Event stream disconnected");
    buffer += decoder.decode(value, {
     stream: !0
    }), buffer = parseSseFrames(buffer, () => {
     received = !0, document.hidden ? ui.refreshAgain = !0 : refreshAll();
    });
   }
  } catch (error) {
   if (signal.aborted) return;
   status("Reconnecting event stream…", !1);
  } finally {
   reader && (await reader.cancel().catch(() => {}), reader.releaseLock());
  }
  received && (attempt = 0);
  const delay = [ 1e3, 2e3, 5e3, 15e3 ][Math.min(attempt++, 3)] * (.8 + .4 * Math.random());
  await new Promise(resolve => {
   const done = () => {
    clearTimeout(timer), signal.removeEventListener("abort", done), resolve();
   }, timer = setTimeout(done, delay);
   signal.addEventListener("abort", done, {
    once: !0
   }), signal.aborted && done();
  });
 }
}

async function loadIdentityFile() {
 notice(), $("identity-conflict").hidden = !0;
 const botId = $("identity-bot").value || "muse", file = $("identity-file").value;
 try {
  const res = await api(`/api/identity/${encodeURIComponent(botId)}/${encodeURIComponent(file)}`);
  ui.identitySha = res.sha256 || null, $("identity-editor").value = res.content || "",
  $("identity-sha").textContent = res.sha256 ? `Loaded SHA-256: ${res.sha256}` : "File does not exist yet (will create on save).";
 } catch (err) {
  notice(err.message);
 }
}

async function loadDiscretion() {
 notice();
 const chatId = $("discretion-chat-id").value.trim();
 if (chatId) try {
  const pol = await api(`/api/side-chats/${encodeURIComponent(chatId)}/discretion`);
  ui.discretionSha = pol.sha256 || null, $("discretion-sha").textContent = `Loaded SHA-256: ${pol.sha256 || "new"}`,
  $("discretion-defaults").textContent = (pol.default_rules || []).join("\n"), $("discretion-rules").value = (pol.user_rules || []).join("\n");
 } catch (err) {
  notice(err.message);
 }
}

async function inspectCatalog(kind, key) {
 notice();
 try {
  const res = await api(`/api/${kind}?load=${encodeURIComponent(key)}`);
  $("skill-detail-box").hidden = !1, $("skill-detail-title").textContent = `${kind}: ${key}`,
  $("skill-detail-body").textContent = JSON.stringify(res, null, 2);
 } catch (err) {
  notice(err.message);
 }
}

document.querySelectorAll("[data-tab]").forEach(btn => {
 btn.addEventListener("click", () => setTab(btn.dataset.tab));
}), $("connection-form").addEventListener("submit", event => {
 event.preventDefault(), ui.token = $("api-token").value.trim(), $("api-token").value = "",
 ui.epoch++, notice(), connectEvents();
}), $("channels-list").addEventListener("click", event => {
 const button = event.target.closest("[data-channel]");
 button && (ui.channel = button.dataset.channel, ui.epoch++, notice(), renderChannels(ui.channels),
 $("messages-list").replaceChildren(node("p", "Loading messages…", "muted")), ui.transcript = "",
 refreshAll());
}), $("messages-list").addEventListener("click", event => {
 const optBtn = event.target.closest("[data-option-label]");
 if (optBtn && ui.channel) return void mutate(optBtn, `/api/channels/${encodeURIComponent(ui.channel)}/messages`, {
  content: optBtn.dataset.optionLabel
 });
 const rxnBtn = event.target.closest("[data-react-message]");
 rxnBtn && mutate(rxnBtn, `/api/messages/${encodeURIComponent(rxnBtn.dataset.reactMessage)}/reactions`, {
  emoji: rxnBtn.dataset.reactEmoji
 });
}), $("message-form").addEventListener("submit", event => {
 event.preventDefault();
 const input = $("message-input"), content = input.value;
 content.trim() && ui.channel && mutate(event.submitter || event.target.querySelector("button"), `/api/channels/${encodeURIComponent(ui.channel)}/messages`, {
  content: content
 }, () => {
  input.value === content && (input.value = "");
 });
}), $("task-form").addEventListener("submit", event => {
 event.preventDefault();
 const input = $("task-input"), instruction = input.value;
 instruction.trim() && mutate(event.submitter || event.target.querySelector("button"), "/api/queue", {
  bot_id: $("task-bot").value,
  instruction: instruction,
  priority: 5
 }, () => {
  input.value === instruction && (input.value = "");
 });
}), $("channel-create-form").addEventListener("submit", event => {
 event.preventDefault();
 const form = event.target, body = {
  id: $("channel-id").value.trim(),
  topic: $("channel-topic").value,
  owner_bot_id: $("channel-owner").value || null,
  participants: [ ...$("channel-members").selectedOptions ].map(o => o.value)
 };
 mutate(event.submitter || form.querySelector("button"), "/api/channels", body, channel => {
  ui.channel = channel.id, form.reset();
 });
}), $("queue-list").addEventListener("click", event => {
 const button = event.target.closest("[data-task]");
 if (!button) return;
 let body;
 if ("wheel-resume" === button.dataset.action) {
  const note = prompt("Optional note about what you completed:", "Completed login/2FA step");
  if (null === note) return;
  body = {
   note: note
  };
 }
 mutate(button, `/api/tasks/${encodeURIComponent(button.dataset.task)}/${button.dataset.action}`, body);
}), document.querySelectorAll("[data-audit]").forEach(button => {
 button.addEventListener("click", () => {
  ui.audit = button.dataset.audit, ui.epoch++, notice(), refreshAll();
 });
}), $("feed-prompt-form").addEventListener("submit", event => {
 event.preventDefault(), mutate(event.submitter, "/api/activity/prompt", {
  prompt: $("feed-brief-prompt").value
 }, () => {
  refreshTabData("feed");
 });
}), $("feed-create-form").addEventListener("submit", event => {
 event.preventDefault();
 const body = {
  kicker: $("feed-post-kicker").value.trim() || "Editorial",
  title: $("feed-post-title").value,
  body: $("feed-post-body").value
 };
 mutate(event.submitter, "/api/activity", body, () => {
  event.target.reset(), refreshTabData("feed");
 });
}), $("feed-list").addEventListener("click", event => {
 const btn = event.target.closest("[data-rate-post]");
 btn && mutate(btn, `/api/activity/${encodeURIComponent(btn.dataset.ratePost)}/rate`, {
  rating: Number(btn.dataset.rateVal)
 }, () => {
  refreshTabData("feed");
 });
}), $("ideas-refresh-btn").addEventListener("click", () => {
 refreshTabData("ideas");
}), $("ideas-create-form").addEventListener("submit", event => {
 event.preventDefault();
 const body = {
  title: $("idea-title").value.trim(),
  description: $("idea-desc").value.trim() || void 0,
  action_prompt: $("idea-prompt").value.trim() || void 0
 };
 mutate(event.submitter, "/api/ideas", body, () => {
  event.target.reset(), refreshTabData("ideas");
 });
}), $("ideas-list").addEventListener("click", event => {
 const runBtn = event.target.closest("[data-idea-run]");
 if (runBtn) return void mutate(runBtn, `/api/ideas/${encodeURIComponent(runBtn.dataset.ideaRun)}/run`, {
  bot_id: $("task-bot").value || "muse"
 });
 const dismissBtn = event.target.closest("[data-idea-dismiss]");
 dismissBtn && mutate(dismissBtn, `/api/ideas/${encodeURIComponent(dismissBtn.dataset.ideaDismiss)}/dismiss`, {}, () => {
  refreshTabData("ideas");
 });
}), $("goal-create-form").addEventListener("submit", event => {
 event.preventDefault();
 const stopRaw = $("goal-stops").value.trim(), stopConditions = stopRaw ? stopRaw.split(",").map(s => s.trim()).filter(Boolean) : [], body = {
  parent_slug: $("goal-parent").value || void 0,
  title: $("goal-title").value,
  description: $("goal-desc").value,
  stop_conditions: stopConditions
 };
 mutate(event.submitter, "/api/goals", body, () => {
  event.target.reset(), refreshTabData("goals");
 });
}), $("goals-list").addEventListener("click", async event => {
 const actionBtn = event.target.closest("[data-goal-action]");
 if (actionBtn) return void mutate(actionBtn, "/api/goals", {
  slug: actionBtn.dataset.goalSlug,
  action: actionBtn.dataset.goalAction,
  verified: !0
 }, () => {
  refreshTabData("goals");
 }, "PATCH");
 const fileBtn = event.target.closest("[data-goal-file]");
 if (fileBtn) {
  notice();
  try {
   const slug = encodeURIComponent(fileBtn.dataset.goalSlug), rel = fileBtn.dataset.goalFile.split("/").map(encodeURIComponent).join("/"), text = await api(`/api/goals/${slug}/files/${rel}`);
   $("goal-file-viewer").hidden = !1, $("goal-file-title").textContent = `${fileBtn.dataset.goalSlug}/${fileBtn.dataset.goalFile}`,
   $("goal-file-content").textContent = "string" == typeof text ? text : JSON.stringify(text, null, 2);
  } catch (err) {
   notice(err.message);
  }
 }
}), $("space-create-form").addEventListener("submit", event => {
 event.preventDefault();
 const html = $("space-html").value.trim(), body = {
  slug: $("space-slug").value.trim(),
  title: $("space-title").value.trim() || void 0,
  kind: "static",
  html: html || void 0
 };
 mutate(event.submitter, "/api/spaces", body, created => {
  event.target.reset(), refreshTabData("spaces"), created?.slug && openSpace(created.slug);
 });
}), $("spaces-list").addEventListener("click", event => {
 const openBtn = event.target.closest("[data-space-open]");
 if (openBtn) return void openSpace(openBtn.dataset.spaceOpen);
 const pubBtn = event.target.closest("[data-space-pub]");
 pubBtn && (ui.activeSpaceSlug = pubBtn.dataset.spacePub, $("space-stage").hidden = !1,
 $("space-publish-confirm").hidden = !1, $("space-active-label").textContent = `Publish Confirmation: ${ui.activeSpaceSlug}`);
}), $("space-theme-light").addEventListener("click", () => postSpaceTheme("light")),
$("space-theme-dark").addEventListener("click", () => postSpaceTheme("dark")), $("space-publish-btn").addEventListener("click", () => {
 ui.activeSpaceSlug && ($("space-publish-confirm").hidden = !1);
}), $("space-publish-confirm-cancel").addEventListener("click", () => {
 $("space-publish-confirm").hidden = !0;
}), $("space-publish-confirm-yes").addEventListener("click", () => {
 ui.activeSpaceSlug && mutate($("space-publish-confirm-yes"), `/api/spaces/${encodeURIComponent(ui.activeSpaceSlug)}/publish`, {
  confirmed: !0
 }, () => {
  $("space-publish-confirm").hidden = !0, refreshTabData("spaces");
 });
}), $("identity-load-btn").addEventListener("click", () => {
 loadIdentityFile();
}), $("identity-save-btn").addEventListener("click", async () => {
 notice(), $("identity-conflict").hidden = !0;
 const botId = $("identity-bot").value || "muse", file = $("identity-file").value, btn = $("identity-save-btn");
 btn.disabled = !0;
 try {
  const res = await api(`/api/identity/${encodeURIComponent(botId)}/${encodeURIComponent(file)}`, {
   method: "PUT",
   body: JSON.stringify({
    content: $("identity-editor").value,
    expected_sha256: ui.identitySha
   })
  });
  ui.identitySha = res.sha256, $("identity-sha").textContent = `Saved SHA-256: ${res.sha256}`;
 } catch (err) {
  409 === err.status && ($("identity-conflict").hidden = !1, $("identity-conflict").textContent = `409 Conflict: file changed on disk (current SHA-256: ${err.current_sha256 || "unknown"}). Click Load File to inspect latest before saving.`),
  notice(err.message);
 } finally {
  btn.disabled = !1;
 }
}), $("prefs-form").addEventListener("submit", event => {
 event.preventDefault();
 const qStart = $("prefs-quiet-start").value, qEnd = $("prefs-quiet-end").value, body = {
  add_tell: $("prefs-tell").value.trim() || void 0,
  add_opt_out: $("prefs-opt-out").value.trim() || void 0,
  quiet_start_hour: "" !== qStart ? Number(qStart) : void 0,
  quiet_end_hour: "" !== qEnd ? Number(qEnd) : void 0,
  home_tz: $("prefs-home-tz").value.trim() || void 0,
  work_tz: $("prefs-work-tz").value.trim() || void 0,
  feedback_note: $("prefs-feedback-note").value.trim() || void 0
 };
 mutate(event.submitter, "/api/proactive-prefs", body, updated => {
  $("prefs-summary").textContent = JSON.stringify(updated, null, 2), $("prefs-tell").value = "",
  $("prefs-opt-out").value = "", $("prefs-feedback-note").value = "";
 }, "PUT");
}), $("import-preview-btn").addEventListener("click", async () => {
 notice();
 const input = $("import-input").value;
 if (input.trim()) try {
  const rep = await api("/api/memory/import", {
   method: "POST",
   body: JSON.stringify({
    input: input,
    apply: !1,
    bot_id: $("identity-bot").value || "muse"
   })
  });
  ui.importSha = rep.report_sha256 || null, ui.importText = input, $("import-apply-btn").disabled = !ui.importSha,
  $("import-report").textContent = JSON.stringify(rep, null, 2);
 } catch (err) {
  notice(err.message);
 }
}), $("import-apply-btn").addEventListener("click", async () => {
 if (notice(), ui.importSha) try {
  const rep = await api("/api/memory/import", {
   method: "POST",
   body: JSON.stringify({
    input: ui.importText,
    apply: !0,
    report_sha256: ui.importSha,
    bot_id: $("identity-bot").value || "muse"
   })
  });
  ui.importSha = null, $("import-apply-btn").disabled = !0, $("import-report").textContent = JSON.stringify(rep, null, 2);
 } catch (err) {
  notice(err.message);
 }
}), $("explain-form").addEventListener("submit", async event => {
 event.preventDefault(), notice();
 try {
  const res = await api("/api/memory/explain", {
   method: "POST",
   body: JSON.stringify({
    target: $("explain-target").value.trim()
   })
  });
  $("explain-output").textContent = JSON.stringify(res, null, 2);
 } catch (err) {
  notice(err.message);
 }
}), $("forget-plan-form").addEventListener("submit", async event => {
 event.preventDefault(), notice();
 try {
  const plan = await api("/api/memory/forget/plan", {
   method: "POST",
   body: JSON.stringify({
    request: $("forget-request").value,
    bot_id: $("identity-bot").value || "muse"
   })
  });
  ui.forgetPlan = plan, $("forget-plan-output").textContent = JSON.stringify(plan, null, 2),
  $("forget-confirm-box").hidden = !1, $("forget-confirm-check").checked = !1, $("forget-execute-btn").disabled = !0,
  $("forget-report-output").textContent = "";
 } catch (err) {
  notice(err.message);
 }
}), $("forget-confirm-check").addEventListener("change", () => {
 $("forget-execute-btn").disabled = !$("forget-confirm-check").checked || !ui.forgetPlan;
}), $("forget-execute-btn").addEventListener("click", async () => {
 if (ui.forgetPlan && $("forget-confirm-check").checked) {
  notice();
  try {
   const report = await api("/api/memory/forget/confirm", {
    method: "POST",
    body: JSON.stringify({
     plan_id: ui.forgetPlan.plan_id,
     confirmed: !0,
     verification_query: $("forget-verify-query").value.trim() || void 0,
     bot_id: $("identity-bot").value || "muse"
    })
   });
   ui.forgetPlan = null, $("forget-confirm-box").hidden = !0, $("forget-report-output").textContent = JSON.stringify(report, null, 2);
  } catch (err) {
   notice(err.message);
  }
 }
}), $("discretion-load-form").addEventListener("submit", event => {
 event.preventDefault(), loadDiscretion();
}), $("discretion-save-btn").addEventListener("click", () => {
 const chatId = $("discretion-chat-id").value.trim();
 chatId && mutate($("discretion-save-btn"), `/api/side-chats/${encodeURIComponent(chatId)}/discretion`, {
  rules_text: $("discretion-rules").value,
  expected_sha256: ui.discretionSha,
  loosens: $("discretion-loosens").checked
 }, pol => {
  ui.discretionSha = pol.sha256 || null, $("discretion-sha").textContent = `Saved SHA-256: ${pol.sha256 || "ok"}`;
 }, "PUT");
}), $("skill-search-form").addEventListener("submit", event => {
 event.preventDefault(), refreshTabData("skills");
}), $("skills-list").addEventListener("click", event => {
 const btn = event.target.closest("[data-load-skill]");
 btn && inspectCatalog("skills", btn.dataset.loadSkill);
}), $("connectors-list").addEventListener("click", event => {
 const btn = event.target.closest("[data-load-connector]");
 btn && inspectCatalog("connectors", btn.dataset.loadConnector);
}), $("custom-skill-form").addEventListener("submit", event => {
 event.preventDefault();
 const body = {
  name: $("custom-skill-name").value.trim(),
  title: $("custom-skill-title").value.trim(),
  description: $("custom-skill-desc").value.trim(),
  body_markdown: $("custom-skill-body").value
 };
 mutate(event.submitter, "/api/skills/custom", body, () => {
  event.target.reset(), refreshTabData("skills");
 });
}), document.addEventListener("visibilitychange", () => {
 !document.hidden && ui.refreshAgain && (ui.refreshAgain = !1, refreshAll());
}), window.addEventListener("pagehide", () => {
 ui.stream?.abort(), ui.token = "";
}), setTab(ui.tab), connectEvents();
