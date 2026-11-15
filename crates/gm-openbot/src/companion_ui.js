'use strict';

const MAX_RENDERED_PARTS = 20;

const ui = {
  token: '',
  channel: '',
  audit: '',
  tab: window.location.pathname === '/admin/audit' ? 'audit' : 'chat',
  epoch: 0,
  refreshing: false,
  transcript: '',
  channels: [],
  connectionError: false,
  pending: new Set(),
  identitySha: null,
  importSha: null,
  importText: '',
  forgetPlan: null,
  discretionSha: null,
  activeSpaceSlug: null,
  activeSpaceUrl: null,
};

const $ = id => document.getElementById(id);

function node(tag, text, className) {
  const el = document.createElement(tag);
  if (text !== undefined) el.textContent = text;
  if (className) el.className = className;
  return el;
}

function notice(message = '') {
  $('notice').textContent = message;
}

function status(message, ok) {
  $('status-pill').textContent = message;
  $('status-pill').className = `badge ${ok ? 'permitted' : 'failed'}`;
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers);
  if (ui.token) headers.set('Authorization', `Bearer ${ui.token}`);
  if (options.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  const response = await fetch(path, {
    ...options,
    headers,
    cache: 'no-store',
    signal: AbortSignal.timeout(10000),
  });
  if (!response.ok) {
    let payload = null;
    try {
      payload = await response.json();
    } catch (_) {
      payload = null;
    }
    if (payload && typeof payload.error === 'string' && payload.error.trim()) {
      const err = new Error(payload.error);
      err.status = response.status;
      if (payload.current_sha256 !== undefined) err.current_sha256 = payload.current_sha256;
      throw err;
    }
    const messages = {
      401: 'Authentication required. Enter the server token and connect.',
      400: 'Request rejected. Check inputs or channel state.',
      403: 'Action blocked by safety policy or confirmation gate.',
      404: 'Requested resource no longer exists. Refresh and try again.',
      409: 'Optimistic concurrency or uniqueness conflict.',
    };
    const err = new Error(messages[response.status] || `Server returned HTTP ${response.status}. Try again.`);
    err.status = response.status;
    throw err;
  }
  const ctype = response.headers.get('content-type') || '';
  if (ctype.includes('application/json')) return response.json();
  return response.text();
}

// ---- Hand-written O(n) Muse directive parser matching gm-muse/src/directives.rs ----
function splitCappedSegments(text) {
  const segments = [];
  let restStart = 0;
  let i = 0;
  while (i < text.length) {
    if (text[i] === '<') {
      let matchedLen = 0;
      if (text.startsWith('<message_break/>', i)) matchedLen = '<message_break/>'.length;
      else if (text.startsWith('<message_break />', i)) matchedLen = '<message_break />'.length;
      if (matchedLen > 0) {
        const seg = text.slice(restStart, i).trim();
        if (seg) segments.push(seg);
        i += matchedLen;
        restStart = i;
        continue;
      }
    }
    i += 1;
  }
  const tail = text.slice(restStart).trim();
  if (tail) segments.push(tail);
  if (segments.length > MAX_RENDERED_PARTS) {
    const merged = segments.slice(MAX_RENDERED_PARTS - 1).join('\n\n');
    segments.length = MAX_RENDERED_PARTS - 1;
    segments.push(merged);
  }
  return segments;
}

function tryScanSandboxAttachment(s, start) {
  if (!s.startsWith('![', start)) return null;
  const altStart = start + 2;
  let depth = 1;
  let closeBracket = -1;
  for (let i = altStart; i < s.length; i += 1) {
    const ch = s[i];
    if (ch === '\n' || ch === '\r') return null;
    if (ch === '[') depth += 1;
    else if (ch === ']') {
      depth -= 1;
      if (depth === 0) {
        closeBracket = i;
        break;
      }
    }
  }
  if (closeBracket < 0) return null;
  const afterBracket = closeBracket + 1;
  if (!s.startsWith('(sandbox://', afterBracket)) return null;
  const uriStart = afterBracket + 1;
  let pDepth = 1;
  let closeParen = -1;
  for (let i = uriStart; i < s.length; i += 1) {
    const ch = s[i];
    if (ch === '\n' || ch === '\r' || ch === '\0') return null;
    if (ch === '(') pDepth += 1;
    else if (ch === ')') {
      pDepth -= 1;
      if (pDepth === 0) {
        closeParen = i;
        break;
      }
    }
  }
  if (closeParen < 0) return null;
  const uri = s.slice(uriStart, closeParen).trim();
  if (uri === 'sandbox://' || /\s/.test(uri)) return null;
  const alt = s.slice(altStart, closeBracket).trim();
  return [{ alt, uri }, closeParen + 1];
}

function tryScanCitation(s, start) {
  if (s[start] !== '【') return null;
  const innerStart = start + 1;
  let depth = 1;
  for (let i = innerStart; i < s.length; i += 1) {
    const ch = s[i];
    if (ch === '【') depth += 1;
    else if (ch === '】') {
      depth -= 1;
      if (depth === 0) {
        const raw = s.slice(innerStart, i).trim();
        if (!raw) return null;
        return [raw, i + 1];
      }
    }
  }
  return null;
}

function tryScanOptionsBlock(s, start) {
  if (!s.startsWith('<options>', start)) return null;
  const innerStart = start + '<options>'.length;
  const closeIdx = s.indexOf('</options>', innerStart);
  if (closeIdx < 0) return null;
  const inner = s.slice(innerStart, closeIdx);
  const endIdx = closeIdx + '</options>'.length;
  const extracted = [];
  let pos = 0;
  while (true) {
    const optStart = inner.indexOf('<option>', pos);
    if (optStart < 0) break;
    const itemStart = optStart + '<option>'.length;
    const optEnd = inner.indexOf('</option>', itemStart);
    if (optEnd < 0) break;
    const label = inner.slice(itemStart, optEnd).trim();
    if (label) extracted.push(label);
    pos = optEnd + '</option>'.length;
  }
  if (!extracted.length) {
    for (const rawLine of inner.split(/\r?\n/)) {
      const trimmed = rawLine.trim();
      if (!trimmed) continue;
      let cleaned = trimmed;
      if (trimmed.startsWith('- ') || trimmed.startsWith('* ')) cleaned = trimmed.slice(2).trim();
      else {
        const match = trimmed.match(/^\d+\.\s+(.*)$/);
        if (match) cleaned = match[1].trim();
      }
      if (cleaned) extracted.push(cleaned);
    }
  }
  return [extracted, endIdx];
}

function cleanPartText(raw) {
  const lines = [];
  let prevBlank = true;
  for (const line of raw.split(/\r?\n/)) {
    const trimmedEnd = line.replace(/\s+$/, '');
    if (!trimmedEnd.trim()) {
      if (!prevBlank) {
        lines.push('');
        prevBlank = true;
      }
    } else {
      lines.push(trimmedEnd);
      prevBlank = false;
    }
  }
  while (lines.length && !lines[lines.length - 1]) lines.pop();
  return lines.join('\n').trim();
}

function scanSegment(segment) {
  let textBuf = '';
  const attachments = [];
  const citations = [];
  const citationIndexMap = new Map();
  const options = [];
  let i = 0;
  while (i < segment.length) {
    const ch = segment[i];
    if (ch === '!') {
      const scanned = tryScanSandboxAttachment(segment, i);
      if (scanned) {
        attachments.push(scanned[0]);
        i = scanned[1];
        continue;
      }
    }
    if (ch === '【') {
      const scanned = tryScanCitation(segment, i);
      if (scanned) {
        const rawCite = scanned[0];
        let idx = citationIndexMap.get(rawCite);
        if (!idx) {
          idx = citations.length + 1;
          citationIndexMap.set(rawCite, idx);
          citations.push({ index: idx, raw: rawCite });
        }
        textBuf += `[${idx}]`;
        i = scanned[1];
        continue;
      }
    }
    if (ch === '<') {
      const scanned = tryScanOptionsBlock(segment, i);
      if (scanned) {
        options.push(...scanned[0]);
        i = scanned[1];
        continue;
      }
    }
    textBuf += ch;
    i += 1;
  }
  return {
    text: cleanPartText(textBuf),
    attachments,
    citations,
    options,
  };
}

function parseDirectives(text) {
  if (!text || !text.trim()) return [];
  return splitCappedSegments(text)
    .map(scanSegment)
    .filter(p => p.text || p.attachments.length || p.citations.length || p.options.length);
}

// ---- Core UI helpers ----
function setTab(tabName) {
  ui.tab = tabName;
  document.querySelectorAll('[data-tab]').forEach(btn => {
    btn.setAttribute('aria-selected', String(btn.dataset.tab === tabName));
  });
  document.querySelectorAll('.tab-panel').forEach(panel => {
    panel.hidden = panel.id !== `panel-${tabName}`;
  });
  const grid = $('workspace-grid');
  const auditPanel = $('audit-panel');
  if (tabName === 'audit') {
    grid.classList.add('audit-focus');
    auditPanel.hidden = false;
  } else if (tabName === 'chat') {
    grid.classList.remove('audit-focus');
    auditPanel.hidden = false;
  } else {
    grid.classList.add('audit-focus');
    auditPanel.hidden = true;
  }
  void refreshTabData(tabName);
}

function fillBots(id, bots, includeNone = false) {
  const select = $(id);
  const values = new Set([...select.selectedOptions].map(o => o.value));
  const options = bots.map(b => {
    const option = node('option', `@${b.id}`);
    option.value = b.id;
    option.selected = values.has(b.id);
    return option;
  });
  if (includeNone) {
    const none = node('option', 'No default responder');
    none.value = '';
    none.selected = values.has('');
    options.unshift(none);
  }
  select.replaceChildren(...options);
}

function renderBots(bots) {
  const view = JSON.stringify(bots);
  if ($('bots-list').dataset.view === view) return;
  $('bots-list').dataset.view = view;
  $('bots-list').replaceChildren(...bots.map(b => {
    const item = node('div', undefined, 'item');
    item.append(
      node('strong', `${b.avatar_emoji} @${b.id} · ${b.display_name}`),
      node('div', b.role_title, 'muted'),
    );
    return item;
  }));
  const signature = JSON.stringify(bots.map(b => b.id));
  if ($('task-bot').dataset.bots !== signature) {
    fillBots('task-bot', bots);
    fillBots('channel-owner', bots, true);
    fillBots('channel-members', bots);
    fillBots('identity-bot', bots);
    $('task-bot').dataset.bots = signature;
  }
}

function renderChannels(channels) {
  ui.channels = channels;
  if (!channels.some(c => c.id === ui.channel)) ui.channel = channels[0]?.id || '';
  const view = JSON.stringify([channels, ui.channel]);
  if ($('channels-list').dataset.view === view) return;
  $('channels-list').dataset.view = view;
  $('channels-list').replaceChildren(...channels.map(c => {
    const button = node('button', `${c.name}${c.paused ? ' · paused' : ''}`, 'channel-button');
    button.type = 'button';
    button.dataset.channel = c.id;
    button.setAttribute('aria-pressed', String(c.id === ui.channel));
    return button;
  }));
  const current = channels.find(c => c.id === ui.channel);
  $('channel-heading').textContent = current?.name || 'No channels';
  $('channel-description').textContent = current
    ? `${current.topic} · Default responder: ${current.owner_bot_id ? '@' + current.owner_bot_id : 'none'}${current.paused ? ' · Paused: messages are saved without starting work.' : ''}`
    : 'Create a channel to start a conversation.';
  $('message-form').querySelector('button').disabled = !current;
}

function renderMessages(messages, channel) {
  const signature = JSON.stringify([channel, messages]);
  if (signature === ui.transcript) return;
  const list = $('messages-list');
  const follow = list.scrollHeight - list.scrollTop - list.clientHeight < 50 || channel !== list.dataset.channel;
  list.replaceChildren(...messages.map(m => {
    const item = node('div', undefined, 'item');
    item.append(node('strong', m.bot_id ? `@${m.bot_id}` : m.author));
    const time = node('time', ` · ${new Date(m.timestamp).toLocaleString()}`, 'muted');
    time.dateTime = m.timestamp;
    item.append(time);

    const parts = parseDirectives(m.content || '');
    if (!parts.length) {
      item.append(node('div', m.content || '', 'content'));
    } else {
      for (const part of parts) {
        const bubble = node('div', undefined, parts.length > 1 ? 'bubble' : '');
        if (part.text) bubble.append(node('div', part.text, 'content'));
        if (part.attachments.length) {
          const attRow = node('div', undefined, 'chips');
          for (const att of part.attachments) {
            const label = att.alt ? `${att.alt} (${att.uri})` : att.uri;
            attRow.append(node('span', `📎 ${label}`, 'chip'));
          }
          bubble.append(attRow);
        }
        if (part.options.length) {
          const optRow = node('div', undefined, 'chips');
          for (const label of part.options) {
            const btn = node('button', label, 'secondary');
            btn.type = 'button';
            btn.dataset.optionLabel = label;
            optRow.append(btn);
          }
          bubble.append(optRow);
        }
        if (part.citations.length) {
          const det = node('details');
          det.append(node('summary', `Sources (${part.citations.length})`));
          for (const cite of part.citations) {
            det.append(node('div', `[${cite.index}] ${cite.raw}`, 'muted'));
          }
          bubble.append(det);
        }
        item.append(bubble);
      }
    }

    const rxnRow = node('div', undefined, 'chips');
    const rxnList = Array.isArray(m.reactions) ? m.reactions : [];
    for (const r of rxnList) {
      const btn = node('button', `${r.emoji} ${r.count}`, 'secondary');
      btn.type = 'button';
      btn.dataset.reactMessage = m.id;
      btn.dataset.reactEmoji = r.emoji;
      rxnRow.append(btn);
    }
    for (const quick of ['👍', '❤️', '🔥']) {
      if (!rxnList.some(r => r.emoji === quick)) {
        const qbtn = node('button', quick, 'secondary');
        qbtn.type = 'button';
        qbtn.dataset.reactMessage = m.id;
        qbtn.dataset.reactEmoji = quick;
        rxnRow.append(qbtn);
      }
    }
    item.append(rxnRow);
    return item;
  }));
  if (!messages.length) list.append(node('p', 'No messages yet. Send the first one below.', 'muted'));
  list.dataset.channel = channel;
  ui.transcript = signature;
  if (follow) list.scrollTop = list.scrollHeight;
}

function actionButton(id, action, label) {
  const button = node('button', label, 'secondary');
  button.type = 'button';
  button.dataset.task = id;
  button.dataset.action = action;
  return button;
}

function renderQueue(tasks) {
  const view = JSON.stringify(tasks);
  if ($('queue-list').dataset.view === view) return;
  $('queue-list').dataset.view = view;
  $('queue-list').replaceChildren(...tasks.map(t => {
    const item = node('div', undefined, 'item');
    item.append(node('strong', `@${t.bot_id} · ${t.status.replaceAll('_', ' ').toUpperCase()}`), node('div', t.instruction, 'content'));
    if (t.result_summary) item.append(node('div', t.result_summary, 'content'));
    const actions = node('div', undefined, 'row');
    if (t.status === 'waiting_for_human' || (t.mutating_in_flight && ['failed', 'cancelled'].includes(t.status))) {
      item.append(node('div', `Take the Wheel: ${t.blocker_note || 'Human intervention needed'}`, 'content'));
      actions.append(actionButton(t.id, 'wheel-resume', 'Hand Back to Bot'));
    }
    if (['queued', 'running'].includes(t.status)) actions.append(actionButton(t.id, 'pause', 'Pause'));
    if (t.status === 'paused') actions.append(actionButton(t.id, 'wheel-resume', 'Review / Resume'));
    if (['failed', 'cancelled'].includes(t.status) && !t.mutating_in_flight) actions.append(actionButton(t.id, 'retry', 'Retry'));
    if (['queued', 'running', 'pause_requested', 'paused', 'waiting_for_human'].includes(t.status)) actions.append(actionButton(t.id, 'cancel', 'Cancel'));
    item.append(actions);
    return item;
  }));
  if (!tasks.length) $('queue-list').append(node('p', 'No queued tasks.', 'muted'));
}

function renderAudit(records) {
  const view = JSON.stringify(records);
  if ($('audit-list').dataset.view === view) return;
  $('audit-list').dataset.view = view;
  $('audit-list').replaceChildren(...records.map(r => {
    const item = node('div', undefined, 'item');
    const category = ['permitted', 'refused', 'failed'].includes(r.category) ? r.category : '';
    item.append(
      node('span', r.category.toUpperCase(), `badge ${category}`),
      node('code', ` ${r.rule_id}`),
      node('div', r.action_summary, 'content'),
      node('div', r.citation, 'muted'),
    );
    return item;
  }));
  if (!records.length) $('audit-list').append(node('p', 'No matching audit entries.', 'muted'));
}

// ---- Feed tab ----
function renderFeed(data) {
  const posts = Array.isArray(data?.posts) ? data.posts : [];
  const promptInput = $('feed-brief-prompt');
  if (document.activeElement !== promptInput && data?.brief_prompt !== undefined && data?.brief_prompt !== null) {
    promptInput.value = data.brief_prompt || '';
  }
  const view = JSON.stringify(posts);
  if ($('feed-list').dataset.view === view) return;
  $('feed-list').dataset.view = view;
  $('feed-list').replaceChildren(...posts.map(p => {
    const item = node('div', undefined, 'item');
    const head = node('div', undefined, 'row');
    head.append(node('span', p.kicker || 'Update', 'badge'), node('strong', p.title));
    item.append(head, node('div', p.body, 'content'));
    if (Array.isArray(p.signal_citations) && p.signal_citations.length) {
      item.append(node('div', `Citations: ${p.signal_citations.join(' · ')}`, 'muted'));
    }
    const actions = node('div', undefined, 'row');
    actions.append(node('span', p.rating != null ? `Rating: ${p.rating}` : 'Rate:', 'muted'));
    for (const [val, label] of [[5, 'Helpful (5)'], [1, '👍 +1'], [-1, '👎 -1']]) {
      const btn = node('button', label, 'secondary');
      btn.type = 'button';
      btn.dataset.ratePost = p.id;
      btn.dataset.rateVal = String(val);
      actions.append(btn);
    }
    item.append(actions);
    return item;
  }));
  if (!posts.length) $('feed-list').append(node('p', 'No feed posts yet.', 'muted'));
}

// ---- Ideas tab ----
function renderIdeas(ideas) {
  const list = Array.isArray(ideas) ? ideas : [];
  const view = JSON.stringify(list);
  if ($('ideas-list').dataset.view === view) return;
  $('ideas-list').dataset.view = view;
  $('ideas-list').replaceChildren(...list.map(idea => {
    const item = node('div', undefined, 'item');
    item.append(
      node('strong', idea.title),
      node('div', `${idea.source || 'hybrid'} · confidence ${(Number(idea.confidence || 0) * 100).toFixed(0)}%`, 'muted'),
      node('div', idea.description || '', 'content'),
    );
    const actions = node('div', undefined, 'row');
    const runBtn = node('button', 'Run', undefined);
    runBtn.type = 'button';
    runBtn.dataset.ideaRun = idea.id;
    const dismissBtn = node('button', 'Dismiss once', 'secondary');
    dismissBtn.type = 'button';
    dismissBtn.dataset.ideaDismiss = idea.id;
    actions.append(runBtn, dismissBtn);
    item.append(actions);
    return item;
  }));
  if (!list.length) $('ideas-list').append(node('p', 'No proactive ideas available right now.', 'muted'));
}

// ---- Goals tab ----
function renderGoals(goals) {
  const items = Array.isArray(goals) ? goals : [];
  const topLevel = items.filter(g => !g.parent_goal_id);
  const subParentSel = $('goal-parent');
  const prevParent = subParentSel.value;
  const pOpts = [node('option', '(Top-level Goal)')];
  pOpts[0].value = '';
  for (const g of topLevel) {
    const opt = node('option', `${g.title} (${g.slug})`);
    opt.value = g.slug;
    pOpts.push(opt);
  }
  subParentSel.replaceChildren(...pOpts);
  subParentSel.value = prevParent;

  const view = JSON.stringify(items);
  if ($('goals-list').dataset.view === view) return;
  $('goals-list').dataset.view = view;

  const byParent = new Map();
  for (const g of items) {
    if (g.parent_goal_id) {
      const arr = byParent.get(g.parent_goal_id) || [];
      arr.push(g);
      byParent.set(g.parent_goal_id, arr);
    }
  }

  function renderGoalCard(g, isSubgoal) {
    const card = node('div', undefined, isSubgoal ? 'bubble' : 'item');
    const row = node('div', undefined, 'row');
    row.append(
      node('strong', `${isSubgoal ? '↳ ' : ''}${g.title}`),
      node('span', g.status || 'active', 'badge'),
      node('code', g.slug),
    );
    card.append(row);
    if (g.description) card.append(node('div', g.description, 'content'));
    if (g.pause_notes) card.append(node('div', `Note: ${g.pause_notes}`, 'muted'));
    if (Array.isArray(g.stop_conditions) && g.stop_conditions.length) {
      card.append(node('div', `Stop conditions: ${g.stop_conditions.join(', ')}`, 'muted'));
    }
    const files = Array.isArray(g.files) ? g.files : [];
    if (files.length) {
      const fileRow = node('div', undefined, 'chips');
      for (const rel of files) {
        const fbtn = node('button', `📄 ${rel}`, 'secondary');
        fbtn.type = 'button';
        fbtn.dataset.goalSlug = g.slug;
        fbtn.dataset.goalFile = rel;
        fileRow.append(fbtn);
      }
      card.append(fileRow);
    }
    const actions = node('div', undefined, 'row');
    for (const [act, label] of [['complete', 'Complete'], ['abandon', 'Abandon'], ['delete', 'Delete']]) {
      const btn = node('button', label, act === 'delete' ? 'danger' : 'secondary');
      btn.type = 'button';
      btn.dataset.goalAction = act;
      btn.dataset.goalSlug = g.slug;
      actions.append(btn);
    }
    card.append(actions);
    if (!isSubgoal) {
      const children = byParent.get(g.id) || [];
      for (const sub of children) card.append(renderGoalCard(sub, true));
    }
    return card;
  }

  $('goals-list').replaceChildren(...topLevel.map(g => renderGoalCard(g, false)));
  if (!items.length) $('goals-list').append(node('p', 'No goals created yet.', 'muted'));
}

// ---- Spaces tab ----
function renderSpaces(spaces) {
  const list = Array.isArray(spaces) ? spaces : [];
  const view = JSON.stringify(list);
  if ($('spaces-list').dataset.view === view) return;
  $('spaces-list').dataset.view = view;
  $('spaces-list').replaceChildren(...list.map(sp => {
    const item = node('div', undefined, 'item');
    const row = node('div', undefined, 'row');
    row.append(
      node('strong', sp.title || sp.slug),
      node('code', sp.slug),
      node('span', sp.published ? 'PUBLISHED' : 'LOCAL ONLY', sp.published ? 'badge permitted' : 'badge'),
    );
    item.append(row);
    const openBtn = node('button', 'Open Live Space', undefined);
    openBtn.type = 'button';
    openBtn.dataset.spaceOpen = sp.slug;
    const pubBtn = node('button', 'Publish…', 'secondary');
    pubBtn.type = 'button';
    pubBtn.dataset.spacePub = sp.slug;
    const btns = node('div', undefined, 'row');
    btns.append(openBtn, pubBtn);
    item.append(btns);
    return item;
  }));
  if (!list.length) $('spaces-list').append(node('p', 'No static Spaces created yet.', 'muted'));
}

async function openSpace(slug) {
  notice();
  try {
    const res = await api(`/api/spaces/${encodeURIComponent(slug)}/url`);
    ui.activeSpaceSlug = slug;
    ui.activeSpaceUrl = res.url;
    $('space-stage').hidden = false;
    $('space-publish-confirm').hidden = true;
    $('space-active-label').textContent = `Space: ${slug}`;
    $('space-active-origin').textContent = `Served on isolated origin: ${new URL(res.url).origin} (port ${res.spaces_port})`;
    $('space-iframe').src = res.url;
  } catch (err) {
    notice(err.message);
  }
}

function postSpaceTheme(theme) {
  if (!ui.activeSpaceUrl) return;
  const iframe = $('space-iframe');
  if (!iframe.contentWindow) return;
  const targetOrigin = new URL(ui.activeSpaceUrl).origin;
  iframe.contentWindow.postMessage({ type: 'hatch:space:theme', theme }, targetOrigin);
}

// ---- Skills tab ----
function renderSkillsAndConnectors(skills, connectors) {
  const sList = Array.isArray(skills) ? skills : [];
  $('skills-list').replaceChildren(...sList.map(s => {
    const item = node('div', undefined, 'item');
    const row = node('div', undefined, 'row');
    row.append(node('strong', s.title || s.name), node('code', s.name), node('span', s.category || 'skill', 'badge'));
    const btn = node('button', 'Inspect SKILL.md', 'secondary');
    btn.type = 'button';
    btn.dataset.loadSkill = s.name;
    row.append(btn);
    item.append(row, node('div', s.description || '', 'muted'));
    return item;
  }));
  if (!sList.length) $('skills-list').append(node('p', 'No matching skills.', 'muted'));

  const cList = Array.isArray(connectors) ? connectors : [];
  $('connectors-list').replaceChildren(...cList.map(c => {
    const item = node('div', undefined, 'item');
    const row = node('div', undefined, 'row');
    row.append(node('strong', c.title || c.id), node('code', c.id), node('span', c.require_auth ? 'auth required' : 'no auth', 'badge'));
    const btn = node('button', 'Inspect Connector', 'secondary');
    btn.type = 'button';
    btn.dataset.loadConnector = c.id;
    row.append(btn);
    item.append(row, node('div', c.description || '', 'muted'));
    return item;
  }));
  if (!cList.length) $('connectors-list').append(node('p', 'No matching connectors.', 'muted'));
}

// ---- Refreshers ----
async function refreshTabData(tabName = ui.tab) {
  try {
    if (tabName === 'feed') renderFeed(await api('/api/activity'));
    else if (tabName === 'ideas') renderIdeas(await api('/api/ideas'));
    else if (tabName === 'goals') renderGoals(await api('/api/goals'));
    else if (tabName === 'spaces') renderSpaces(await api('/api/spaces'));
    else if (tabName === 'skills') {
      const q = $('skill-search-q').value.trim();
      const qs = q ? `?q=${encodeURIComponent(q)}` : '';
      const [skills, connectors] = await Promise.all([api(`/api/skills${qs}`), api(`/api/connectors${qs}`)]);
      renderSkillsAndConnectors(skills, connectors);
    } else if (tabName === 'identity') {
      const prefs = await api('/api/proactive-prefs');
      $('prefs-summary').textContent = JSON.stringify(prefs, null, 2);
    }
  } catch (err) {
    if (err.status !== 401) notice(err.message);
  }
}

async function refreshAll() {
  if (ui.refreshing) return;
  ui.refreshing = true;
  const epoch = ui.epoch;
  const audit = ui.audit;
  const channel = ui.channel;
  try {
    const [bots, channels, tasks, records, runtime] = await Promise.all([
      api('/api/bots'),
      api('/api/channels'),
      api('/api/queue'),
      api(`/api/audit${audit ? '?category=' + encodeURIComponent(audit) : ''}`),
      api('/api/runtime'),
    ]);
    if (epoch !== ui.epoch) return;
    renderBots(bots);
    renderChannels(channels);
    renderQueue(tasks);
    if (audit === ui.audit) renderAudit(records);
    const selected = ui.channel;
    if (selected) {
      const messages = await api(`/api/channels/${encodeURIComponent(selected)}/messages`);
      if (epoch === ui.epoch && selected === ui.channel) renderMessages(messages, selected);
    } else renderMessages([], '');
    if (epoch === ui.epoch) {
      status(
        runtime.worker_running
          ? (runtime.demo_mode ? 'Demo workers · simulated model' : 'Connected · workers active')
          : 'Connected · task worker unavailable',
        true,
      );
      $('runtime-status').textContent = runtime.worker_running
        ? `Tasks execute automatically. Host desktop control: ${runtime.host_desktop_opt_in ? 'enabled by opt-in' : 'disabled'}. Shell: ${runtime.shell_environment}. Spaces port: ${runtime.spaces_port}.`
        : 'Task worker is unavailable; requests remain queued until a runtime starts.';
      if (ui.connectionError) notice();
      ui.connectionError = false;
      if (ui.tab !== 'chat' && ui.tab !== 'audit') void refreshTabData(ui.tab);
    }
  } catch (error) {
    if (epoch === ui.epoch) {
      ui.connectionError = true;
      status('Connection unavailable', false);
      notice(error.message);
    }
  } finally {
    ui.refreshing = false;
    if (epoch !== ui.epoch || audit !== ui.audit || channel !== ui.channel) void refreshAll();
  }
}

async function mutate(button, path, body, onSuccess, method = 'POST') {
  const key = `${method}:${path}`;
  if (ui.pending.has(key)) return;
  ui.pending.add(key);
  if (button) button.disabled = true;
  ui.connectionError = false;
  notice();
  try {
    const result = await api(path, { method, body: body === undefined ? undefined : JSON.stringify(body) });
    if (result && result.ok === false) throw new Error('The operation did not complete. Refresh and try again.');
    onSuccess?.(result);
    ui.epoch++;
    void refreshAll();
  } catch (error) {
    notice(error.message);
  } finally {
    ui.pending.delete(key);
    if (button) button.disabled = false;
  }
}

// ---- Event Wiring ----
document.querySelectorAll('[data-tab]').forEach(btn => {
  btn.addEventListener('click', () => setTab(btn.dataset.tab));
});

$('connection-form').addEventListener('submit', event => {
  event.preventDefault();
  ui.token = $('api-token').value.trim();
  $('api-token').value = '';
  ui.epoch++;
  notice();
  void refreshAll();
});

$('channels-list').addEventListener('click', event => {
  const button = event.target.closest('[data-channel]');
  if (!button) return;
  ui.channel = button.dataset.channel;
  ui.epoch++;
  notice();
  renderChannels(ui.channels);
  $('messages-list').replaceChildren(node('p', 'Loading messages…', 'muted'));
  ui.transcript = '';
  void refreshAll();
});

$('messages-list').addEventListener('click', event => {
  const optBtn = event.target.closest('[data-option-label]');
  if (optBtn && ui.channel) {
    void mutate(optBtn, `/api/channels/${encodeURIComponent(ui.channel)}/messages`, { content: optBtn.dataset.optionLabel });
    return;
  }
  const rxnBtn = event.target.closest('[data-react-message]');
  if (rxnBtn) {
    void mutate(
      rxnBtn,
      `/api/messages/${encodeURIComponent(rxnBtn.dataset.reactMessage)}/reactions`,
      { emoji: rxnBtn.dataset.reactEmoji },
    );
  }
});

$('message-form').addEventListener('submit', event => {
  event.preventDefault();
  const input = $('message-input');
  const content = input.value;
  if (!content.trim() || !ui.channel) return;
  void mutate(event.submitter || event.target.querySelector('button'), `/api/channels/${encodeURIComponent(ui.channel)}/messages`, { content }, () => {
    if (input.value === content) input.value = '';
  });
});

$('task-form').addEventListener('submit', event => {
  event.preventDefault();
  const input = $('task-input');
  const instruction = input.value;
  if (!instruction.trim()) return;
  void mutate(event.submitter || event.target.querySelector('button'), '/api/queue', { bot_id: $('task-bot').value, instruction, priority: 5 }, () => {
    if (input.value === instruction) input.value = '';
  });
});

$('channel-create-form').addEventListener('submit', event => {
  event.preventDefault();
  const form = event.target;
  const body = {
    id: $('channel-id').value.trim(),
    topic: $('channel-topic').value,
    owner_bot_id: $('channel-owner').value || null,
    participants: [...$('channel-members').selectedOptions].map(o => o.value),
  };
  void mutate(event.submitter || form.querySelector('button'), '/api/channels', body, channel => {
    ui.channel = channel.id;
    form.reset();
  });
});

$('queue-list').addEventListener('click', event => {
  const button = event.target.closest('[data-task]');
  if (!button) return;
  let body;
  if (button.dataset.action === 'wheel-resume') {
    const note = prompt('Optional note about what you completed:', 'Completed login/2FA step');
    if (note === null) return;
    body = { note };
  }
  void mutate(button, `/api/tasks/${encodeURIComponent(button.dataset.task)}/${button.dataset.action}`, body);
});

document.querySelectorAll('[data-audit]').forEach(button => {
  button.addEventListener('click', () => {
    ui.audit = button.dataset.audit;
    ui.epoch++;
    notice();
    void refreshAll();
  });
});

// Feed listeners
$('feed-prompt-form').addEventListener('submit', event => {
  event.preventDefault();
  void mutate(event.submitter, '/api/activity/prompt', { prompt: $('feed-brief-prompt').value }, () => {
    void refreshTabData('feed');
  });
});

$('feed-create-form').addEventListener('submit', event => {
  event.preventDefault();
  const body = {
    kicker: $('feed-post-kicker').value.trim() || 'Editorial',
    title: $('feed-post-title').value,
    body: $('feed-post-body').value,
  };
  void mutate(event.submitter, '/api/activity', body, () => {
    event.target.reset();
    void refreshTabData('feed');
  });
});

$('feed-list').addEventListener('click', event => {
  const btn = event.target.closest('[data-rate-post]');
  if (!btn) return;
  void mutate(btn, `/api/activity/${encodeURIComponent(btn.dataset.ratePost)}/rate`, { rating: Number(btn.dataset.rateVal) }, () => {
    void refreshTabData('feed');
  });
});

// Ideas listeners
$('ideas-refresh-btn').addEventListener('click', () => void refreshTabData('ideas'));
$('ideas-create-form').addEventListener('submit', event => {
  event.preventDefault();
  const body = {
    title: $('idea-title').value.trim(),
    description: $('idea-desc').value.trim() || undefined,
    action_prompt: $('idea-prompt').value.trim() || undefined,
  };
  void mutate(event.submitter, '/api/ideas', body, () => {
    event.target.reset();
    void refreshTabData('ideas');
  });
});

$('ideas-list').addEventListener('click', event => {
  const runBtn = event.target.closest('[data-idea-run]');
  if (runBtn) {
    void mutate(runBtn, `/api/ideas/${encodeURIComponent(runBtn.dataset.ideaRun)}/run`, { bot_id: $('task-bot').value || 'muse' });
    return;
  }
  const dismissBtn = event.target.closest('[data-idea-dismiss]');
  if (dismissBtn) {
    void mutate(dismissBtn, `/api/ideas/${encodeURIComponent(dismissBtn.dataset.ideaDismiss)}/dismiss`, {}, () => {
      void refreshTabData('ideas');
    });
  }
});

// Goals listeners
$('goal-create-form').addEventListener('submit', event => {
  event.preventDefault();
  const stopRaw = $('goal-stops').value.trim();
  const stopConditions = stopRaw ? stopRaw.split(',').map(s => s.trim()).filter(Boolean) : [];
  const body = {
    parent_slug: $('goal-parent').value || undefined,
    title: $('goal-title').value,
    description: $('goal-desc').value,
    stop_conditions: stopConditions,
  };
  void mutate(event.submitter, '/api/goals', body, () => {
    event.target.reset();
    void refreshTabData('goals');
  });
});

$('goals-list').addEventListener('click', async event => {
  const actionBtn = event.target.closest('[data-goal-action]');
  if (actionBtn) {
    void mutate(
      actionBtn,
      '/api/goals',
      { slug: actionBtn.dataset.goalSlug, action: actionBtn.dataset.goalAction, verified: true },
      () => void refreshTabData('goals'),
      'PATCH',
    );
    return;
  }
  const fileBtn = event.target.closest('[data-goal-file]');
  if (fileBtn) {
    notice();
    try {
      const slug = encodeURIComponent(fileBtn.dataset.goalSlug);
      const rel = fileBtn.dataset.goalFile.split('/').map(encodeURIComponent).join('/');
      const text = await api(`/api/goals/${slug}/files/${rel}`);
      $('goal-file-viewer').hidden = false;
      $('goal-file-title').textContent = `${fileBtn.dataset.goalSlug}/${fileBtn.dataset.goalFile}`;
      $('goal-file-content').textContent = typeof text === 'string' ? text : JSON.stringify(text, null, 2);
    } catch (err) {
      notice(err.message);
    }
  }
});

// Spaces listeners
$('space-create-form').addEventListener('submit', event => {
  event.preventDefault();
  const html = $('space-html').value.trim();
  const body = {
    slug: $('space-slug').value.trim(),
    title: $('space-title').value.trim() || undefined,
    kind: 'static',
    html: html || undefined,
  };
  void mutate(event.submitter, '/api/spaces', body, created => {
    event.target.reset();
    void refreshTabData('spaces');
    if (created?.slug) void openSpace(created.slug);
  });
});

$('spaces-list').addEventListener('click', event => {
  const openBtn = event.target.closest('[data-space-open]');
  if (openBtn) {
    void openSpace(openBtn.dataset.spaceOpen);
    return;
  }
  const pubBtn = event.target.closest('[data-space-pub]');
  if (pubBtn) {
    ui.activeSpaceSlug = pubBtn.dataset.spacePub;
    $('space-stage').hidden = false;
    $('space-publish-confirm').hidden = false;
    $('space-active-label').textContent = `Publish Confirmation: ${ui.activeSpaceSlug}`;
  }
});

$('space-theme-light').addEventListener('click', () => postSpaceTheme('light'));
$('space-theme-dark').addEventListener('click', () => postSpaceTheme('dark'));
$('space-publish-btn').addEventListener('click', () => {
  if (ui.activeSpaceSlug) $('space-publish-confirm').hidden = false;
});
$('space-publish-confirm-cancel').addEventListener('click', () => {
  $('space-publish-confirm').hidden = true;
});
$('space-publish-confirm-yes').addEventListener('click', () => {
  if (!ui.activeSpaceSlug) return;
  void mutate(
    $('space-publish-confirm-yes'),
    `/api/spaces/${encodeURIComponent(ui.activeSpaceSlug)}/publish`,
    { confirmed: true },
    () => {
      $('space-publish-confirm').hidden = true;
      void refreshTabData('spaces');
    },
  );
});

// Identity, Prefs, Import, Explain, Forget & Discretion listeners
async function loadIdentityFile() {
  notice();
  $('identity-conflict').hidden = true;
  const botId = $('identity-bot').value || 'muse';
  const file = $('identity-file').value;
  try {
    const res = await api(`/api/identity/${encodeURIComponent(botId)}/${encodeURIComponent(file)}`);
    ui.identitySha = res.sha256 || null;
    $('identity-editor').value = res.content || '';
    $('identity-sha').textContent = res.sha256 ? `Loaded SHA-256: ${res.sha256}` : 'File does not exist yet (will create on save).';
  } catch (err) {
    notice(err.message);
  }
}

$('identity-load-btn').addEventListener('click', () => void loadIdentityFile());
$('identity-save-btn').addEventListener('click', async () => {
  notice();
  $('identity-conflict').hidden = true;
  const botId = $('identity-bot').value || 'muse';
  const file = $('identity-file').value;
  const btn = $('identity-save-btn');
  btn.disabled = true;
  try {
    const res = await api(`/api/identity/${encodeURIComponent(botId)}/${encodeURIComponent(file)}`, {
      method: 'PUT',
      body: JSON.stringify({ content: $('identity-editor').value, expected_sha256: ui.identitySha }),
    });
    ui.identitySha = res.sha256;
    $('identity-sha').textContent = `Saved SHA-256: ${res.sha256}`;
  } catch (err) {
    if (err.status === 409) {
      $('identity-conflict').hidden = false;
      $('identity-conflict').textContent = `409 Conflict: file changed on disk (current SHA-256: ${err.current_sha256 || 'unknown'}). Click Load File to inspect latest before saving.`;
    }
    notice(err.message);
  } finally {
    btn.disabled = false;
  }
});

$('prefs-form').addEventListener('submit', event => {
  event.preventDefault();
  const qStart = $('prefs-quiet-start').value;
  const qEnd = $('prefs-quiet-end').value;
  const body = {
    add_tell: $('prefs-tell').value.trim() || undefined,
    add_opt_out: $('prefs-opt-out').value.trim() || undefined,
    quiet_start_hour: qStart !== '' ? Number(qStart) : undefined,
    quiet_end_hour: qEnd !== '' ? Number(qEnd) : undefined,
    home_tz: $('prefs-home-tz').value.trim() || undefined,
    work_tz: $('prefs-work-tz').value.trim() || undefined,
    feedback_note: $('prefs-feedback-note').value.trim() || undefined,
  };
  void mutate(event.submitter, '/api/proactive-prefs', body, updated => {
    $('prefs-summary').textContent = JSON.stringify(updated, null, 2);
    $('prefs-tell').value = '';
    $('prefs-opt-out').value = '';
    $('prefs-feedback-note').value = '';
  }, 'PUT');
});

$('import-preview-btn').addEventListener('click', async () => {
  notice();
  const input = $('import-input').value;
  if (!input.trim()) return;
  try {
    const rep = await api('/api/memory/import', {
      method: 'POST',
      body: JSON.stringify({ input, apply: false, bot_id: $('identity-bot').value || 'muse' }),
    });
    ui.importSha = rep.report_sha256 || null;
    ui.importText = input;
    $('import-apply-btn').disabled = !ui.importSha;
    $('import-report').textContent = JSON.stringify(rep, null, 2);
  } catch (err) {
    notice(err.message);
  }
});

$('import-apply-btn').addEventListener('click', async () => {
  notice();
  if (!ui.importSha) return;
  try {
    const rep = await api('/api/memory/import', {
      method: 'POST',
      body: JSON.stringify({
        input: ui.importText,
        apply: true,
        report_sha256: ui.importSha,
        bot_id: $('identity-bot').value || 'muse',
      }),
    });
    ui.importSha = null;
    $('import-apply-btn').disabled = true;
    $('import-report').textContent = JSON.stringify(rep, null, 2);
  } catch (err) {
    notice(err.message);
  }
});

$('explain-form').addEventListener('submit', async event => {
  event.preventDefault();
  notice();
  try {
    const res = await api('/api/memory/explain', {
      method: 'POST',
      body: JSON.stringify({ target: $('explain-target').value.trim() }),
    });
    $('explain-output').textContent = JSON.stringify(res, null, 2);
  } catch (err) {
    notice(err.message);
  }
});

$('forget-plan-form').addEventListener('submit', async event => {
  event.preventDefault();
  notice();
  try {
    const plan = await api('/api/memory/forget/plan', {
      method: 'POST',
      body: JSON.stringify({ request: $('forget-request').value, bot_id: $('identity-bot').value || 'muse' }),
    });
    ui.forgetPlan = plan;
    $('forget-plan-output').textContent = JSON.stringify(plan, null, 2);
    $('forget-confirm-box').hidden = false;
    $('forget-confirm-check').checked = false;
    $('forget-execute-btn').disabled = true;
    $('forget-report-output').textContent = '';
  } catch (err) {
    notice(err.message);
  }
});

$('forget-confirm-check').addEventListener('change', () => {
  $('forget-execute-btn').disabled = !$('forget-confirm-check').checked || !ui.forgetPlan;
});

$('forget-execute-btn').addEventListener('click', async () => {
  if (!ui.forgetPlan || !$('forget-confirm-check').checked) return;
  notice();
  try {
    const report = await api('/api/memory/forget/confirm', {
      method: 'POST',
      body: JSON.stringify({
        plan_id: ui.forgetPlan.plan_id,
        confirmed: true,
        verification_query: $('forget-verify-query').value.trim() || undefined,
        bot_id: $('identity-bot').value || 'muse',
      }),
    });
    ui.forgetPlan = null;
    $('forget-confirm-box').hidden = true;
    $('forget-report-output').textContent = JSON.stringify(report, null, 2);
  } catch (err) {
    notice(err.message);
  }
});

async function loadDiscretion() {
  notice();
  const chatId = $('discretion-chat-id').value.trim();
  if (!chatId) return;
  try {
    const pol = await api(`/api/side-chats/${encodeURIComponent(chatId)}/discretion`);
    ui.discretionSha = pol.sha256 || null;
    $('discretion-sha').textContent = `Loaded SHA-256: ${pol.sha256 || 'new'}`;
    $('discretion-defaults').textContent = (pol.default_rules || []).join('\n');
    $('discretion-rules').value = (pol.user_rules || []).join('\n');
  } catch (err) {
    notice(err.message);
  }
}

$('discretion-load-form').addEventListener('submit', event => {
  event.preventDefault();
  void loadDiscretion();
});

$('discretion-save-btn').addEventListener('click', () => {
  const chatId = $('discretion-chat-id').value.trim();
  if (!chatId) return;
  void mutate(
    $('discretion-save-btn'),
    `/api/side-chats/${encodeURIComponent(chatId)}/discretion`,
    {
      rules_text: $('discretion-rules').value,
      expected_sha256: ui.discretionSha,
      loosens: $('discretion-loosens').checked,
    },
    pol => {
      ui.discretionSha = pol.sha256 || null;
      $('discretion-sha').textContent = `Saved SHA-256: ${pol.sha256 || 'ok'}`;
    },
    'PUT',
  );
});

// Skills listeners
$('skill-search-form').addEventListener('submit', event => {
  event.preventDefault();
  void refreshTabData('skills');
});

async function inspectCatalog(kind, key) {
  notice();
  try {
    const res = await api(`/api/${kind}?load=${encodeURIComponent(key)}`);
    $('skill-detail-box').hidden = false;
    $('skill-detail-title').textContent = `${kind}: ${key}`;
    $('skill-detail-body').textContent = JSON.stringify(res, null, 2);
  } catch (err) {
    notice(err.message);
  }
}

$('skills-list').addEventListener('click', event => {
  const btn = event.target.closest('[data-load-skill]');
  if (btn) void inspectCatalog('skills', btn.dataset.loadSkill);
});

$('connectors-list').addEventListener('click', event => {
  const btn = event.target.closest('[data-load-connector]');
  if (btn) void inspectCatalog('connectors', btn.dataset.loadConnector);
});

$('custom-skill-form').addEventListener('submit', event => {
  event.preventDefault();
  const body = {
    name: $('custom-skill-name').value.trim(),
    title: $('custom-skill-title').value.trim(),
    description: $('custom-skill-desc').value.trim(),
    body_markdown: $('custom-skill-body').value,
  };
  void mutate(event.submitter, '/api/skills/custom', body, () => {
    event.target.reset();
    void refreshTabData('skills');
  });
});

document.addEventListener('visibilitychange', () => { if (!document.hidden) void refreshAll(); });
setInterval(() => { if (!document.hidden) void refreshAll(); }, 3000);
setTab(ui.tab);
void refreshAll();
