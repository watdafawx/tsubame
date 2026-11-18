/*
  U4 shared builders + command palette. Needs mock.js first (uses Tsu.win and Tsu internals).
  Plain DOM strings. Class names map to draw calls, see mock-u4.css.

  Tsu4.palette(state, w, h) -> html of the main window with the palette open
     state: { mode:'cmd'|'path'|'filter'|'tabs'|'fresh'|'none', q:'rn', hot:0 }
  Tsu4.cmds                  -> command table (also used by the Keys page)
*/
(function () {
  'use strict';
  var T = window.TsuInternals, esc = T.esc;

  /* ---------- extra 16 px glyphs (same stroke style as mock.js) ---------- */
  var X = {
    gear: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v1.8M8 12.4v1.8M1.8 8h1.8M12.4 8h1.8M3.6 3.6l1.3 1.3M11.1 11.1l1.3 1.3M12.4 3.6l-1.3 1.3M4.9 11.1l-1.3 1.3"/>',
    keyboard: '<rect x="1.5" y="4" width="13" height="8" rx="1.2"/><path d="M4 7h.1M6.5 7h.1M9 7h.1M11.5 7h.1M5 9.8h6" stroke-width="1.6"/>',
    cmd: '<path d="M5.5 3.5a2 2 0 1 0-2 2h9a2 2 0 1 0-2-2v9a2 2 0 1 0 2-2h-9a2 2 0 1 0 2 2z"/>',
    eye: '<path d="M1.5 8S4 3.7 8 3.7 14.5 8 14.5 8 12 12.3 8 12.3 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="1.9"/>',
    eyeoff: '<path d="M1.5 8S4 3.7 8 3.7 14.5 8 14.5 8 12 12.3 8 12.3 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="1.9"/><path d="M2.5 13.5 13.5 2.5"/>',
    grip: '<path d="M6 3.5h.1M10 3.5h.1M6 8h.1M10 8h.1M6 12.5h.1M10 12.5h.1" stroke-width="2.2"/>',
    check: '<path d="M3 8.5 6.5 12 13 4.5"/>',
    warn: '<path d="M8 2.2 14.5 13.5h-13zM8 6.6v3.1M8 11.7v.1"/>',
    refresh: '<path d="M13 8a5 5 0 1 1-1.6-3.7M13 2.5v3h-3"/>',
    shield: '<path d="M8 1.8 13 3.6v4c0 3-2.2 5.3-5 6.6-2.8-1.3-5-3.6-5-6.6v-4zM5.8 8l1.6 1.6L10.5 6.5"/>',
    split: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M8 3v10"/>',
    sliders: '<path d="M2.5 5h5M11.5 5h2M2.5 11h2M8.5 11h5"/><circle cx="9.5" cy="5" r="1.7"/><circle cx="6.5" cy="11" r="1.7"/>',
    brush: '<path d="M3 13c0-1.6 1-2.6 2.3-2.6L13 2.8l1.2 1.2-7.6 7.7c0 1.3-1 2.3-2.6 2.3z"/>',
    hash: '<path d="M6.5 2.5 5.5 13.5M11 2.5 10 13.5M3 6h10.5M2.5 10h10.5"/>',
    at: '<circle cx="8" cy="8" r="2.4"/><path d="M10.4 8v1.1a1.8 1.8 0 0 0 3.6 0V8a6 6 0 1 0-2.4 4.8"/>',
    slash: '<path d="M10.5 2.5 5.5 13.5"/>',
    gt: '<path d="M3.5 4.5 8 8l-4.5 3.5M9.5 12h3.5"/>',
    dots: '<path d="M3.5 8h.1M8 8h.1M12.5 8h.1" stroke-width="2.2"/>',
    clock: '<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.3 1.5"/>',
    undo: '<path d="M5 3.5 2 6.5l3 3M2.5 6.5H10a3.5 3.5 0 0 1 0 7H7"/>',
    redo: '<path d="M11 3.5l3 3-3 3M13.5 6.5H6a3.5 3.5 0 0 0 0 7h3"/>',
    selall: '<rect x="2.5" y="2.5" width="11" height="11" rx="1" stroke-dasharray="2 2"/>',
    newfolder: '<path d="M2 13V4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V13zM8 7.5v4M6 9.5h4"/>',
    newfile: '<path d="M3.5 1.8h6L13 5.3v8.9H3.5zM9.5 1.8v3.5H13M8 7.5v4M6 9.5h4"/>',
    theme: '<circle cx="8" cy="8" r="5.5"/><path class="f" d="M8 2.5a5.5 5.5 0 0 1 0 11z"/>',
    dense: '<path d="M2.5 4h11M2.5 8h11M2.5 12h11"/>',
    zebra: '<rect x="2" y="3" width="12" height="3" rx=".5"/><rect x="2" y="10" width="12" height="3" rx=".5"/>',
    hidden: '<path d="M2 13V4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V13z" stroke-dasharray="2 1.6"/>',
    term: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M4.5 6.5 7 8.5 4.5 10.5M8.5 10.5h3"/>',
    quit: '<path d="M6.5 2.5h-3v11h3M10 5.5 12.5 8 10 10.5M12.5 8H6"/>',
    zoom: '<circle cx="7" cy="7" r="4"/><path d="M10 10l3.5 3.5M5.3 7h3.4M7 5.3v3.4"/>',
    sort: '<path d="M4.5 3v10M2.5 11l2 2 2-2M11.5 13V3M9.5 5l2-2 2 2"/>',
    preview: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M9.5 3v10"/>',
    archive: '<rect x="2.5" y="2.5" width="11" height="3" rx=".6"/><path d="M3.5 5.5v8h9v-8M7 8.5h2"/>',
    rollback: '<path d="M3 8a5 5 0 1 1 1.6 3.7M3 13.5v-3h3"/>',
    folder: '<path d="M2 13V4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V13z"/>',
    plug: '<path d="M6 2.5v3M10 2.5v3M4.5 5.5h7v2.2a3.5 3.5 0 0 1-7 0zM8 11.2v2.3"/>',
    db: '<ellipse cx="8" cy="4" rx="5" ry="2"/><path d="M3 4v8c0 1.1 2.2 2 5 2s5-.9 5-2V4M3 8c0 1.1 2.2 2 5 2s5-.9 5-2"/>',
    wrench: '<path d="M10.5 2.2a3.3 3.3 0 0 0-3 4.5L2.5 11.7l1.8 1.8 5-5a3.3 3.3 0 0 0 4.5-3l-2 2-1.8-.3-.3-1.8z"/>',
    lock: '<rect x="3" y="7" width="10" height="6.5" rx="1"/><path d="M5.2 7V5a2.8 2.8 0 0 1 5.6 0v2"/>',
    explorer: '<path d="M2 13V4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V13z"/><path d="M5 10h6"/>'
  };
  Object.keys(X).forEach(function (k) { T.IC[k] = X[k]; });
  var ic = T.ic;

  /* ---------- small widgets (one fill/stroke/text each) ---------- */
  function kc(k, cls) { return '<span class="kcap ' + (cls || '') + '">' + esc(k) + '</span>'; }
  function keys(combo, cls) {
    if (!combo) return '<span class="keys"><span class="kcap off">unbound</span></span>';
    return '<span class="keys">' + combo.split('+').map(function (k) { return kc(k, cls); }).join('') + '</span>';
  }
  function tog(on, cls) { return '<span class="tog ' + (on ? 'on' : 'off') + ' ' + (cls || '') + '"><i></i></span>'; }
  function seg(items, cur, hover) {
    return '<span class="segc">' + items.map(function (s, i) { return '<span class="' + (i === cur ? 'on' : '') + (i === hover ? ' hover' : '') + '">' + s + '</span>'; }).join('') + '</span>';
  }
  function btn(label, cls, icon) { return '<span class="bt ' + (cls || '') + '">' + (icon ? ic(icon, 14) : '') + label + '</span>'; }
  function field(text, cls, o) {
    o = o || {};
    return '<span class="fld ' + (cls || '') + '" style="' + (o.w ? 'width:' + o.w + 'px' : '') + '">' + (o.icon ? ic(o.icon, 14) : '') +
      (text || o.caret ? '<span class="v t">' + esc(text) + (o.caret ? '<span class="caret"></span>' : '') + '</span>' : '') +
      (o.ph ? '<span class="ph t">' + esc(o.ph) + '</span>' : '') + (o.tail || '') + '</span>';
  }
  function dd(text, open, items, curIdx, w) {
    var h = '<span class="ddw"><span class="fld dd ' + (open ? 'open' : '') + '" style="' + (w ? 'width:' + w + 'px' : '') + '"><span class="v t">' + esc(text) + '</span>' + ic('chevd', 12) + '</span>';
    if (open && items) {
      h += '<div class="popup">' + items.map(function (s, i) {
        return '<div class="mi ' + (i === curIdx ? 'hot' : '') + '">' + (i === curIdx ? ic('check', 14) : '<span class="blank"></span>') + '<span class="t">' + esc(s) + '</span></div>';
      }).join('') + '</div>';
    }
    return h + '</span>';
  }
  function slider(pct, cls) { return '<span class="sld ' + (cls || '') + '"><i style="width:' + pct + '%"></i><b style="left:' + pct + '%"></b></span>'; }
  function chk(on) { return '<span class="chk ' + (on ? 'on' : '') + '">' + (on ? ic('check', 12) : '') + '</span>'; }

  /* ---------- command table ---------- */
  // [id, name, category, icon, default keys, (optional) custom]
  var CMDS = [
    ['file.newfolder', 'New folder', 'File', 'newfolder', 'Ctrl+Shift+N'],
    ['file.newfile', 'New file', 'File', 'newfile', 'Ctrl+Alt+N'],
    ['file.open', 'Open', 'File', 'open', 'Enter'],
    ['file.opentab', 'Open in new tab', 'File', 'newtab', 'Ctrl+Enter'],
    ['file.rename', 'Rename', 'File', 'rename', 'F2'],
    ['file.delete', 'Delete', 'File', 'trash', 'Del'],
    ['file.deleteperm', 'Delete permanently', 'File', 'trash', 'Shift+Del'],
    ['file.props', 'Properties', 'File', 'info', 'Alt+Enter'],
    ['file.copypath', 'Copy as path', 'File', 'path', 'Ctrl+Shift+C'],
    ['edit.cut', 'Cut', 'Edit', 'cut', 'Ctrl+X'],
    ['edit.copy', 'Copy', 'Edit', 'copy', 'Ctrl+C'],
    ['edit.paste', 'Paste', 'Edit', 'paste', 'Ctrl+V'],
    ['edit.undo', 'Undo', 'Edit', 'undo', 'Ctrl+Z'],
    ['edit.redo', 'Redo', 'Edit', 'redo', 'Ctrl+Y'],
    ['edit.selall', 'Select all', 'Edit', 'selall', 'Ctrl+A'],
    ['edit.invert', 'Invert selection', 'Edit', 'selall', null],
    ['nav.back', 'Back', 'Navigate', 'back', 'Alt+Left'],
    ['nav.fwd', 'Forward', 'Navigate', 'fwd', 'Alt+Right'],
    ['nav.up', 'Up one folder', 'Navigate', 'up', 'Alt+Up'],
    ['nav.goto', 'Go to path', 'Navigate', 'slash', 'Ctrl+L'],
    ['nav.home', 'Go to Home', 'Navigate', 'home', 'Alt+Home'],
    ['nav.refresh', 'Refresh', 'Navigate', 'refresh', 'F5'],
    ['tab.new', 'New tab', 'Tabs', 'newtab', 'Ctrl+T'],
    ['tab.close', 'Close tab', 'Tabs', 'x', 'Ctrl+W'],
    ['tab.reopen', 'Reopen closed tab', 'Tabs', 'undo', 'Ctrl+Shift+T'],
    ['tab.next', 'Next tab', 'Tabs', 'chev', 'Ctrl+Tab'],
    ['tab.prev', 'Previous tab', 'Tabs', 'back', 'Ctrl+Shift+Tab'],
    ['tab.dup', 'Duplicate tab', 'Tabs', 'copy', null],
    ['pane.toggle', 'Toggle dual pane', 'Panes', 'split', 'F3'],
    ['pane.swap', 'Swap panes', 'Panes', 'split', 'Ctrl+U'],
    ['pane.copy', 'Copy to other pane', 'Panes', 'copy', null],
    ['pane.move', 'Move to other pane', 'Panes', 'cut', null],
    ['view.hidden', 'Toggle hidden files', 'View', 'hidden', 'Ctrl+H'],
    ['view.zebra', 'Toggle zebra rows', 'View', 'zebra', 'Ctrl+Shift+Z'],
    ['view.density', 'Cycle row density', 'View', 'dense', 'Ctrl+Shift+D'],
    ['view.theme', 'Toggle light/dark theme', 'View', 'theme', 'Ctrl+Shift+L'],
    ['view.preview', 'Toggle preview pane', 'View', 'preview', 'Alt+P'],
    ['view.zoomin', 'Zoom in', 'View', 'zoom', 'Ctrl+='],
    ['view.sort', 'Sort by\u2026', 'View', 'sort', null],
    ['find.filter', 'Filter this folder', 'Search', 'search', 'Ctrl+F'],
    ['find.global', 'Search everywhere', 'Search', 'search', 'Ctrl+E'],
    ['tool.palette', 'Command palette', 'Tools', 'cmd', 'Ctrl+Shift+P'],
    ['tool.settings', 'Open settings', 'Tools', 'gear', 'Ctrl+,'],
    ['tool.term', 'Open terminal here', 'Tools', 'term', 'Ctrl+Alt+T'],
    ['tool.vscode', 'Open in VS Code', 'Tools', 'path', null, true],
    ['tool.update', 'Check for updates', 'Tools', 'download', null],
    ['app.quit', 'Quit Tsubame', 'App', 'quit', 'Alt+F4']
  ].map(function (c) { return { id: c[0], n: c[1], cat: c[2], ic: c[3], k: c[4], custom: !!c[5] }; });
  var BYID = {}; CMDS.forEach(function (c) { BYID[c.id] = c; });
  var RECENT = ['tool.settings', 'tab.reopen', 'pane.toggle', 'file.newfolder', 'view.hidden'];

  /* greedy fuzzy: leftmost subsequence, prefers word starts when scoring; returns {score, hits[]} or null */
  function fuzzy(text, q) {
    q = q.toLowerCase().replace(/\s+/g, '');
    if (!q) return { score: 0, hits: [] };
    var t = text.toLowerCase(), hits = [], ti = 0, score = 0, prev = -2;
    for (var qi = 0; qi < q.length; qi++) {
      var found = -1;
      for (var j = ti; j < t.length; j++) {
        if (t[j] === q[qi]) {
          // prefer a word start within the next 6 chars when the current hit is mid-word
          var atStart = j === 0 || t[j - 1] === ' ';
          if (!atStart) { for (var k = j + 1; k < Math.min(t.length, j + 7); k++) if (t[k] === q[qi] && t[k - 1] === ' ') { j = k; atStart = true; break; } }
          found = j; score += (atStart ? 8 : 1) + (j === prev + 1 ? 6 : 0) - (atStart ? 0 : j * 0.4); break;
        }
      }
      if (found < 0) return null;
      hits.push(found); prev = found; ti = found + 1;
    }
    return { score: score - t.length * 0.08, hits: hits };
  }
  function hl(text, hits) {
    if (!hits || !hits.length) return esc(text);
    var set = {}; hits.forEach(function (i) { set[i] = 1; });
    var out = '', open = false;
    for (var i = 0; i < text.length; i++) {
      if (set[i] && !open) { out += '<mark>'; open = true; }
      if (!set[i] && open) { out += '</mark>'; open = false; }
      out += esc(text[i]);
    }
    return out + (open ? '</mark>' : '');
  }

  /* ---------- palette pieces ---------- */
  var MODES = [['cmd', '>', 'Commands'], ['path', '/', 'Go to path'], ['filter', '#', 'Filter folder'], ['tabs', '@', 'Open tabs']];
  var PFX = { cmd: '>', path: '/', filter: '#', tabs: '@', fresh: '', none: '>' };
  var MODEICON = { cmd: 'cmd', path: 'folder', filter: 'search', tabs: 'newtab', fresh: 'cmd', none: 'cmd' };
  function palInput(st) {
    var m = st.mode, p = PFX[m] || '';
    var q = st.q || '';
    var h = '<div class="pal-in">' + ic(MODEICON[m] || 'cmd', 18) + '<div class="val">';
    if (m === 'fresh' && !q) h += '<span class="caret" style="margin-right:2px"></span><span class="ph">Type a command, or / # @ for other modes</span>';
    else h += (p ? '<span class="pfx">' + p + '</span>' : '') + '<span>' + esc(q) + '</span><span class="caret"></span>';
    h += '</div>' + kc('Esc') + '</div>';
    return h;
  }
  function palModes(st) {
    var cur = st.mode === 'none' ? 'cmd' : st.mode;
    return '<div class="pal-modes">' + MODES.map(function (m) { return '<span class="pm ' + (m[0] === cur ? 'on' : '') + '"><b>' + m[1] + '</b>' + m[2] + '</span>'; }).join('') +
      '<span style="margin-left:auto">Type a prefix, or Backspace on an empty field to switch</span></div>';
  }
  function palFoot(items) {
    return '<div class="pal-ft">' + items.map(function (i) { return '<span class="g">' + i[0].split(/[ +]/).map(function (k) { return kc(k); }).join('') + ' ' + i[1] + '</span>'; }).join('') + '</div>';
  }
  function cmdRow(c, hits, hot) {
    return '<div class="pr ' + (hot ? 'hot' : '') + '">' + ic(c.ic) + '<span class="nm t">' + hl(c.n, hits) + (c.custom ? ' <span class="tag acc" style="margin-left:6px">Custom</span>' : '') + '</span>' +
      '<span class="cat">' + c.cat + '</span>' + keys(c.k) + '</div>';
  }
  function sbarFor(thumbTop, thumbH) { return '<div class="sbar"><i style="top:' + thumbTop + 'px;height:' + thumbH + 'px"></i></div>'; }

  function palCmd(st) {
    var q = st.q || '', rows = '', hot = st.hot || 0, n = 0;
    if (!q) {
      rows += '<div class="pal-sec">Recent</div>';
      RECENT.forEach(function (id) { rows += cmdRow(BYID[id], null, n++ === hot); });
      rows += '<div class="pal-sec">All commands <span class="n">' + CMDS.length + '</span></div>';
      CMDS.filter(function (c) { return RECENT.indexOf(c.id) < 0; }).slice(0, 4).forEach(function (c) { rows += cmdRow(c, null, false); });
      return '<div class="pal-res">' + rows + sbarFor(2, 120) + '</div>' + palFoot([['\u2191 \u2193', 'Move'], ['Enter', 'Run'], ['Ctrl+Enter', 'Run, keep open'], ['Tab', 'Show shortcut']]);
    }
    var res = CMDS.map(function (c) { var f = fuzzy(c.n, q); return f && { c: c, f: f }; }).filter(Boolean).sort(function (a, b) {
      var ra = RECENT.indexOf(a.c.id) >= 0 ? 1.5 : 0, rb = RECENT.indexOf(b.c.id) >= 0 ? 1.5 : 0; return (b.f.score + rb) - (a.f.score + ra);
    });
    rows += '<div class="pal-sec">Best matches <span class="n">' + res.length + '</span></div>';
    res.slice(0, 8).forEach(function (r) { rows += cmdRow(r.c, r.f.hits, n++ === hot); });
    return '<div class="pal-res">' + rows + '</div>' + palFoot([['\u2191 \u2193', 'Move'], ['Enter', 'Run'], ['Ctrl+Enter', 'Run, keep open'], ['Tab', 'Show shortcut']]);
  }

  var DIRS = [['Desktop', 4], ['Documents', 31], ['Downloads', 33], ['Dropbox', 12]];
  function palPath(st) {
    var rows = '<div class="pal-sec">Folders in <span style="color:var(--c-text);font-weight:400">C:\\Users\\Alex\\</span> <span class="n">4</span></div>';
    DIRS.forEach(function (d, i) {
      var typed = 'D';
      rows += '<div class="pr ' + (i === (st.hot == null ? 2 : st.hot) ? 'hot' : '') + '">' + T.folderIcon(16) + '<span class="nm t path"><span class="dim">C:\\Users\\Alex\\</span><mark>D</mark>' + esc(d[0].slice(1)) + '</span>' +
        '<span class="sub">' + d[1] + ' items</span>' + (i === 2 ? '<span class="keys">' + kc('Tab') + '<span class="sub" style="margin-left:4px">complete</span></span>' : '<span class="keys" style="width:84px"></span>') + '</div>';
    });
    rows += '<div class="pal-sec">Recent folders</div>';
    [['X:\\Dev\\tsubame\\mocks', 'yesterday'], ['\\\\rig\\Games\\Minecraft packs', '2 days ago'], ['C:\\Users\\Alex\\Documents\\Invoices', 'last week']].forEach(function (r) {
      rows += '<div class="pr">' + ic('clock') + '<span class="nm t path">' + esc(r[0]) + '</span><span class="sub">' + r[1] + '</span></div>';
    });
    return '<div class="pal-res">' + rows + '</div>' + palFoot([['\u2191 \u2193', 'Move'], ['Tab', 'Complete'], ['Enter', 'Go'], ['Ctrl+Enter', 'Open in new tab'], ['Backspace', 'Up one level']]);
  }

  function palFilter(st) {
    var q = st.q || 'inv', D = Tsu.data;
    var m = D.filter(function (r) { return r.n.toLowerCase().indexOf(q.toLowerCase()) >= 0; });
    var rows = '<div class="pr hot">' + ic('hash') + '<span class="nm t">Filter <b style="font-weight:600">Downloads</b> by \u201C' + esc(q) + '\u201D</span><span class="sub"><span style="color:var(--c-text)">' + m.length + '</span> of ' + D.length + ' items match</span></div>';
    rows += '<div class="pal-sec">Matches <span class="n">live preview, the list behind updates as you type</span></div>';
    m.slice(0, 6).forEach(function (r) {
      var i = r.n.toLowerCase().indexOf(q.toLowerCase());
      rows += '<div class="pr">' + (r.k === 'dir' ? T.folderIcon(16) : T.fileIcon(r.k)) + '<span class="nm t">' + esc(r.n.slice(0, i)) + '<mark>' + esc(r.n.slice(i, i + q.length)) + '</mark>' + esc(r.n.slice(i + q.length)) + '</span>' +
        '<span class="sub">' + esc(r.t) + '</span><span class="sub" style="width:56px;text-align:right">' + esc(T.fmtSize(r.s)) + '</span></div>';
    });
    return '<div class="pal-res">' + rows + '</div>' + palFoot([['Enter', 'Keep filter in folder'], ['\u2191 \u2193', 'Select a match'], ['Shift+Enter', 'Open selected'], ['Esc', 'Cancel, clear filter']]);
  }

  var TABS = [['Downloads', 'C:\\Users\\Alex\\Downloads', 'Pane 1', 1, true], ['tsubame', 'X:\\Dev\\tsubame', 'Pane 1', 2], ['Documents', 'C:\\Users\\Alex\\Documents', 'Pane 1', 3],
    ['Minecraft packs', '\\\\rig\\Games\\Minecraft packs', 'Pane 2', 4], ['blink', 'X:\\Dev\\blink', 'Pane 2', 5], ['Invoices', 'C:\\Users\\Alex\\Documents\\Invoices', 'Pane 2', 6]];
  function palTabs(st) {
    var rows = '<div class="pal-sec">Open tabs <span class="n">6, across 2 panes</span></div>', hot = st.hot == null ? 1 : st.hot;
    TABS.forEach(function (t, i) {
      rows += '<div class="pr ' + (i === hot ? 'hot' : '') + '">' + T.folderIcon(16) + '<span class="nm t">' + esc(t[0]) + ' <span class="dim" style="margin-left:6px;font-size:var(--font-size-xs);color:var(--c-text-dim)">' + esc(t[1]) + '</span></span>' +
        (t[4] ? '<span class="tag acc">Current</span>' : '<span class="tag">' + t[2] + '</span>') + '<span class="keys" style="width:64px">' + kc('Alt') + kc(String(t[3])) + '</span></div>';
    });
    return '<div class="pal-res">' + rows + '</div>' + palFoot([['\u2191 \u2193', 'Move'], ['Enter', 'Switch to tab'], ['Ctrl+Enter', 'Move tab here'], ['Delete', 'Close tab']]);
  }

  function palFresh() {
    var cell = function (b, l, d) { return '<div class="cell"><b>' + b + '</b><span>' + l + '</span><span class="d">' + d + '</span></div>'; };
    return '<div class="pal-empty">' + ic('cmd', 32) + '<div class="big">Find any command</div><div class="hint">No recent commands yet. Start typing, or pick a mode with a prefix.</div>' +
      '<div class="grid">' + cell('>', 'Commands', 'rename') + cell('/', 'Go to path', 'C:\\') + cell('#', 'Filter this folder', 'invoice') + cell('@', 'Jump to open tab', 'dow') + '</div></div>' +
      palFoot([['\u2191 \u2193', 'Move'], ['Enter', 'Run'], ['Esc', 'Close']]);
  }
  function palNone(st) {
    var q = st.q || 'zzq';
    return '<div class="pal-none">' + ic('search', 24, '') + '<div class="big">No commands match \u201C' + esc(q) + '\u201D</div><div class="hint">Try fewer letters, or press <span class="kcap" style="display:inline-flex">/</span> to go to a path, <span class="kcap" style="display:inline-flex">#</span> to filter this folder.</div></div>' +
      '<div class="pal-res" style="padding-top:0"><div class="pr none">' + ic('plus') + '<span class="nm t" style="color:var(--c-text-dim)">Create a custom command called \u201C' + esc(q) + '\u201D\u2026</span><span class="sub">Settings \u203A Context menu</span></div></div>' +
      palFoot([['Backspace', 'Edit search'], ['Esc', 'Close']]);
  }

  function palette(st, w, h) {
    st = st || {};
    var m = st.mode || 'cmd', body;
    if (m === 'cmd') body = palCmd(st); else if (m === 'path') body = palPath(st); else if (m === 'filter') body = palFilter(st);
    else if (m === 'tabs') body = palTabs(st); else if (m === 'fresh') body = palFresh(); else body = palNone(st);
    var ov = '<div class="pal">' + palInput(st) + (m === 'fresh' ? '' : palModes(st)) + body + '</div>';
    var base = Tsu.win({ w: w || 1280, h: h || 640, sel: 'multi', zebra: !!st.zebra, pathEdit: m === 'path' ? undefined : undefined });
    return base.slice(0, base.length - 6) + ov + '</div>';
  }

  window.Tsu4 = {
    palette: palette, cmds: CMDS, byId: BYID, recent: RECENT, fuzzy: fuzzy, hl: hl,
    kc: kc, keys: keys, tog: tog, seg: seg, btn: btn, field: field, dd: dd, slider: slider, chk: chk, ic: ic, esc: esc, T: T
  };
})();
