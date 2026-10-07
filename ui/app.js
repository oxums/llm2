/* LLM Proxy console — vanilla JS + Tailwind PlayCDN */
const S = {
  key: localStorage.getItem('llm_key') || '',
  view: 'overview',
  providers: [], aliases: {},
  catalog: { providers: {}, decisions: [], linked: [], providers_seen: [] },
  stats: null, selAlias: null, draft: null, draftName: null,
  clfTab: 'classifier',
};

const $ = (s, r) => (r || document).querySelector(s);
const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));
const esc = (s) => String(s == null ? '' : s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmt = (n) => n == null ? '—' : Number(n).toLocaleString('en-US');

async function api(path, opts) {
  opts = opts || {};
  opts.headers = Object.assign({ 'Authorization': 'Bearer ' + S.key, 'Content-Type': 'application/json' }, opts.headers || {});
  const r = await fetch(path, opts);
  if (r.status === 401) { logout(true); throw new Error('unauthorized'); }
  const t = await r.text();
  let j = null;
  try { j = t ? JSON.parse(t) : null; } catch (e) { j = { raw: t }; }
  if (!r.ok) {
    const m = (j && j.error && j.error.message) || ('HTTP ' + r.status);
    throw new Error(typeof m === 'string' ? m : JSON.stringify(m).slice(0, 300));
  }
  return j;
}

function toast(msg, kind) {
  const d = document.createElement('div');
  d.className = 'toast';
  d.style.borderColor = kind === 'err' ? 'rgba(244,63,94,.4)' : undefined;
  d.textContent = msg;
  $('#toasts').appendChild(d);
  setTimeout(() => { d.style.opacity = '0'; d.style.transition = '.3s'; setTimeout(() => d.remove(), 320); }, 3200);
}

/* ---------------- auth ---------------- */
async function checkAuth() {
  if (!S.key) return false;
  try { await api('/api/me'); return true; } catch (e) { return false; }
}
function showLogin() {
  $('#login').classList.remove('hidden'); $('#login').classList.add('flex');
  $('#app').classList.add('hidden');
}
function showApp() {
  $('#login').classList.add('hidden'); $('#login').classList.remove('flex');
  $('#app').classList.remove('hidden');
}
function logout(expired) {
  S.key = ''; localStorage.removeItem('llm_key');
  showLogin();
  if (expired) toast('Session expired — sign in again', 'err');
}
async function loginFlow() {
  const v = $('#login-key').value.trim() || 'admin';
  S.key = v;
  try {
    await api('/api/me');
    localStorage.setItem('llm_key', v);
    $('#login-err').classList.add('hidden');
    showApp(); await boot();
  } catch (e) {
    $('#login-err').textContent = 'Wrong key. Check API_KEY on the server.';
    $('#login-err').classList.remove('hidden');
    S.key = '';
  }
}

/* ---------------- data ---------------- */
async function loadAll() {
  const [stats, providers, aliases, catalog] = await Promise.all([
    api('/api/stats'), api('/api/providers'), api('/api/aliases'), api('/api/catalog'),
  ]);
  S.stats = stats; S.providers = providers; S.aliases = aliases || {}; S.catalog = catalog;
  if (!S.selAlias || !S.aliases[S.selAlias]) {
    const names = Object.keys(S.aliases).sort();
    S.selAlias = names[0] || null;
  }
  S.draft = null;
}
async function boot() {
  await loadAll();
  $('#sb-base').textContent = location.origin + '/v1';
  renderAll();
  setInterval(async () => {
    if (S.view !== 'overview') return;
    try { S.stats = await api('/api/stats'); renderOverview(); } catch (e) {}
  }, 5000);
}

/* ---------------- nav ---------------- */
function setView(v) {
  S.view = v;
  $$('.navit[data-view]').forEach((b) => b.classList.toggle('on', b.dataset.view === v));
  ['overview', 'providers', 'aliases'].forEach((x) => $('#view-' + x).classList.toggle('hidden', x !== v));
  $('#view-title').textContent = v[0].toUpperCase() + v.slice(1);
  if (v === 'overview') renderOverview();
  if (v === 'providers') renderProviders();
  if (v === 'aliases') renderAliases();
}
function renderAll() { setView(S.view); }

/* ---------------- overview ---------------- */
function setupSteps() {
  const linked = (S.catalog.linked || []).length;
  const aliases = Object.keys(S.aliases).length;
  const steps = [
    { done: linked > 0, t: 'Link a provider', d: linked ? linked + ' linked' : 'Add an API key in Providers' },
    { done: aliases > 0, t: 'Create an alias', d: aliases ? aliases + ' aliases' : 'Group models with a pick mode' },
    { done: (S.stats && S.stats.metrics.requests) > 0, t: 'Send traffic', d: 'Point an OpenAI client at /v1' },
  ];
  const n = steps.filter((s) => s.done).length;
  return { steps, n };
}
function renderOverview() {
  const m = (S.stats && S.stats.metrics) || {};
  const { steps, n } = setupSteps();
  const provRows = S.providers.map((p) =>
    '<div class="flex items-center gap-3 py-2.5 border-b border-[#1c1c21] last:border-0">' +
    '<span class="text-[13px] font-medium w-36 truncate">' + esc(p.id) + '</span>' +
    (p.linked ? '<span class="badge b-green"><span class="dot"></span>linked</span>' : '<span class="badge b-zinc"><span class="dot"></span>not linked</span>') +
    '<span class="ml-auto text-[12px] text-zinc-500">' + fmt(p.models_count) + ' models</span></div>').join('');
  const aliasRows = Object.keys(S.aliases).sort().map((n2) => {
    const a = S.aliases[n2];
    const kind = a.kind || 'chat';
    const kb = kind === 'chat' ? 'b-blue' : kind === 'embedding' ? 'b-violet' : 'b-amber';
    return '<div class="flex items-center gap-3 py-2.5 border-b border-[#1c1c21] last:border-0">' +
      '<button class="text-[13px] font-medium hover:text-white" data-go-alias="' + esc(n2) + '">' + esc(n2) + '</button>' +
      '<span class="badge ' + kb + '">' + esc(kind) + '</span>' +
      '<span class="badge b-zinc">' + esc(a.pick_mode || '') + '</span>' +
      '<span class="ml-auto text-[12px] text-zinc-500">' + Object.keys(a.models || {}).length + ' models</span></div>';
  }).join('') || '<div class="py-6 text-center text-[13px] text-zinc-600">No aliases yet — create one in the Aliases tab.</div>';

  $('#view-overview').innerHTML =
    '<div class="card mb-5"><div class="flex flex-col md:flex-row">' +
      '<div class="flex-1 p-5"><div class="text-[13.5px] font-semibold">⚡ Finish setting up your proxy</div>' +
      '<div class="text-[12.5px] text-zinc-500 mt-1">Link providers, create aliases, then send OpenAI-compatible traffic through <span class="code">/v1</span>.</div>' +
      '<div class="text-[12px] text-zinc-500 mt-3">' + n + ' of 3 steps complete</div>' +
      '<div class="h-1.5 rounded-full bg-[#232329] mt-2 overflow-hidden"><div class="h-full rounded-full bg-gradient-to-r from-emerald-400 to-sky-400" style="width:' + Math.round(n / 3 * 100) + '%"></div></div></div>' +
      '<div class="md:w-[290px] border-t md:border-t-0 md:border-l border-[#232329] p-5"><div class="text-[11px] uppercase tracking-wide text-zinc-500">Up next</div>' +
      '<div class="text-[13px] font-medium mt-1">' + esc((steps.find((s) => !s.done) || { t: 'All set 🎉' }).t) + '</div>' +
      '<div class="text-[12px] text-zinc-500">' + esc((steps.find((s) => !s.done) || { d: 'Proxy is ready for traffic.' }).d) + '</div>' +
      '<button class="btn btn-pri mt-3" data-go="providers">Continue setup</button></div>' +
    '</div></div>' +
    '<div class="flex items-center gap-2 mb-3"><h2 class="text-[15px] font-semibold">Overview</h2><div class="ml-auto flex gap-1.5">' +
      ['All', 'Chat', 'Decisions'].map((t) => '<span class="tab' + (t === 'All' ? ' on' : '') + '">' + t + '</span>').join('') + '</div></div>' +
    '<div class="grid grid-cols-2 lg:grid-cols-4 gap-3.5 mb-3.5">' +
      statCard('Requests', fmt(m.requests), 'total proxied') +
      statCard('Errors', fmt(m.errors), m.requests ? (100 * m.errors / Math.max(1, m.requests)).toFixed(1) + '% error rate' : 'no traffic yet') +
      statCard('Avg latency', (m.avg_latency_ms || 0) + ' ms', 'proxy overhead + upstream') +
      statCard('Tokens in / out', fmt(m.tokens_in) + ' / ' + fmt(m.tokens_out), 'prompt / completion') +
    '</div>' +
    '<div class="grid md:grid-cols-2 gap-3.5">' +
      '<div class="card"><div class="card-h"><div class="card-t">Providers</div><div class="card-d">' + (S.catalog.linked || []).length + ' of ' + S.providers.length + ' linked · ' + fmt(S.stats.models) + ' chat models · ' + fmt(S.stats.decision_models) + ' decision models</div></div><div class="card-p !pt-2">' + provRows + '</div></div>' +
      '<div class="card"><div class="card-h"><div class="card-t">Aliases</div><div class="card-d">Routing groups with pick modes</div></div><div class="card-p !pt-2">' + aliasRows + '</div></div>' +
    '</div>' +
    '<div class="card mt-3.5"><div class="card-h"><div class="card-t">Call the proxy</div><div class="card-d">Drop-in OpenAI base URL — swap the model for <span class="code">provider/model</span>, <span class="code">any/model</span> or an alias.</div></div>' +
    '<div class="card-p"><pre class="code text-[12px] leading-relaxed bg-[#0c0c0e] border border-[#232329] rounded-[10px] p-4 overflow-x-auto text-zinc-300">curl ' + esc(location.origin) + '/v1/chat/completions \\\n  -H "Authorization: Bearer $API_KEY" -H "Content-Type: application/json" \\\n  -d \'{"model": "fast", "messages": [{"role": "user", "content": "hi"}],\n       "prefer_models": ["groq/groq-compound-mini"]}\'</pre></div></div>';

  $$('[data-go]').forEach((b) => b.onclick = () => setView(b.dataset.go));
  $$('[data-go-alias]').forEach((b) => b.onclick = () => { S.selAlias = b.dataset.goAlias; S.draft = null; setView('aliases'); });
}
function statCard(t, v, d) {
  return '<div class="card"><div class="p-5"><div class="text-[12.5px] text-zinc-400">' + t + '</div>' +
    '<div class="text-[30px] font-semibold tracking-tight mt-1">' + v + '</div>' +
    '<div class="text-[11.5px] text-zinc-600 mt-1 flex items-center gap-1.5"><span class="w-1.5 h-1.5 rounded-full border border-sky-500 inline-block"></span>' + d + '</div></div></div>';
}

/* ---------------- providers ---------------- */
const KIND_BADGE = { chat: 'b-blue', decision: 'b-amber', unsupported: 'b-rose', custom: 'b-zinc' };
function renderProviders() {
  const cards = S.providers.map((p) => {
    const kb = KIND_BADGE[p.kind] || 'b-zinc';
    return '<div class="card"><div class="p-5">' +
      '<div class="flex items-center gap-2"><div class="text-[14.5px] font-semibold capitalize">' + esc(p.id) + '</div>' +
      '<span class="badge ' + kb + '">' + esc(p.kind || 'chat') + '</span></div>' +
      '<div class="code text-[11.5px] text-zinc-500 mt-1.5 truncate">' + esc(p.effective_base_url || '—') + '</div>' +
      '<div class="text-[12px] text-zinc-500 mt-1">' + fmt(p.models_count) + ' models in catalog</div>' +
      '<div class="flex items-center mt-4">' +
      (p.linked ? '<span class="badge b-green"><span class="dot"></span>Configured</span>' : '<span class="badge b-zinc"><span class="dot"></span>Not configured</span>') +
      '<button class="btn btn-sec ml-auto !text-[12px]" data-cfg="' + esc(p.id) + '">' + (p.linked ? 'Edit →' : 'Configure →') + '</button>' +
      '</div></div></div>';
  }).join('');
  $('#view-providers').innerHTML = '<div class="grid sm:grid-cols-2 xl:grid-cols-3 gap-3.5">' + cards + '</div>' +
    '<p class="text-[12px] text-zinc-600 mt-4">Keys stay server-side in <span class="code">config.json</span> (gitignored). Bedrock / Vertex / ElevenLabs are listed but unsupported in v1.</p>';
  $$('#view-providers [data-cfg]').forEach((b) => b.onclick = () => openProviderSheet(b.dataset.cfg));
}

function kvEditorHTML(rows) {
  return '<div id="kv-rows">' + rows.map((r) =>
    '<div class="kv-row"><input class="inp code" data-k="k" placeholder="HEADER-NAME" value="' + esc(r[0]) + '">' +
    '<input class="inp code" data-k="v" placeholder="value" value="' + esc(r[1]) + '">' +
    '<button class="btn btn-ghost !px-2.5" data-kv-del>✕</button></div>').join('') + '</div>' +
    '<button id="kv-add" class="btn btn-sec !text-[12px]">+ Add header</button>';
}
function kvRead() {
  const o = {};
  $$('#kv-rows .kv-row').forEach((r) => {
    const k = $('.inp[data-k="k"]', r).value.trim(), v = $('.inp[data-k="v"]', r).value;
    if (k) o[k] = v;
  });
  return o;
}
function kvWire() {
  $('#kv-add').onclick = () => {
    const d = document.createElement('div');
    d.className = 'kv-row';
    d.innerHTML = '<input class="inp code" data-k="k" placeholder="HEADER-NAME"><input class="inp code" data-k="v" placeholder="value"><button class="btn btn-ghost !px-2.5" data-kv-del>✕</button>';
    $('#kv-rows').appendChild(d); wireDel(d);
  };
  $$('#kv-rows [data-kv-del]').forEach((b) => b.onclick = () => b.closest('.kv-row').remove());
  function wireDel(r) { $('[data-kv-del]', r).onclick = () => r.remove(); }
}

function openProviderSheet(id) {
  const p = S.providers.find((x) => x.id === id) || {};
  const hdrs = Object.entries(p.extra_headers || {});
  openSheet(id, (p.kind || 'chat') + ' provider · ' + (p.effective_base_url || ''),
    '<div><label class="lbl">API key <span class="text-zinc-600">(blank = keep current)</span></label>' +
    '<input id="f-key" type="password" class="inp code" placeholder="' + (p.linked ? '•••••• (linked)' : 'sk-...') + '"></div>' +
    '<div><label class="lbl">Base URL <span class="text-zinc-600">(blank = default)</span></label>' +
    '<input id="f-base" class="inp code" placeholder="' + esc(p.effective_base_url || '') + '" value="' + esc(p.base_url || '') + '"></div>' +
    (id === 'azure-openai' ? '<div><label class="lbl">API version</label><input id="f-ver" class="inp code" placeholder="2024-10-01-preview" value="' + esc(p.api_version || '') + '"></div>' : '') +
    '<div><label class="lbl">Extra headers <span class="text-zinc-600">(per-request, e.g. HTTP-Referer for OpenRouter)</span></label>' + kvEditorHTML(hdrs) + '</div>' +
    '<div><label class="lbl">Enabled models <span class="text-zinc-600">(one per line, blank = all fetched)</span></label>' +
    '<textarea id="f-models" class="inp code" placeholder="gpt-4o-mini&#10;gpt-4o">' + esc((p.enabled_models || []).join('\n')) + '</textarea></div>',
    async () => {
      const patch = { extra_headers: kvRead() };
      const k = $('#f-key').value.trim(); if (k) patch.api_key = k;
      const b2 = $('#f-base').value.trim(); patch.base_url = b2 ? b2 : null;
      const mv = $('#f-models').value.split('\n').map((s) => s.trim()).filter(Boolean);
      patch.enabled_models = mv.length ? mv : null;
      if (id === 'azure-openai' && $('#f-ver')) patch.api_version = $('#f-ver').value.trim() || null;
      await api('/api/providers/' + encodeURIComponent(id), { method: 'PUT', body: JSON.stringify(patch) });
      toast(id + ' saved — catalog refreshing');
      await loadAll(); renderProviders();
    }, 'Save provider');
  kvWire();
}

/* ---------------- sheet infra ---------------- */
function openSheet(title, desc, bodyHTML, onSave, saveLabel) {
  $('#sheet-title').textContent = title;
  $('#sheet-desc').textContent = desc || '';
  $('#sheet-body').innerHTML = bodyHTML;
  $('#sheet-foot').innerHTML = '<button id="sheet-cancel" class="btn btn-ghost">Cancel</button>' +
    '<button id="sheet-save" class="btn btn-pri">' + esc(saveLabel || 'Save') + '</button>';
  $('#backdrop').classList.add('open');
  $('#sheet').classList.add('open');
  $('#sheet-cancel').onclick = closeSheet;
  $('#sheet-save').onclick = async () => {
    try { await onSave(); closeSheet(); }
    catch (e) { toast(e.message, 'err'); }
  };
}
function closeSheet() { $('#backdrop').classList.remove('open'); $('#sheet').classList.remove('open'); }

/* ---------------- aliases ---------------- */
function draftAlias() {
  if (S.draft && S.draftName === S.selAlias) return S.draft;
  const src = S.aliases[S.selAlias];
  S.draft = src ? JSON.parse(JSON.stringify(src)) : null;
  S.draftName = S.selAlias;
  return S.draft;
}
function draftKind(a) {
  const keys = Object.keys(a.models || {});
  if (!keys.length) return 'chat';
  const isDec = (k) => {
    if (k.startsWith('typesafe/')) return true;
    const s = k.startsWith('openrouter/') ? k.slice(11) : k;
    return (S.catalog.decisions || []).some((d) => d.slug === s || (d.source + '/' + d.slug) === k);
  };
  if (keys.every(isDec)) return 'decision';
  const isEmb = (k) => {
    const m = k.match(/^([^\/:]+)[\/:](.+)$/);
    if (!m) return false;
    const grp = (S.catalog.providers || {})[m[1].toLowerCase()] || [];
    const hit = grp.find((x) => x.id === m[2]);
    if (hit) return !!hit.embedding;
    return /embed/i.test(m[2]);
  };
  if (keys.every(isEmb)) return 'embedding';
  return 'chat';
}
function chatModelOptions() {
  const o = [];
  Object.entries(S.catalog.providers || {}).forEach(([p, arr]) => {
    arr.forEach((m) => { if (!m.embedding) o.push(p + '/' + m.id); });
  });
  return o.sort();
}
function decisionOptions() {
  return (S.catalog.decisions || []).map((d) => d.source + '/' + d.slug).sort();
}
function capsBadges(a, key) {
  const m = (a.models || {})[key] || {};
  const ov = m.capabilities_override || {};
  const eff = (cap) => {
    if (ov[cap] === true) return true;
    if (ov[cap] === false) return false;
    const mm = key.match(/^([^\/:]+)[\/:](.+)$/);
    if (!mm) return false;
    const grp = (S.catalog.providers || {})[mm[1].toLowerCase()] || [];
    const hit = grp.find((x) => x.id === mm[2]);
    return hit ? !!hit[cap] : false;
  };
  const icons = { image: '🖼', audio: '🔊', video: '🎬', pdf: '📄' };
  return ['image', 'audio', 'video', 'pdf'].filter((c) => eff(c))
    .map((c) => '<span class="badge b-blue" title="' + c + '">' + icons[c] + '</span>').join('');
}

function renderAliases() {
  const names = Object.keys(S.aliases).sort();
  if (!names.length) {
    $('#view-aliases').innerHTML = '<div class="card"><div class="p-10 text-center">' +
      '<div class="text-[15px] font-semibold">No aliases yet</div>' +
      '<div class="text-[13px] text-zinc-500 mt-1">Group provider models behind one name with a pick mode.</div>' +
      '<button id="al-new0" class="btn btn-pri mt-4">+ New alias</button></div></div>';
    $('#al-new0').onclick = newAlias;
    return;
  }
  if (!S.selAlias || !S.aliases[S.selAlias]) S.selAlias = names[0];
  const a = draftAlias();
  const kind = draftKind(a);
  const kb = kind === 'chat' ? 'b-blue' : kind === 'embedding' ? 'b-violet' : 'b-amber';

  const pills = names.map((n) => {
    const aa = n === S.selAlias ? S.draft : S.aliases[n];
    const k = n === S.selAlias ? kind : draftKind(aa);
    return '<button class="pill' + (n === S.selAlias ? ' on' : '') + '" data-al="' + esc(n) + '">' + esc(n) +
      ' <span class="badge ' + (k === 'chat' ? 'b-blue' : k === 'embedding' ? 'b-violet' : 'b-amber') + ' !text-[10px]">' + k.slice(0, 3) + '</span></button>';
  }).join('');

  const members = Object.keys(a.models || {}).sort();
  const showOrder = a.pick_mode === 'custom_order' || a.pick_mode === 'failover';
  const order = (a.order && a.order.length ? a.order.filter((o) => members.includes(o)) : members.slice())
    .concat(members.filter((m) => !(a.order || []).includes(m)));
  const rows = order.map((k) => {
    const m = a.models[k];
    return '<div class="flex items-center gap-2 py-2.5 border-b border-[#1c1c21] last:border-0">' +
      (showOrder ? '<span class="flex flex-col gap-0.5"><button class="text-zinc-600 hover:text-white text-[11px] leading-none" data-mv="-1:' + esc(k) + '">▲</button><button class="text-zinc-600 hover:text-white text-[11px] leading-none" data-mv="1:' + esc(k) + '">▼</button></span>' : '') +
      '<div class="min-w-0"><div class="code text-[12.5px] text-zinc-100 truncate">' + esc(k) + '</div>' +
      '<div class="flex gap-1.5 mt-1">' + (m.enabled === false ? '<span class="badge b-zinc">disabled</span>' : '') + capsBadges(a, k) +
      (Object.keys(m.extra_headers || {}).length ? '<span class="badge b-zinc">headers</span>' : '') +
      (m.option_description ? '<span class="badge b-zinc">described</span>' : '') + '</div></div>' +
      '<div class="ml-auto flex items-center gap-1.5 flex-none">' +
      '<button class="btn btn-ghost !text-[12px]" data-hdrs="' + esc(k) + '">Edit headers →</button>' +
      '<button class="btn btn-ghost !text-[12px]" data-set="' + esc(k) + '">Settings →</button>' +
      '<div class="switch' + (m.enabled === false ? '' : ' on') + '" data-tgl="' + esc(k) + '"></div>' +
      '<button class="btn btn-ghost !px-2 !text-[12px]" data-rm="' + esc(k) + '">✕</button>' +
      '</div></div>';
  }).join('') || '<div class="py-6 text-center text-[13px] text-zinc-600">No models — add one below.</div>';

  const addOpts = kind === 'decision' ? decisionOptions() : chatModelOptions();
  const pm = a.pick_mode || 'failover';
  const pmBtn = (v, t) => '<button class="tab flex-1 !text-center' + (pm === v ? ' on' : '') + '" data-pm="' + v + '">' + t + '</button>';

  $('#view-aliases').innerHTML =
    '<div class="flex items-center gap-2 mb-4 flex-wrap" id="al-pills">' + pills +
    '<button id="al-new" class="btn btn-sec !px-3 ml-auto">+</button></div>' +
    '<div class="grid lg:grid-cols-[1fr_320px] gap-3.5 items-start">' +
      '<div class="card"><div class="card-h flex items-center gap-2"><div><div class="card-t flex items-center gap-2">' +
      '<input id="al-name" class="inp !w-44 !py-1 !text-[13.5px] font-semibold" value="' + esc(S.selAlias) + '">' +
      '<span class="badge ' + kb + '">' + kind + '</span></div>' +
      '<div class="card-d">Models in this alias · ' + (kind === 'decision' ? 'decisions-only' : kind === 'embedding' ? 'embeddings-only' : 'chat') + '</div></div>' +
      '<div class="ml-auto flex gap-2"><button id="al-del" class="btn btn-danger !text-[12px]">Delete</button>' +
      '<button id="al-save" class="btn btn-pri !text-[12px]">Save alias</button></div></div>' +
      '<div class="card-p !pt-3">' + rows +
      '<div class="flex gap-2 mt-3"><input id="al-add" list="dl-models" class="inp code" placeholder="' + (kind === 'decision' ? 'openrouter/cloudflare/clef' : 'openai/gpt-4o-mini') + '">' +
      '<datalist id="dl-models">' + addOpts.slice(0, 2000).map((o) => '<option value="' + esc(o) + '">').join('') + '</datalist>' +
      '<button id="al-addbtn" class="btn btn-sec flex-none">Add</button></div>' +
      (kind === 'decision' && !addOpts.length ? '<p class="text-[12px] text-amber-400/90 mt-2">No decision models: link <b>typesafe</b> and/or <b>openrouter</b> in Providers, then ↻ Refresh catalog.</p>' : '') +
      '</div></div>' +
      '<div class="card"><div class="card-h"><div class="card-t">Pick mode</div><div class="card-d">How a member is chosen per request</div></div>' +
      '<div class="card-p space-y-3"><div class="flex gap-1 bg-[#0c0c0e] border border-[#232329] rounded-[10px] p-1">' +
        pmBtn('round_robin', 'Round Robin') + pmBtn('classifier', 'Classifier') + pmBtn('custom_order', 'Custom Order') + '</div>' +
        '<div id="pm-body">' + pmBody(a, kind) + '</div>' +
        (kind === 'chat' ? spoofHTML(a) : '') +
      '</div></div>' +
    '</div>';

  // wire
  $$('#al-pills [data-al]').forEach((b) => b.onclick = () => { S.selAlias = b.dataset.al; S.draft = null; renderAliases(); });
  $('#al-new').onclick = newAlias;
  $('#al-del').onclick = async () => {
    if (!confirm('Delete alias ' + S.selAlias + '?')) return;
    await api('/api/aliases/' + encodeURIComponent(S.selAlias), { method: 'DELETE' });
    S.selAlias = null; S.draft = null; toast('Alias deleted'); await loadAll(); renderAliases();
  };
  $('#al-save').onclick = saveAlias;
  $('#al-name').onchange = (e) => { renameAlias(e.target.value.trim()); };
  $$('#view-aliases [data-pm]').forEach((b) => b.onclick = () => {
    const v = b.dataset.pm;
    S.draft.pick_mode = v === 'custom_order' ? 'custom_order' : v;
    if (v === 'custom_order' && !(S.draft.order || []).length) S.draft.order = Object.keys(S.draft.models || {}).sort();
    renderAliases();
  });
  $$('#view-aliases [data-tgl]').forEach((b) => b.onclick = () => {
    const m = S.draft.models[b.dataset.tgl]; m.enabled = m.enabled === false ? true : false; renderAliases();
  });
  $$('#view-aliases [data-rm]').forEach((b) => b.onclick = () => {
    delete S.draft.models[b.dataset.rm];
    S.draft.order = (S.draft.order || []).filter((o) => o !== b.dataset.rm);
    renderAliases();
  });
  $$('#view-aliases [data-mv]').forEach((b) => b.onclick = () => {
    const [d, k] = b.dataset.mv.split(/:(.+)/).filter(Boolean);
    const dir = parseInt(d, 10);
    if (!(S.draft.order || []).length) S.draft.order = Object.keys(S.draft.models).sort();
    const o = S.draft.order, i = o.indexOf(k), j = i + dir;
    if (i < 0 || j < 0 || j >= o.length) return;
    [o[i], o[j]] = [o[j], o[i]]; renderAliases();
  });
  $$('#view-aliases [data-hdrs]').forEach((b) => b.onclick = () => openHeadersSheet(b.dataset.hdrs));
  $$('#view-aliases [data-set]').forEach((b) => b.onclick = () => openMemberSheet(b.dataset.set));
  $('#al-addbtn').onclick = () => {
    const v = $('#al-add').value.trim();
    if (!v) return;
    if (!/^[^\/:]+\/[^\/]+/.test(v) && !/^[^\/:]+:[^:]+/.test(v)) { toast('Use provider/model format', 'err'); return; }
    S.draft.models[v] = S.draft.models[v] || { enabled: true, extra_headers: {}, extra_body: null, capabilities_override: {}, option_description: null, embed_dimensions: null };
    renderAliases();
  };
  wirePmBody(a, kind);
  wireSpoof();
}

function pmBody(a, kind) {
  if (kind !== 'chat') return '<p class="text-[12.5px] text-zinc-500">Failover order' + (a.pick_mode === 'round_robin' ? ' with round-robin start' : '') + '. Use ▲▼ on members' + (a.pick_mode === 'round_robin' ? '' : ' to set explicit order') + '. Cooldown 60s on 429/5xx.</p>' +
    (a.pick_mode === 'failover' ? '<p class="text-[12px] text-zinc-600 mt-1">Tries members in order; skips cooled-down ones unless preferred with bypass.</p>' : '');
  if (a.pick_mode === 'classifier') {
    const c = a.classifier || {};
    const be = c.backend || 'jev';
    const modelOpts = be === 'jev' ? decisionOptions() : chatModelOptions();
    const members = Object.keys(a.models || {}).sort();
    return '<div class="space-y-3">' +
      '<div><label class="lbl">Backend</label><select id="c-be" class="inp">' +
      '<option value="jev"' + (be === 'jev' ? ' selected' : '') + '>Jev / decision model (Choice)</option>' +
      '<option value="text_structured"' + (be === 'text_structured' ? ' selected' : '') + '>Text model (structured output)</option></select></div>' +
      '<div><label class="lbl">Model</label><select id="c-model" class="inp code">' +
      modelOpts.map((o) => '<option' + (o === c.model ? ' selected' : '') + '>' + esc(o) + '</option>').join('') + '</select>' +
      (!modelOpts.length ? '<p class="text-[12px] text-amber-400/90 mt-1">No decision models — link typesafe/openrouter + refresh.</p>' : '') + '</div>' +
      '<div><label class="lbl">Fallback</label><select id="c-fb" class="inp code"><option value="">(head of order)</option>' +
      members.map((m) => '<option' + (m === c.fallback ? ' selected' : '') + '>' + esc(m) + '</option>').join('') + '</select></div>' +
      '<div><label class="lbl">Prompt</label><textarea id="c-prompt" class="inp">' + esc(c.prompt || 'Which model should handle this request?') + '</textarea>' +
      '<p class="text-[11.5px] text-zinc-600 mt-1">Edit the single options description in their settings.</p></div>' +
      '<div><label class="lbl">Min confidence <span id="c-conf-v" class="code text-zinc-300">' + esc(String(c.min_confidence != null ? c.min_confidence : 0.5)) + '</span></label>' +
      '<input id="c-conf" type="range" min="0" max="1" step="0.05" value="' + esc(String(c.min_confidence != null ? c.min_confidence : 0.5)) + '" class="w-full"></div>' +
      '</div>';
  }
  if (a.pick_mode === 'round_robin') return '<p class="text-[12.5px] text-zinc-500">Rotates across healthy members; failed ones cool down for 60s. Use ▲▼ to set the rotation start order.</p>';
  return '<p class="text-[12.5px] text-zinc-500">Tries members in order — uses a provider till it fails, cools it 60s, moves on. Retries immediately if everything is cooling. Use ▲▼ to set the order.</p>';
}
function spoofHTML(a) {
  const g = (k) => {
    const v = (a.capabilities_spoof || {})[k] || 'auto';
    return '<label class="text-[11.5px] text-zinc-500">' + k + '</label><select data-spoof="' + k + '" class="inp !py-1 !text-[12px]">' +
      ['auto', 'on', 'off'].map((o) => '<option' + (o === v ? ' selected' : '') + '>' + o + '</option>').join('') + '</select>';
  };
  return '<div><label class="lbl">Advertised capabilities <span class="text-zinc-600">(spoofed /v1/models)</span></label>' +
    '<div class="grid grid-cols-4 gap-2">' + g('image') + g('audio') + g('video') + g('pdf') + '</div></div>';
}
function wirePmBody(a, kind) {
  if (!$('#c-be')) return;
  $('#c-be').onchange = (e) => { a.classifier.backend = e.target.value; renderAliases(); };
  $('#c-model').onchange = (e) => { a.classifier.model = e.target.value; };
  $('#c-fb').onchange = (e) => { a.classifier.fallback = e.target.value; };
  $('#c-prompt').onchange = (e) => { a.classifier.prompt = e.target.value; };
  $('#c-conf').oninput = (e) => { a.classifier.min_confidence = parseFloat(e.target.value); $('#c-conf-v').textContent = e.target.value; };
}
function wireSpoof() {
  $$('#view-aliases [data-spoof]').forEach((s) => s.onchange = () => {
    S.draft.capabilities_spoof = S.draft.capabilities_spoof || {};
    S.draft.capabilities_spoof[s.dataset.spoof] = s.value;
  });
}
function newAlias() {
  let i = 1, n = 'alias';
  while (S.aliases[n]) { i++; n = 'alias-' + i; }
  S.aliases[n] = { models: {}, pick_mode: 'failover', order: [], capabilities_spoof: {}, classifier: { backend: 'jev', model: 'typesafe/jev-latest', fallback: '', prompt: 'Which model should handle this request?', min_confidence: 0.5, timeout_ms: 15000 } };
  S.selAlias = n; S.draft = JSON.parse(JSON.stringify(S.aliases[n])); S.draftName = n;
  renderAliases();
  setTimeout(() => { const el = $('#al-name'); if (el) { el.focus(); el.select(); } }, 50);
}
async function renameAlias(n) {
  if (!n || n === S.selAlias) { renderAliases(); return; }
  if (S.aliases[n]) { toast('Name taken', 'err'); renderAliases(); return; }
  try {
    await api('/api/aliases/' + encodeURIComponent(n), { method: 'PUT', body: JSON.stringify(S.draft) });
    await api('/api/aliases/' + encodeURIComponent(S.selAlias), { method: 'DELETE' });
    delete S.aliases[S.selAlias]; S.selAlias = n; S.draft = null;
    toast('Alias renamed'); await loadAll(); renderAliases();
  } catch (e) { toast(e.message, 'err'); }
}
async function saveAlias() {
  try {
    await api('/api/aliases/' + encodeURIComponent(S.selAlias), { method: 'PUT', body: JSON.stringify(S.draft) });
    toast('Alias saved'); await loadAll(); renderAliases();
  } catch (e) { toast(e.message, 'err'); }
}

function openHeadersSheet(key) {
  const m = S.draft.models[key];
  openSheet('Headers · ' + key, 'Merged per request (member wins over provider). Useful for OpenRouter attribution, Anthropic betas, Vertex tiers.',
    '<div><label class="lbl">Extra headers</label>' + kvEditorHTML(Object.entries(m.extra_headers || {})) + '</div>' +
    '<div><label class="lbl">Extra body (JSON object, e.g. {"service_tier":"fast"})</label>' +
    '<textarea id="m-body" class="inp code">' + esc(m.extra_body ? JSON.stringify(m.extra_body) : '') + '</textarea></div>',
    async () => {
      m.extra_headers = kvRead();
      const t = $('#m-body').value.trim();
      m.extra_body = t ? JSON.parse(t) : null;
      renderAliases(); toast('Headers staged — Save alias to persist');
    }, 'Stage headers');
  kvWire();
}
function segHTML(name, val) {
  // val: undefined|null=inherit, true=on, false=off
  const cur = val === true ? 'on' : val === false ? 'off' : 'inherit';
  return '<div class="flex gap-1" data-seg="' + name + '">' +
    ['inherit', 'on', 'off'].map((o) => '<button class="tab !text-[11.5px] !py-1 !px-2.5' + (o === cur ? ' on' : '') + '" data-segv="' + o + '">' + o + '</button>').join('') + '</div>';
}
function openMemberSheet(key) {
  const m = S.draft.models[key];
  const ov = m.capabilities_override || {};
  openSheet('Settings · ' + key, 'Per-model behavior inside this alias.',
    '<div class="flex items-center gap-2.5"><div class="switch' + (m.enabled === false ? '' : ' on') + '" id="m-en"></div><span class="text-[13px]">Enabled in alias</span></div>' +
    '<div><label class="lbl">Option description <span class="text-zinc-600">(classifier criteria text)</span></label>' +
    '<textarea id="m-desc" class="inp" placeholder="Cheap fast generalist. Good for simple QA.">' + esc(m.option_description || '') + '</textarea></div>' +
    '<div><label class="lbl">Capability overrides <span class="text-zinc-600">(inherit = from catalog)</span></label>' +
    '<div class="grid grid-cols-2 gap-2">' +
    ['image', 'audio', 'video', 'pdf'].map((c) => '<div class="flex items-center justify-between gap-2"><span class="text-[12.5px]">' + c + '</span>' + segHTML(c, ov[c]) + '</div>').join('') +
    '</div></div>' +
    '<div><label class="lbl">Embed dimensions <span class="text-zinc-600">(embedding aliases)</span></label>' +
    '<input id="m-dim" type="number" class="inp code" placeholder="auto" value="' + esc(m.embed_dimensions != null ? m.embed_dimensions : '') + '"></div>',
    async () => {
      m.enabled = $('#m-en').classList.contains('on') ? true : false;
      const d = $('#m-desc').value.trim(); m.option_description = d ? d : null;
      const dim = $('#m-dim').value.trim(); m.embed_dimensions = dim ? parseInt(dim, 10) : null;
      m.capabilities_override = {};
      $$('#sheet-body [data-seg]').forEach((g) => {
        const on = $('.tab.on', g);
        const v = on ? on.dataset.segv : 'inherit';
        if (v === 'on') m.capabilities_override[g.dataset.seg] = true;
        if (v === 'off') m.capabilities_override[g.dataset.seg] = false;
      });
      renderAliases(); toast('Settings staged — Save alias to persist');
    }, 'Stage settings');
  $('#m-en').onclick = (e) => e.target.classList.toggle('on');
  $$('#sheet-body [data-seg] .tab').forEach((b) => b.onclick = () => {
    $$('.tab', b.closest('[data-seg]')).forEach((x) => x.classList.remove('on'));
    b.classList.add('on');
  });
}

/* ---------------- boot ---------------- */
document.addEventListener('DOMContentLoaded', async () => {
  $$('.navit[data-view]').forEach((b) => b.onclick = () => setView(b.dataset.view));
  $('#login-btn').onclick = loginFlow;
  $('#login-key').addEventListener('keydown', (e) => { if (e.key === 'Enter') loginFlow(); });
  $('#btn-logout').onclick = () => logout(false);
  $('#btn-reload').onclick = async () => {
    try { await api('/api/reload', { method: 'POST' }); await loadAll(); renderAll(); toast('Catalog refreshed'); }
    catch (e) { toast(e.message, 'err'); }
  };
  $('#btn-copy-base').onclick = () => {
    navigator.clipboard.writeText(location.origin + '/v1').then(() => toast('Base URL copied'));
  };
  $('#sheet-close').onclick = closeSheet;
  $('#backdrop').onclick = closeSheet;
  if (await checkAuth()) { showApp(); await boot(); } else showLogin();
});
