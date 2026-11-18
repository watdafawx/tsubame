/*
  Tsubame window mock builder. Plain DOM strings; all look comes from mock.css + tokens.css.
  Only used to avoid pasting the same 60 rows into every state. The markup it emits is
  what the U-tasks should match: see the class names in mock.css (each maps to one
  fill / stroke / text call in the renderer).

  Tsu.win(opts) -> html string for one window. opts (all optional):
    w, h        window size in px (default 1280 x 720)
    tabs        [{t, active, hover}]
    path        ['C:','Users','Alex','Downloads']   breadcrumb segments
    hoverCrumb  index of the hovered crumb
    pathEdit    {text, sugg:[names], hot:index, base:'C:\\Users\\Alex\\'}  edit mode
    loading     {pct, count}                        progress line + partial list
    empty       true                                empty folder
    filter      'inv'                               filter-as-you-type
    sel         'multi' | 'single' | 'none'
    menu        {x, y}                              context menu at point
    zebra       true                                row-alt striping
*/
(function () {
  'use strict';

  /* ---------- icons: 16 px stroke glyphs (the real app draws the same with lines, or an icon font) ---------- */
  var IC = {
    back: '<path d="M13 8H3.5M7.5 3.5 3 8l4.5 4.5"/>',
    fwd: '<path d="M3 8h9.5M8.5 3.5 13 8l-4.5 4.5"/>',
    up: '<path d="M8 13V3.5M3.5 7.5 8 3l4.5 4.5"/>',
    chev: '<path d="M6 3.5 10.5 8 6 12.5"/>',
    chevd: '<path d="M3.5 6 8 10.5 12.5 6"/>',
    plus: '<path d="M8 3v10M3 8h10"/>',
    x: '<path d="M4 4l8 8M12 4l-8 8"/>',
    search: '<circle cx="7" cy="7" r="4"/><path d="M10 10l3.5 3.5"/>',
    home: '<path d="M2 8 8 2.5 14 8M3.8 7v6.5h8.4V7"/>',
    desktop: '<rect x="2" y="3" width="12" height="8" rx="1"/><path d="M6 14h4M8 11v3"/>',
    download: '<path d="M8 2.5v8M4.5 7 8 10.5 11.5 7M3 13.5h10"/>',
    docs: '<path d="M3.5 1.8h6L13 5.3v8.9H3.5zM9.5 1.8v3.5H13M5.5 8.5h5M5.5 11h5"/>',
    pics: '<rect x="2" y="3" width="12" height="10" rx="1"/><circle cx="5.5" cy="6.5" r="1"/><path d="M2.5 12 6.5 8.5 9 10.5 11 8.5 13.5 11"/>',
    music: '<path d="M6 11.5V3.5l7-1.5v8"/><circle cx="4.5" cy="11.5" r="1.7"/><circle cx="11.5" cy="10" r="1.7"/>',
    video: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M6.5 6v4l3.5-2z"/>',
    drive: '<rect x="1.8" y="5" width="12.4" height="6" rx="1.5"/><circle cx="11.5" cy="8" r=".6"/>',
    star: '<path d="M8 2l1.8 3.8 4.2.5-3.1 2.9.8 4.1L8 11.2 4.3 13.3l.8-4.1L2 6.3l4.2-.5z"/>',
    net: '<rect x="2" y="3" width="12" height="7" rx="1"/><path d="M8 10v3M5 13h6"/>',
    cut: '<circle cx="4.5" cy="12" r="1.8"/><circle cx="11.5" cy="12" r="1.8"/><path d="M5.6 10.5 11 2.5M10.4 10.5 5 2.5"/>',
    copy: '<rect x="5.5" y="5.5" width="8" height="8" rx="1"/><path d="M10.5 5.5v-2a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2"/>',
    paste: '<rect x="3" y="3" width="10" height="11" rx="1"/><path d="M6 3V2h4v1M6 7.5h4M6 10.5h4"/>',
    rename: '<path d="M3 13l.7-3L11 2.7 13.3 5 6 12.3zM9.6 4.2l2.2 2.2"/>',
    trash: '<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 9h5.8l.6-9M7 7v4M9 7v4"/>',
    info: '<circle cx="8" cy="8" r="6"/><path d="M8 7.2v4.3M8 4.9v.1"/>',
    open: '<path d="M2 13V4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V13z"/>',
    newtab: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M8 5.5v5M5.5 8h5"/>',
    openwith: '<rect x="2.5" y="2.5" width="11" height="11" rx="1.5"/><path d="M6 10 10 6M7 6h3v3"/>',
    path: '<path d="M6 4 2.5 8 6 12M10 4l3.5 4L10 12"/>',
    shell: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M4.5 6.5 7 8.5 4.5 10.5M8.5 10.5h3"/>'
  };
  function ic(name, size, extra) {
    size = size || 16;
    return '<svg class="ic ' + (extra || '') + '" width="' + size + '" height="' + size + '" viewBox="0 0 16 16">' + IC[name] + '</svg>';
  }
  // U4: expose the helpers mock-u4.js reuses (one line, nothing else in this file changed)
  window.TsuInternals = { IC: IC, ic: function (n, s, e) { return ic(n, s, e); }, folderIcon: function (s) { return folderIcon(s); }, fileIcon: function (k) { return fileIcon(k); }, logo: function () { return logo(); }, winctl: function (n) { return winctl(n); }, esc: function (s) { return esc(s); }, fmtSize: function (b) { return fmtSize(b); } };
  function winctl(name) {
    var p = { min: '<path d="M0 5h10"/>', max: '<rect x=".5" y=".5" width="9" height="9"/>', close: '<path d="M0 0l10 10M10 0 0 10"/>' }[name];
    return '<svg class="ic" width="10" height="10" viewBox="0 0 10 10" style="stroke-width:1">' + p + '</svg>';
  }
  function sortArrow(up) {
    // 8x5 filled triangle, drawn as a path (no glyph font needed)
    return '<svg width="8" height="5" viewBox="0 0 8 5"><path class="f" fill="currentColor" d="' + (up ? 'M0 5 4 0 8 5z' : 'M0 0h8L4 5z') + '"/></svg>';
  }
  function folderIcon(size) {
    size = size || 16;
    return '<svg width="' + size + '" height="' + size + '" viewBox="0 0 16 16">' +
      '<path d="M1.2 3.4c0-.7.5-1.2 1.2-1.2h3.4l1.5 1.7h6.3c.7 0 1.2.5 1.2 1.2v1H1.2z" fill="var(--c-folder)" fill-opacity=".7"/>' +
      '<path d="M1.2 5.2c0-.7.5-1.2 1.2-1.2h11.2c.7 0 1.2.5 1.2 1.2v7.4c0 .7-.5 1.2-1.2 1.2H2.4c-.7 0-1.2-.5-1.2-1.2z" fill="var(--c-folder)"/></svg>';
  }
  // placeholder for shell icons (those are bitmaps from the shell at run time)
  var KIND = { pdf: '#d9534f', img: '#2fb3a2', zip: '#d4a017', exe: '#6f8fd8', doc: '#3b78d1', xls: '#3aa564', txt: '#8f98a8', code: '#a77bd9', av: '#d9718f', iso: '#8f98a8', data: '#e07b39' };
  function fileIcon(kind) {
    var c = KIND[kind] || KIND.txt;
    return '<svg width="16" height="16" viewBox="0 0 16 16"><path d="M3 1.5h6.2L13 5.3V14a.5.5 0 0 1-.5.5h-9A.5.5 0 0 1 3 14z" fill="' + c + '"/>' +
      '<path d="M9.2 1.5v3.8H13z" fill="#fff" fill-opacity=".45"/></svg>';
  }

  /* ---------- data: a Downloads folder (folders first, name ascending) ---------- */
  var KB = 1024, MB = KB * KB, GB = MB * KB;
  var FOLDERS = [
    ['Installers', '2026-11-12 09:41'], ['Invoices', '2026-11-03 17:20'], ['ISOs', '2026-10-21 22:08'],
    ['Screenshots', '2026-11-14 18:22'], ['tsubame-assets', '2026-11-16 21:04'], ['Unsorted', '2026-09-30 11:15']
  ].map(function (f) { return { n: f[0], d: f[1], t: 'File folder', s: null, k: 'dir' }; });
  var FILES = [
    ['0412-receipt.pdf', '2026-04-12 10:03', 'PDF document', 184 * KB, 'pdf'],
    ['blink-0.9.2-x64.zip', '2026-11-08 20:47', 'ZIP archive', 2.4 * MB, 'zip'],
    ['Cascadia-Code-2407.24.zip', '2026-08-02 14:30', 'ZIP archive', 11.8 * MB, 'zip'],
    ['crash-report-2026-11-09.txt', '2026-11-09 03:12', 'Text document', 48 * KB, 'txt'],
    ['dataset_v2.csv', '2026-10-27 16:55', 'CSV file', 38.2 * MB, 'data'],
    ['Final inv draft.docx', '2026-10-30 13:09', 'Word document', 96 * KB, 'doc'],
    ['flyer-print.psd', '2026-09-11 19:36', 'PSD file', 142 * MB, 'img'],
    ['ghidra_11.2_PUBLIC.zip', '2026-07-19 08:20', 'ZIP archive', 418 * MB, 'zip'],
    ['inventory.xlsx', '2026-11-05 12:44', 'Excel workbook', 212 * KB, 'xls'],
    ['invite-list.txt', '2026-10-02 21:01', 'Text document', 3 * KB, 'txt'],
    ['Invoice-Acme-2026-10.pdf', '2026-10-31 09:15', 'PDF document', 311 * KB, 'pdf'],
    ['invoice_scan_0412.jpg', '2026-04-12 10:07', 'JPEG image', 2.9 * MB, 'img'],
    ['logo-final-v3.png', '2026-11-15 23:48', 'PNG image', 86 * KB, 'img'],
    ['Minecraft-1.21.4-server.jar', '2026-11-01 18:30', 'Executable Jar File', 54.6 * MB, 'exe'],
    ['notes.md', '2026-11-16 20:55', 'Markdown file', 6 * KB, 'txt'],
    ['ReShade_Setup_6.1.1.exe', '2026-09-25 15:22', 'Application', 3.1 * MB, 'exe'],
    ['rustup-init.exe', '2026-08-14 10:48', 'Application', 11.4 * MB, 'exe'],
    ['Screenshot 2026-11-14 at 18.22.07 - the very long exported name that keeps going and going so it has to be cut with an ellipsis at the column edge - final copy (2).png', '2026-11-14 18:22', 'PNG image', 1.8 * MB, 'img'],
    ['setup-obs-30.2.exe', '2026-10-09 20:13', 'Application', 29.7 * MB, 'exe'],
    ['spotify-export.json', '2026-11-11 07:58', 'JSON file', 9.4 * MB, 'data'],
    ['Steam-Invite.png', '2026-10-19 22:40', 'PNG image', 412 * KB, 'img'],
    ['trailer-cut-04.mp4', '2026-11-13 02:16', 'MP4 video', 1.12 * GB, 'av'],
    ['tsubame-icon.ico', '2026-11-16 22:31', 'Icon', 94 * KB, 'img'],
    ['wallpaper-4k-03.webp', '2026-09-03 17:09', 'WEBP image', 3.6 * MB, 'img'],
    ['Win11_24H2_English_x64.iso', '2026-10-21 22:05', 'Disc image file', 5.84 * GB, 'iso'],
    ['yt-dlp.exe', '2026-11-10 13:37', 'Application', 17.2 * MB, 'exe'],
    ['Zoom-Installer.exe', '2026-06-22 09:02', 'Application', 14.8 * MB, 'exe']
  ].map(function (f) { return { n: f[0], d: f[1], t: f[2], s: f[3], k: f[4] }; });
  var DATA = FOLDERS.concat(FILES);

  var SIDE = {
    quick: [['home', 'Home'], ['desktop', 'Desktop'], ['download', 'Downloads', 'cur'], ['docs', 'Documents'], ['pics', 'Pictures']],
    drives: [['Windows (C:)', '118 GB', 75], ['Dev (X:)', '1.1 TB', 70], ['Games (G:)', '41 GB', 97]],
    marks: [['star', 'tsubame'], ['star', 'blink'], ['star', 'Minecraft packs'], ['net', 'rig']]
  };

  function fmtSize(b) {
    if (b == null) return '';
    if (b < KB) return b + ' B';
    if (b < MB) return Math.round(b / KB) + ' KB';
    if (b < GB) return (b / MB).toFixed(b < 10 * MB ? 1 : 0).replace(/\.0$/, '') + ' MB';
    return (b / GB).toFixed(2) + ' GB';
  }
  function esc(s) { return s.replace(/&/g, '&amp;').replace(/</g, '&lt;'); }
  function mark(name, f) {
    if (!f) return esc(name);
    var i = name.toLowerCase().indexOf(f.toLowerCase());
    if (i < 0) return esc(name);
    return esc(name.slice(0, i)) + '<mark>' + esc(name.slice(i, i + f.length)) + '</mark>' + esc(name.slice(i + f.length));
  }

  /* ---------- pieces ---------- */
  function logo() {
    // small-cut app mark, same path as res/tsubame-small.svg
    return '<svg class="logo" viewBox="0 0 100 100"><rect width="100" height="100" rx="22" fill="var(--brand-navy)"/>' +
      '<g transform="translate(13 13) scale(.74)"><g transform="rotate(40 50 53) translate(2 3) scale(.96)">' +
      '<path fill="var(--brand-cream)" d="M50 12C57 12 61 18 61 26C74 28 87 38 94 54C82 47 72 46 63 48C64 62 68 78 74 94C63 86 55 78 50 70C45 78 37 86 26 94C32 78 36 62 37 48C28 46 18 47 6 54C13 38 26 28 39 26C39 18 43 12 50 12Z"/></g></g></svg>';
  }
  function titlebar(o) {
    var tabs = o.tabs || [{ t: 'Downloads', active: true }, { t: 'tsubame', hover: true }, { t: 'Documents' }];
    var h = logo() + '<div class="tabs">';
    tabs.forEach(function (t) {
      h += '<div class="tab' + (t.active ? ' active' : '') + (t.hover ? ' hover' : '') + '">' + folderIcon(14) +
        '<span class="t">' + esc(t.t) + '</span><span class="x' + (t.hover ? ' hot' : '') + '">' + ic('x', 10) + '</span></div>';
    });
    h += '</div><div class="newtab">' + ic('plus', 14) + '</div><div class="drag"></div>' +
      '<div class="wc"><span>' + winctl('min') + '</span><span>' + winctl('max') + '</span><span class="close">' + winctl('close') + '</span></div>';
    return '<div class="titlebar">' + h + '</div>';
  }
  function pathbar(o) {
    var h = '';
    if (o.pathEdit) {
      var pe = o.pathEdit;
      h = '<div class="pathbar edit">' + folderIcon(16).replace('<svg', '<svg style="margin-right:6px"') +
        '<div class="pathtext">' + esc(pe.text) + '<span class="caret"></span></div>' +
        '<div class="popup" style="left:-1px;right:-1px;top:30px"><div style="display:flex;flex-direction:column">';
      pe.sugg.forEach(function (s, i) {
        h += '<div class="sug' + (i === pe.hot ? ' hot' : '') + '">' + folderIcon(16) + '<span class="t">' + esc(pe.base) + '<b style="font-weight:600">' + esc(s) + '</b></span></div>';
      });
      h += '</div></div></div>';
      return h;
    }
    var segs = o.path || ['C:', 'Users', 'Alex', 'Downloads'];
    h = '<div class="pathbar">' + ic('drive', 16) + '';
    segs.forEach(function (s, i) {
      if (i) h += '<span class="sep">' + ic('chev', 12) + '</span>';
      h += '<span class="crumb' + (i === segs.length - 1 ? ' last' : '') + (o.hoverCrumb === i ? ' hot' : '') + '">' + esc(s) + '</span>';
    });
    return h + '</div>';
  }
  function filterbox(o) {
    if (o.filter != null) {
      return '<div class="filter on">' + ic('search', 16) + '<span class="val">' + esc(o.filter) + '<span class="caret" style="margin-left:0"></span></span>' + '<span class="dim" style="display:grid">' + ic('x', 14) + '</span></div>';
    }
    return '<div class="filter">' + ic('search', 16) + '<span class="ph">Filter</span><span class="kbd">Ctrl+F</span></div>';
  }
  function toolbar(o) {
    var h = '<div class="btn hot">' + ic('back') + '</div><div class="btn off">' + ic('fwd') + '</div><div class="btn">' + ic('up') + '</div><div class="navgap"></div>';
    h += pathbar(o) + filterbox(o);
    if (o.loading) h += '<i class="progress" style="width:' + Math.round(o.loading.pct * 100) + '%"></i>';
    return '<div class="toolbar">' + h + '</div>';
  }
  function sidebar(o) {
    var h = '<div class="sh">' + ic('chevd', 12) + 'Quick access</div>';
    SIDE.quick.forEach(function (q) {
      h += '<div class="si' + (q[2] === 'cur' && o.curDrive == null ? ' cur' : '') + '">' + ic(q[0]) + '<span class="t">' + q[1] + '</span></div>';
    });
    h += '<div class="sh" style="margin-top:6px">' + ic('chevd', 12) + 'Drives</div>';
    SIDE.drives.forEach(function (d, i) {
      h += '<div class="drive' + (o.curDrive === i ? ' cur' : '') + '"><div class="r1">' + ic('drive') + '<span class="t" style="flex:1">' + d[0] + '</span><span class="free">' + d[1] + ' free</span></div>' +
        '<div class="bar"><i class="' + (d[2] > 90 ? 'full' : '') + '" style="width:' + d[2] + '%"></i></div></div>';
    });
    h += '<div class="sh" style="margin-top:6px">' + ic('chevd', 12) + 'Bookmarks</div>';
    SIDE.marks.forEach(function (m, i) {
      h += '<div class="si' + (o.hoverMark === i ? ' hover' : '') + '">' + ic(m[0]) + '<span class="t">' + m[1] + '</span></div>';
    });
    return '<div class="sidebar">' + h + '</div>';
  }
  function colhead(o) {
    return '<div class="colhead">' +
      '<div class="ch c-name sorted"><span>Name</span>' + sortArrow(true) + '</div>' +
      '<div class="ch c-date">Date modified</div>' +
      '<div class="ch c-type">Type</div>' +
      '<div class="ch c-size r">Size</div></div>';
  }
  function row(r, cls, f) {
    return '<div class="row ' + cls + '"><div class="cell name c-name">' + (r.k === 'dir' ? folderIcon(16) : fileIcon(r.k)) +
      '<span class="t">' + mark(r.n, f) + '</span></div>' +
      '<div class="cell c-date"><span class="t">' + r.d + '</span></div>' +
      '<div class="cell c-type"><span class="t">' + r.t + '</span></div>' +
      '<div class="cell r c-size"><span class="t">' + fmtSize(r.s) + '</span></div></div>';
  }
  function scrollbar(total, listH, hot) {
    var content = total * 22;
    if (content <= listH) return '<div class="sbar"></div>';
    var th = Math.max(24, Math.round(listH * listH / content));
    return '<div class="sbar"><i class="' + (hot ? 'hot' : '') + '" style="top:2px;height:' + (th - 4) + 'px"></i></div>';
  }
  function fixtureRows(n) {
    var ext = [['.dat', 'DAT file', 'txt'], ['.bin', 'BIN file', 'txt'], ['.log', 'Text document', 'txt'], ['.png', 'PNG image', 'img']];
    var out = [];
    for (var i = 1; i <= n; i++) {
      var e = ext[i % 4];
      out.push({ n: 'file-' + ('00000' + i).slice(-6) + e[0], d: '2026-11-16 21:' + ('0' + (i % 60)).slice(-2), t: e[1], s: ((i * 7919) % 900 + 12) * KB, k: e[2] });
    }
    return out;
  }

  /* ---------- context menu ---------- */
  var MENU = [
    ['open', 'Open', 'Enter'], ['newtab', 'Open in new tab', 'Ctrl+Enter'], ['openwith', 'Open with', '\u25B8'], '-',
    ['cut', 'Cut', 'Ctrl+X'], ['copy', 'Copy', 'Ctrl+C', 'hot'], ['paste', 'Paste', 'Ctrl+V', 'off'], ['path', 'Copy as path', 'Ctrl+Shift+C'], '-',
    ['rename', 'Rename', 'F2'], ['trash', 'Delete', 'Del', 'danger'], '-',
    ['shell', 'Show more options', 'Shift+F10'], ['info', 'Properties', 'Alt+Enter']
  ];
  function menu(x, y) {
    var h = '<div class="popup" style="left:' + x + 'px;top:' + y + 'px;width:var(--layout-menu-width)">';
    MENU.forEach(function (m) {
      if (m === '-') { h += '<div class="msep"><i></i></div>'; return; }
      var flag = m[3] || '';
      h += '<div class="mi ' + flag + '">' + ic(m[0]) + '<span class="t">' + m[1] + '</span><span class="k">' + m[2] + '</span></div>';
    });
    return h + '</div>';
  }

  /* ---------- window ---------- */
  function win(o) {
    o = o || {};
    var W = o.w || 1280, H = o.h || 720;
    var listH = H - 34 - 40 - 24 - 24;
    var rows = DATA, filt = o.filter;
    if (o.loading) rows = fixtureRows(o.loading.shown || 16);
    if (filt) rows = DATA.filter(function (r) { return r.n.toLowerCase().indexOf(filt.toLowerCase()) >= 0; });
    if (o.empty) rows = [];

    // selection state
    var sel = {}, focus = -1, hover = -1;
    if (!o.empty && !o.loading) {
      if (filt) { sel = {}; focus = 0; hover = 3; }
      else if (o.sel === 'single') { sel = { 11: 1 }; focus = 11; hover = 20; }
      else if (o.sel === 'none') { focus = 0; hover = 20; }
      else if (o.menu) { sel = { 9: 1, 10: 1, 11: 1, 15: 1 }; focus = 11; hover = -1; }
      else { sel = { 9: 1, 10: 1, 11: 1, 15: 1 }; focus = 17; hover = 21; }
    }
    if (o.rowState) { sel = o.rowState.sel || {}; focus = o.rowState.focus; hover = o.rowState.hover; }

    var rh = '';
    rows.forEach(function (r, i) {
      var cls = [];
      if (o.zebra && i % 2) cls.push('alt');
      if (sel[i]) cls.push('sel');
      if (i === hover && !sel[i]) cls.push('hover');
      if (i === focus) cls.push('focus');
      rh += row(r, cls.join(' '), filt);
    });

    var emptyHtml = o.empty ? '<div class="empty">' + folderIcon(40) +
      '<div class="big">This folder is empty</div><div class="hint">Drop files here, or press <b>Ctrl+Shift+N</b> for a new folder</div></div>' : '';

    // status bar
    var n = rows.length, left, mid = '', right;
    var picked = Object.keys(sel).map(function (k) { return rows[+k]; });
    if (o.loading) left = 'Reading folder\u2026 ' + o.loading.count.toLocaleString('en-US') + ' items';
    else if (filt) left = '<span class="em">' + n + '</span> of ' + DATA.length + ' items match \u201C' + esc(filt) + '\u201D';
    else left = n + ' item' + (n === 1 ? '' : 's');
    if (picked.length) {
      var tot = picked.reduce(function (a, r) { return a + (r.s || 0); }, 0);
      mid = '<span class="v"></span><span class="em">' + picked.length + ' selected</span><span>' + fmtSize(tot) + '</span>';
    }
    right = o.loading ? '' : '118 GB free of 476 GB';

    var html = '<div class="win' + (o.zebra ? '' : ' zebra-off') + '" style="width:' + W + 'px;height:' + H + 'px">' +
      titlebar(o) + toolbar(o) +
      '<div class="body">' + sidebar(o) +
      '<div class="main">' + colhead(o) +
      '<div class="list"><div class="rows">' + rh + '</div>' + emptyHtml + scrollbar(o.loading ? 100000 : rows.length, listH, false) + '</div></div></div>' +
      '<div class="status"><span>' + left + '</span>' + mid + '<span class="push">' + right + '</span></div>' +
      (o.menu ? menu(o.menu.x, o.menu.y) : '') + '</div>';
    return html;
  }

  /* ---------- row-state specimens (for the states sheet) ---------- */
  function rowSpecimens() {
    var r = DATA[9];
    function one(label, cls) {
      return '<div style="display:flex;align-items:center;gap:12px"><span style="width:200px;font-size:11px;color:var(--c-text-dim)">' + label + '</span>' +
        '<div class="rowspec" style="width:760px;position:relative;background:var(--c-row)"><div class="rows" style="position:static">' + row(r, cls, '') + '</div></div></div>';
    }
    return '<div class="win" style="width:auto;height:auto;background:transparent;overflow:visible;gap:6px;flex-direction:column">' +
      one('normal', '') + one('alt (zebra, optional)', 'alt') + one('hover', 'hover') + one('selected', 'sel') +
      one('selected, list not focused', 'sel inactive') + one('focus (keyboard cursor)', 'focus') +
      one('focus + selected', 'sel focus') + one('focus + hover', 'hover focus') + '</div>';
  }

  /* U3: internals shared with mock-u3.js (sidebar, panes, rename mocks). Nothing above changed. */
  window.TsuParts = { ic: ic, folderIcon: folderIcon, fileIcon: fileIcon, fmtSize: fmtSize, esc: esc, mark: mark, sortArrow: sortArrow, winctl: winctl, logo: logo, titlebar: titlebar, pathbar: pathbar, filterbox: filterbox, scrollbar: scrollbar };

  /* ---------- page wiring ---------- */
  function qs(k, d) { var m = new RegExp('[?&]' + k + '=([^&]*)').exec(location.search); return m ? decodeURIComponent(m[1]) : d; }
  function setTheme(t) {
    document.documentElement.setAttribute('data-theme', t);
    var b = document.querySelectorAll('[data-theme-btn]');
    for (var i = 0; i < b.length; i++) b[i].classList.toggle('on', b[i].getAttribute('data-theme-btn') === t);
  }
  window.Tsu = { win: win, qs: qs, setTheme: setTheme, rowSpecimens: rowSpecimens, data: DATA };
})();
