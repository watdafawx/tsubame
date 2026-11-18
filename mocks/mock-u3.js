/*
  U3 builders: sidebar, dual pane / splits, inline rename, batch rename, drag states.
  Needs mock.js (Tsu, TsuParts) first. Plain DOM strings, same class names as mock.css + mock-u3.css.

  Tsu3.win(o) -> html for one window. o:
    mode      'single' (M1 layout: tabs in title bar, toolbar over sidebar) | 'v' (side by side) | 'h' (stacked)
    w, h      window size (default 1280 x 720)
    density   22 | 26 | 30
    sb        sidebar state (see sidebar3)
    panes     [paneA, paneB]  pane state (see pane3). single mode uses panes[0]
    active    0 | 1
    split     {pct: 50, state: 'hover'|'drag'}      pane A share in percent
    sync      true | false                          sync-scroll indicator
    batch     {...}                                 batch rename bar (single mode)
    ov        extra overlay html (drag images, menus), placed in window coordinates
*/
(function () {
  'use strict';
  var P = window.TsuParts, T = window.Tsu;
  var esc = P.esc, folderIcon = P.folderIcon, fileIcon = P.fileIcon, fmtSize = P.fmtSize, sortArrow = P.sortArrow;
  var DATA = T.data;

  /* ---------- extra 16 px glyphs (same stroke style as mock.js) ---------- */
  var IC3 = {
    grip: '<circle class="f" cx="6" cy="4" r=".9"/><circle class="f" cx="10" cy="4" r=".9"/><circle class="f" cx="6" cy="8" r=".9"/><circle class="f" cx="10" cy="8" r=".9"/><circle class="f" cx="6" cy="12" r=".9"/><circle class="f" cx="10" cy="12" r=".9"/>',
    pinoff: '<path d="M8 2l1.8 3.8 4.2.5-3.1 2.9.8 4.1L8 11.2 4.3 13.3l.8-4.1L2 6.3l4.2-.5z"/><path d="M2.5 13.5l11-11"/>',
    panel1: '<rect x="2" y="3" width="12" height="10" rx="1"/>',
    panelv: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M8 3v10"/>',
    panelh: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M2 8h12"/>',
    link: '<path d="M6.8 9.2l2.4-2.4M7.2 5.2l1-1a2.4 2.4 0 0 1 3.4 3.4l-1 1M8.8 10.8l-1 1a2.4 2.4 0 0 1-3.4-3.4l1-1"/>',
    clock: '<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/>',
    warn: '<path d="M8 2.5 14 13H2z"/><path d="M8 6.5v3M8 11.2v.1"/>',
    collapse: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M6 3v10M11 6.5 9.5 8l1.5 1.5"/>',
    expand: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M6 3v10M9.5 6.5 11 8 9.5 9.5"/>',
    down: '<path d="M8 3v9.5M3.5 8.5 8 13l4.5-4.5"/>',
    moveto: '<path d="M2.5 8h8M7 4.5 10.5 8 7 11.5M13.5 3v10"/>',
    copyto: '<rect x="2" y="4" width="7" height="8" rx="1"/><path d="M11 8h3M12.5 6.5 14 8l-1.5 1.5"/>',
    arrow: '<path d="M2.5 8h9.5M8.5 4.5 12 8l-3.5 3.5"/>',
    drive2: '<rect x="1.8" y="5" width="12.4" height="6" rx="1.5"/><circle cx="11.5" cy="8" r=".6"/>'
  };
  function ic(n, s, x) {
    s = s || 16;
    if (!IC3[n]) return P.ic(n, s, x);
    return '<svg class="ic ' + (x || '') + '" width="' + s + '" height="' + s + '" viewBox="0 0 16 16">' + IC3[n] + '</svg>';
  }
  function k(t, cls) { return '<span class="u3k' + (cls ? ' ' + cls : '') + '">' + t + '</span>'; }

  /* ---------- data: pane A = M1 Downloads, pane B = a destination folder built from the same rows ---------- */
  function find(n) { for (var i = 0; i < DATA.length; i++) if (DATA[i].n === n) return DATA[i]; throw new Error('no row ' + n); }
  function dir(n, d) { return { n: n, d: d, t: 'File folder', s: null, k: 'dir' }; }
  var A = DATA;
  var B = [dir('2026', '2026-11-02 08:40'), dir('Archive', '2026-09-14 19:05'), dir('Receipts', '2026-10-31 18:12')].concat(
    ['0412-receipt.pdf', 'dataset_v2.csv', 'Final inv draft.docx', 'inventory.xlsx', 'invite-list.txt', 'Invoice-Acme-2026-10.pdf', 'invoice_scan_0412.jpg', 'notes.md'].map(find));
  function idx(rows, n) { for (var i = 0; i < rows.length; i++) if (rows[i].n === n) return i; return -1; }

  /* ---------- rows ---------- */
  function seg(text, a, b, c) {
    if (b > a) return esc(text.slice(0, a)) + '<span class="selr">' + esc(text.slice(a, b)) + '</span><span class="caret"></span>' + esc(text.slice(b));
    c = c == null ? text.length : c;
    return esc(text.slice(0, c)) + '<span class="caret"></span>' + esc(text.slice(c));
  }
  function editor(e) {
    // e: {text, a, b, c, state: undefined|'bad', tip: {icon, text, dim}, keys: bool}
    var h = '<div class="ed' + (e.state ? ' ' + e.state : '') + '"><span class="edt">' + seg(e.text, e.a || 0, e.b || 0, e.c) + '</span>';
    if (e.keys) h += '<span class="edk">' + k('Enter') + k('Esc') + '</span>';
    if (e.tip) h += '<div class="tip3 bad">' + ic('warn', 14) + '<span>' + esc(e.tip.text) + '</span>' + (e.tip.dim ? '<span class="dim">' + e.tip.dim + '</span>' : '') + '</div>';
    return h + '</div>';
  }
  function prow(r, o) {
    var cols = o.cols;
    var h = '<div class="row ' + o.cls + '">';
    if (o.ed) h += '<div class="cell name c-name editing">' + (r.k === 'dir' ? folderIcon(16) : fileIcon(r.k)) + editor(o.ed) + '</div>';
    else h += '<div class="cell name c-name">' + (r.k === 'dir' ? folderIcon(16) : fileIcon(r.k)) + '<span class="t">' + esc(r.n) + '</span></div>';
    h += '<div class="cell c-date"><span class="t">' + r.d + '</span></div>';
    if (cols === 'full') h += '<div class="cell c-type"><span class="t">' + r.t + '</span></div>';
    h += '<div class="cell r c-size"><span class="t">' + fmtSize(r.s) + '</span></div></div>';
    return h;
  }
  function colhead(cols) {
    return '<div class="colhead"><div class="ch c-name sorted"><span>Name</span>' + sortArrow(true) + '</div><div class="ch c-date">Date modified</div>' +
      (cols === 'full' ? '<div class="ch c-type">Type</div>' : '') + '<div class="ch c-size r">Size</div></div>';
  }
  function listArea(p) {
    var rows = p.rows, h = '';
    rows.forEach(function (r, i) {
      var cls = [];
      var sel = p.sel && p.sel[i];
      if (sel) cls.push('sel' + (p.active ? '' : ' inactive'));
      if (p.hover === i && !sel) cls.push('hover');
      if (p.drop === i) cls.push('drop');
      if (p.focus === i && p.active && !(p.ed && p.ed.row === i)) cls.push('focus');
      h += prow(r, { cls: cls.join(' '), cols: p.cols, ed: p.ed && p.ed.row === i ? p.ed : null });
    });
    return colhead(p.cols) + '<div class="list"><div class="rows">' + h + '</div>' + P.scrollbar(rows.length, 500, false) + '</div>';
  }
  function status(p, right) {
    var n = p.rows.length, picked = [];
    for (var i in (p.sel || {})) picked.push(p.rows[+i]);
    var left = p.statusLeft || (n + ' items');
    var h = '<div class="status"><span>' + left + '</span>';
    if (picked.length) {
      var tot = picked.reduce(function (a, r) { return a + (r.s || 0); }, 0);
      h += '<span class="v"></span><span class="em">' + picked.length + ' selected</span><span>' + fmtSize(tot) + '</span>';
    }
    return h + '<span class="push grp">' + (right || '') + '</span></div>';
  }

  /* ---------- pane ---------- */
  function tabs(p) {
    var h = '<div class="ptabs">';
    p.tabs.forEach(function (t) {
      h += '<div class="tab' + (t.active ? ' active' : '') + (t.hover ? ' hover' : '') + '">' + folderIcon(14) + '<span class="t">' + esc(t.t) + '</span><span class="x' + (t.hover ? ' hot' : '') + '">' + ic('x', 10) + '</span></div>';
    });
    return h + '<div class="newtab">' + ic('plus', 14) + '</div></div>';
  }
  function pane(p, o, which) {
    p.active = o.active === which;
    var dual = o.mode !== 'single';
    var h = '<div class="pane' + (p.active ? ' active' : '') + (p.dropPane ? ' drop' : '') + '" style="flex-grow:' + (which === 0 ? o.split.pct : 100 - o.split.pct) + '">';
    h += tabs(p);
    h += '<div class="toolbar"><div class="btn">' + ic('back') + '</div><div class="btn off">' + ic('fwd') + '</div><div class="btn">' + ic('up') + '</div><div class="navgap"></div>' +
      P.pathbar({ path: p.path }) + '<div class="btn">' + ic('search') + '</div></div>';
    h += '<div class="main3">' + listArea(p) + '</div>';
    var right = '';
    if (p.statusRight != null) right = p.statusRight;
    else if (p.active && dual && p.sel && Object.keys(p.sel).length) {
      var to = o.mode === 'v' ? (which === 0 ? 'right' : 'left') : (which === 0 ? 'bottom' : 'top');
      right = '<span class="grp">' + k('F5') + 'Copy to ' + to + '</span><span class="grp">' + k('F6') + 'Move to ' + to + '</span>';
    } else if (p.active) right = '<span class="grp">' + k('Tab') + 'Other pane</span>';
    else right = p.free || '';
    h += status(p, right);
    return h + (p.ov || '') + '</div>';
  }

  /* ---------- title bar ---------- */
  function tbtools(o) {
    var m = o.mode;
    var h = '<div class="tbtools">';
    if (m !== 'single') h += '<span class="tsync ' + (o.sync ? 'on' : 'off') + '">' + ic('link', 14) + '<span class="t">Sync</span></span>';
    h += '<div class="tseg"><span class="' + (m === 'single' ? 'on' : '') + '">' + ic('panel1', 14) + '</span><span class="' + (m === 'v' ? 'on' : '') + '">' + ic('panelv', 14) + '</span><span class="' + (m === 'h' ? 'on' : '') + '">' + ic('panelh', 14) + '</span></div></div>';
    return h;
  }
  function titlebar(o) {
    var s;
    if (o.mode === 'single') s = P.titlebar({ tabs: o.tabs });
    else s = P.titlebar({ tabs: [] }).replace(/<div class="newtab">.*?<\/div>/, '');
    return s.replace('<div class="drag"></div>', '<div class="drag"></div>' + tbtools(o));
  }

  /* ---------- sidebar ---------- */
  var PIN = [['star', 'tsubame'], ['star', 'blink'], ['star', 'Minecraft packs'], ['net', 'rig']];
  var QUICK = [['home', 'Home'], ['desktop', 'Desktop'], ['download', 'Downloads'], ['docs', 'Documents'], ['pics', 'Pictures'], ['music', 'Music'], ['video', 'Videos']];
  var RECENT = [['Invoices', 'Documents'], ['docs', 'tsubame'], ['world-backup', 'packs'], ['Screenshots', 'Downloads']];
  var DRIVES = [['Windows (C:)', '118 GB', 75], ['Dev (X:)', '1.1 TB', 70], ['Games (G:)', '41 GB', 97], ['Backup (E:)', '312 GB', 48]];
  var NET = [['RIG', '\\\\192.168.50.3'], ['NAS', '\\\\192.168.50.2']];

  /*
    sidebar state s: {collapsed, w, closed:{recent,net}, cur:['quick',2], hover:['pin',1], drop:['pin',1], src:['pin',1],
                      ins: n (insertion line before pinned row n), hdr:'recent' (hovered header), handle:'hover'|'drag',
                      tip:['quick',4] (collapsed tooltip target), foot:true (footer hovered)}
    returns {html, ov, pos}  pos[key] = y of row inside the sidebar (for overlays)
  */
  function sidebar(s, winTop) {
    s = s || {};
    var mini = !!s.collapsed, w = mini ? 48 : (s.w || 208), y = 6, pos = {}, ov = '', body = '';
    function is(a, key, i) { return a && a[0] === key && a[1] === i; }
    function cls(key, i) { return (is(s.cur, key, i) ? ' cur' : '') + (is(s.hover, key, i) ? ' hover' : '') + (is(s.drop, key, i) ? ' drop' : '') + (is(s.src, key, i) ? ' src' : ''); }
    function hdr(id, label, extra, first) {
      var closed = s.closed && s.closed[id];
      var h = '<div class="sh' + (s.hdr === id ? ' hot' : '') + '"' + (first ? '' : ' style="margin-top:6px"') + '>' + ic(closed ? 'chev' : 'chevd', 12) + label + (extra || '') + '</div>';
      y += 26 + (first ? 0 : 6); return h;
    }
    function row(key, i, icon, label, sub, extra) {
      pos[key + i] = y; y += 24;
      return '<div class="si' + cls(key, i) + '">' + icon + '<span class="t">' + esc(label) + '</span>' + (sub ? '<span class="sub">' + esc(sub) + '</span>' : '') + (extra || '') + '</div>';
    }
    function mrow(key, i, icon, bar) {
      pos[key + i] = y; y += bar != null ? 32 : 28;
      return '<div class="si mini' + (bar != null ? ' drv' : '') + cls(key, i) + '">' + icon + (bar != null ? '<span class="mbar"><i class="' + (bar > 90 ? 'full' : '') + '" style="width:' + bar + '%"></i></span>' : '') + '</div>';
    }
    function msep() { y += 9; return '<div class="msep3"><i></i></div>'; }

    if (!mini) {
      body += hdr('pin', 'Pinned', '', true);
      PIN.forEach(function (p, i) {
        if (s.ins === i) body += '<div class="ins"></div>';
        var hov = is(s.hover, 'pin', i);
        body += row('pin', i, hov ? ic('pinoff', 16, 'pin') : ic(p[0]), p[1], '', hov ? ic('grip', 14, 'grip') : '');
      });
      if (s.ins === PIN.length) body += '<div class="ins"></div>';
      body += hdr('quick', 'Quick access');
      QUICK.forEach(function (q, i) { body += row('quick', i, ic(q[0]), q[1]); });
      body += hdr('recent', 'Recent', (s.closed && s.closed.recent) ? '' : '<span class="clr' + (s.clrHot ? ' hot' : '') + '">Clear</span>');
      if (!(s.closed && s.closed.recent)) RECENT.forEach(function (r, i) { body += row('recent', i, folderIcon(16), r[0], r[1]); });
      body += hdr('pc', 'This PC');
      DRIVES.forEach(function (d, i) {
        pos['pc' + i] = y; y += 32;
        body += '<div class="drive' + cls('pc', i).replace(' hover', '') + '"><div class="r1">' + ic('drive') + '<span class="t" style="flex:1">' + d[0] + '</span><span class="free">' + d[1] + ' free</span></div>' +
          '<div class="bar"><i class="' + (d[2] > 90 ? 'full' : '') + '" style="width:' + d[2] + '%"></i></div></div>';
      });
      body += hdr('net', 'Network');
      if (!(s.closed && s.closed.net)) NET.forEach(function (n, i) { body += row('net', i, ic('net'), n[0], n[1]); });
    } else {
      PIN.forEach(function (p, i) { body += mrow('pin', i, ic(p[0])); });
      body += msep();
      QUICK.forEach(function (q, i) { body += mrow('quick', i, ic(q[0])); });
      body += msep() + mrow('recent', 0, ic('clock')) + msep();
      DRIVES.forEach(function (d, i) { body += mrow('pc', i, ic('drive'), d[2]); });
      body += msep() + mrow('net', 0, ic('net'));
    }
    var foot = mini ? '<div class="sb-foot' + (s.foot ? ' hot' : '') + '">' + ic('expand') + '</div>' :
      '<div class="sb-foot' + (s.foot ? ' hot' : '') + '">' + ic('collapse') + '<span>Collapse</span><span class="push">' + k('Ctrl+B') + '</span></div>';
    var handle = s.handle ? '<div class="sb-handle ' + s.handle + '"></div>' : '';
    if (s.tip) {
      var key = s.tip[0] + s.tip[1];
      var label = s.tip[0] === 'quick' ? QUICK[s.tip[1]][1] : s.tip[0] === 'pin' ? PIN[s.tip[1]][1] : s.tip[0] === 'pc' ? DRIVES[s.tip[1]][0] + ' · ' + DRIVES[s.tip[1]][1] + ' free' : 'Recent folders';
      var ty = winTop + pos[key] + (s.tip[0] === 'pc' ? 4 : 2);
      ov += '<div class="tip3" style="left:' + (w + 6) + 'px;top:' + ty + 'px">' + esc(label) + '</div>';
    }
    if (s.chip) ov += '<div class="chip" style="left:' + (w + 8) + 'px;top:' + (winTop + 150) + 'px"><span>' + s.chip + ' px</span><span class="dim">drag left to collapse</span></div>';
    return { html: '<div class="sidebar u3' + (mini ? ' mini' : '') + '" style="width:' + w + 'px"><div class="sb-scroll">' + body + '</div>' + foot + handle + '</div>', ov: ov, pos: pos, w: w };
  }

  /* ---------- window ---------- */
  function win(o) {
    var W = o.w || 1280, Hh = o.h || 720, dual = o.mode !== 'single';
    o.split = o.split || { pct: 50 };
    var winTop = dual ? 34 : 74;
    var sb = sidebar(o.sb, winTop);
    var h = '<div class="win zebra-off" data-density="' + (o.density || 22) + '" style="width:' + W + 'px;height:' + Hh + 'px">' + titlebar(o);
    if (!dual) {
      var p = o.panes[0]; p.active = true;
      var filt = P.filterbox({});
      h += '<div class="toolbar"><div class="btn">' + ic('back') + '</div><div class="btn off">' + ic('fwd') + '</div><div class="btn">' + ic('up') + '</div><div class="navgap"></div>' + P.pathbar({ path: p.path }) + filt + '</div>';
      var bb = o.batch ? batchBar(o.batch) : '';
      h += '<div class="body">' + sb.html + '<div class="main main3" style="position:relative">' + bb + listArea(p) + (o.batch ? batchPreview(o.batch) : '') + '</div></div>';
      h += status(p, p.statusRight != null ? p.statusRight : (p.free || '118 GB free of 476 GB'));
    } else {
      h += '<div class="body">' + sb.html + '<div class="panes' + (o.mode === 'h' ? ' h' : '') + '">' + pane(o.panes[0], o, 0) +
        '<div class="split ' + (o.mode === 'v' ? 'v' : 'h') + (o.split.state ? ' ' + o.split.state : '') + '"></div>' + pane(o.panes[1], o, 1) + '</div></div>';
    }
    return h + sb.ov + (o.ov || '') + '</div>';
  }

  /* ---------- batch rename ---------- */
  function expand(pat, stem, n) { return pat.replace(/\{name\}/g, stem).replace(/\{n\}/g, String(n)); }
  function splitName(nm) { var i = nm.lastIndexOf('.'); return i > 0 ? [nm.slice(0, i), nm.slice(i)] : [nm, '']; }
  function batchModel(pat, items) {
    var out = items.map(function (r, i) { var s = splitName(r.n); return { o: r.n, n: expand(pat, s[0], i + 1) + s[1] }; });
    var cnt = {};
    out.forEach(function (x) { var key = x.n.toLowerCase(); cnt[key] = (cnt[key] || 0) + 1; });
    var bad = 0;
    out.forEach(function (x) { x.bad = cnt[x.n.toLowerCase()] > 1; if (x.bad) bad++; });
    return { rows: out, bad: bad };
  }
  function patHtml(p) { return esc(p).replace(/\{(name|n|ext|date)\}/g, '<span class="tok">{$1}</span>'); }
  function batchBar(b) {
    var m = batchModel(b.pattern, b.items), bad = m.bad > 0;
    return '<div class="bbar"><span class="ttl">Rename ' + b.items.length + ' items</span>' +
      '<div class="bf"><span class="lbl">Name</span><span class="val">' + patHtml(b.pattern) + '<span class="caret"></span></span></div>' +
      '<span class="tokhint">Tokens <code>{name}</code> <code>{n}</code> <code>{date}</code> · extension is kept</span>' +
      '<span class="btns"><span class="u3btn' + (bad ? ' off' : ' pri') + '">Apply ' + k('Enter') + '</span><span class="u3btn">Cancel ' + k('Esc') + '</span></span></div>';
  }
  function batchPreview(b) {
    var m = batchModel(b.pattern, b.items), shown = m.rows.slice(0, 8), h = '';
    h += '<div class="pv"><div class="ph"><span class="em">Preview</span><span>' + m.rows.length + ' renames</span>' +
      (m.bad ? '<span class="bad">' + m.bad + ' names collide</span>' : '<span>no conflicts</span>') + '</div>';
    shown.forEach(function (r) {
      h += '<div class="pr' + (r.bad ? ' bad' : '') + '"><span class="o t">' + esc(r.o) + '</span><span class="ar">' + ic('arrow', 12) + '</span><span class="n t">' + esc(r.n) + '</span>' +
        (r.bad ? '<span class="flag">' + ic('warn', 12) + 'duplicate</span>' : '') + '</div>';
    });
    if (m.rows.length > shown.length) h += '<div class="more">+ ' + (m.rows.length - shown.length) + ' more</div>';
    h += '<div class="pf"></div><div class="pfoot">' + (m.bad ? '<span>Add <b style="color:var(--c-text);font-weight:400;font-family:var(--font-mono)">{n}</b> to make every name unique</span>' : '<span>One undo step: ' + k('Ctrl+Z') + '</span>') + '</div></div>';
    return h;
  }

  /* ---------- overlays ---------- */
  function ctxMenu(x, y) {
    var items = [['open', 'Open', 'Enter'], ['newtab', 'Open in new tab', 'Ctrl+Enter'], ['panelv', 'Open in other pane', 'Ctrl+Shift+Enter'], '-',
      ['rename', 'Rename', 'F2'], ['up', 'Move up', 'Alt+↑'], ['down', 'Move down', 'Alt+↓'], '-', ['pinoff', 'Unpin', 'Del']];
    var h = '<div class="popup" style="left:' + x + 'px;top:' + y + 'px;width:var(--layout-menu-width)">';
    items.forEach(function (m, i) {
      if (m === '-') { h += '<div class="msep"><i></i></div>'; return; }
      h += '<div class="mi' + (m[1] === 'Open in other pane' ? ' hot' : '') + '">' + ic(m[0]) + '<span class="t">' + m[1] + '</span><span class="k">' + m[2] + '</span></div>';
    });
    return h + '</div>';
  }
  function px(v) { return typeof v === 'number' ? v + 'px' : v; }
  function dragImg(x, y, text, count, icon) {
    return '<div class="dimg" style="left:' + px(x) + ';top:' + px(y) + '">' + (icon || fileIcon('txt')) + '<span>' + esc(text) + '</span>' + (count > 1 ? '<span class="badge">' + count + '</span>' : '') + '</div>';
  }
  function dragTip(x, y, icon, l1, mods) {
    var h = '<div class="dtip" style="left:' + px(x) + ';top:' + px(y) + '"><div class="l1">' + ic(icon, 14) + '<span>' + l1 + '</span></div><div class="l2">';
    h += mods.join('<span>·</span>') + '</div></div>';
    return h;
  }
  function mod(key, text) { return '<span style="display:inline-flex;align-items:center;gap:4px">' + k(key) + text + '</span>'; }

  /* ---------- key card (shortcut cheat sheet drawn like a popup) ---------- */
  function keycard() {
    var G = [
      ['Sidebar', [['Ctrl+B', 'Collapse / expand'], ['Alt+↑ ↓', 'Reorder bookmark'], ['Del', 'Unpin bookmark'], ['Ctrl+D', 'Pin current folder'], ['F2', 'Rename bookmark label']]],
      ['Panes', [['Ctrl+\\', 'Single / split'], ['Ctrl+Shift+\\', 'Vertical / horizontal'], ['Tab', 'Switch pane'], ['F5', 'Copy to other pane'], ['F6', 'Move to other pane']]],
      ['Rename', [['F2', 'Rename (stem selected)'], ['Enter', 'Commit'], ['Esc', 'Cancel'], ['Tab', 'Commit, rename next'], ['F2 ×12', 'Batch rename bar']]],
      ['While dragging', [['(none)', 'Same drive: move'], ['Ctrl', 'Force copy'], ['Shift', 'Force move'], ['Alt', 'Create shortcut'], ['Esc', 'Cancel drag']]]
    ];
    var h = '<div class="keycard">';
    G.forEach(function (g) {
      h += '<div><h3>' + g[0] + '</h3>';
      g[1].forEach(function (r) { h += '<div class="kr"><span class="ks">' + r[0].split(' ').map(function (x) { return k(x); }).join('') + '</span><span class="dim">' + r[1] + '</span></div>'; });
      h += '</div>';
    });
    return h + '</div>';
  }

  /* ---------- review page helpers ---------- */
  function qsDensity() { var d = T.qs('density', '22'); return d === '26' || d === '30' ? +d : 22; }
  function wireControls(draw) {
    document.addEventListener('click', function (e) {
      var b = e.target.closest('button'); if (!b) return;
      if (b.dataset.themeBtn) draw({ theme: b.dataset.themeBtn });
      if (b.dataset.densityBtn) draw({ density: +b.dataset.densityBtn });
    });
  }

  window.Tsu3 = { win: win, ic: ic, k: k, A: A, B: B, idx: idx, ctxMenu: ctxMenu, dragImg: dragImg, dragTip: dragTip, mod: mod, keycard: keycard, editor: editor, prow: prow,
    sidebar: sidebar, find: find, density: qsDensity, wire: wireControls, batchModel: batchModel };
})();
