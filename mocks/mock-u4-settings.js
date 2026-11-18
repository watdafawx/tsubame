/*
  U4 settings pages. Needs mock.js, mock-u4.js. Settings open as a full page in a tab ("Settings" tab,
  Ctrl+,), not a modal. Left nav + scrolling content. Every control applies live: there is no Save button.

  Tsu4S.page(name, opts) -> html of the whole 1280 x h window showing that settings page.
  names: general, appearance, context, context-add, keys, keys-record, updates, updates-available,
         default, advanced, states
*/
(function () {
  'use strict';
  var U = window.Tsu4, T = U.T, ic = U.ic, esc = U.esc, kc = U.kc, keys = U.keys, tog = U.tog, seg = U.seg, btn = U.btn, field = U.field, dd = U.dd, slider = U.slider, chk = U.chk;
  T.IC.menu = '<path d="M3 4.5h10M3 8h10M3 11.5h6"/>';
  T.IC.cloud = '<path d="M4.5 12.5a3 3 0 0 1-.3-6A4 4 0 0 1 12 6.5a2.9 2.9 0 0 1-.5 6z"/>';

  /* ---------- layout helpers ---------- */
  function sh(t) { return '<div class="sg-h">' + t + '</div>'; }
  function row(nm, ds, ctl, cls) {
    return '<div class="srow ' + (cls || '') + '"><div class="lab"><div class="nm">' + nm + '</div>' + (ds ? '<div class="ds">' + ds + '</div>' : '') + '</div><div class="ctl2">' + (ctl || '') + '</div></div>';
  }
  function grp(rows, cls) { return '<div class="grp ' + (cls || '') + '">' + rows + '</div>'; }
  function ph(title, sub) { return '<h1 class="pg-h">' + title + '</h1><p class="pg-s">' + sub + '</p>'; }
  function note(icon, html, cls) { return '<div class="note ' + (cls || '') + '">' + ic(icon, 14) + '<div>' + html + '</div></div>'; }
  function banner(html, buttons) { return '<div class="banner">' + ic('info', 16) + '<div class="t2">' + html + '</div>' + (buttons || '') + '</div>'; }

  /* ---------- window shell ---------- */
  var NAV = [['sliders', 'General'], ['brush', 'Appearance'], ['menu', 'Context menu'], ['keyboard', 'Keys'], ['split', 'Panes and tabs'], ['search', 'Search'], ['download', 'Updates'], ['explorer', 'Default file manager'], ['wrench', 'Advanced']];
  function nav(cur, o) {
    o = o || {};
    var h = '<div class="set-nav"><div class="find">' + ic('search', 14) + '<span class="ph">Find a setting</span>' + kc('Ctrl+F') + '</div>';
    NAV.forEach(function (n, i) {
      h += '<div class="ni' + (n[1] === cur ? ' cur' : '') + (o.hoverNav === n[1] ? ' hover' : '') + '">' + ic(n[0]) + '<span class="t">' + n[1] + '</span>' + (o.dot === n[1] ? '<i class="dot"></i>' : '') + '</div>';
    });
    return h + '<div class="ver">Tsubame 0.4.2 \u00B7 settings.toml</div></div>';
  }
  function titlebar() {
    return '<div class="titlebar">' + T.logo() + '<div class="tabs">' +
      '<div class="tab">' + T.folderIcon(14) + '<span class="t">Downloads</span><span class="x">' + ic('x', 10) + '</span></div>' +
      '<div class="tab active"><span class="gear">' + ic('gear', 14) + '</span><span class="t">Settings</span><span class="x">' + ic('x', 10) + '</span></div></div>' +
      '<div class="newtab">' + ic('plus', 14) + '</div><div class="drag"></div>' +
      '<div class="wc"><span>' + T.winctl('min') + '</span><span>' + T.winctl('max') + '</span><span class="close">' + T.winctl('close') + '</span></div></div>';
  }
  function win(navCur, content, h, thumb, o) {
    thumb = thumb || [2, 120];
    return '<div class="win zebra-off" style="width:1280px;height:' + h + 'px">' + titlebar() + '<div class="body">' + nav(navCur, o) +
      '<div class="set-main"><div class="set-page"><div class="set-in">' + content + '</div></div><div class="sbar"><i style="top:' + thumb[0] + 'px;height:' + thumb[1] + 'px"></i></div></div></div></div>';
  }

  /* ================================================================ General */
  function general() {
    var c = ph('General', 'Settings apply as soon as you change them. They are stored in <span class="mono">%APPDATA%\\Tsubame\\settings.toml</span>.');
    c += sh('Startup') + grp(
      row('Start folder', 'Where a new window opens when there is no session to restore.', dd('A specific folder', false, null, 0, 188)) +
      row('Folder to open', null, field('C:\\Users\\Alex\\Downloads', 'grow', { w: 330, icon: 'folder' }) + btn('Browse\u2026')) +
      row('Restore previous session', 'Reopen the tabs, panes and scroll positions you had when you closed Tsubame.', tog(true))
    );
    c += sh('Files and folders') + grp(
      row('Show hidden files', 'Same as Ctrl+H. Items with the hidden attribute are drawn in a dimmer colour.', tog(false)) +
      row('Show protected system files', 'Hidden and system at once: <span class="mono">pagefile.sys</span>, <span class="mono">desktop.ini</span>. Leave off unless you know why you need them.', tog(false)) +
      row('Show file extensions', 'Always show <span class="mono">.png</span>, <span class="mono">.exe</span> and so on in the Name column.', tog(true))
    );
    c += sh('Deleting') + grp(
      row('When I press Delete', 'Shift+Delete does the other one. Items moved to the Recycle Bin can be restored with Ctrl+Z.', seg(['Move to Recycle Bin', 'Delete permanently'], 0)) +
      row('Confirm before deleting', 'Ask first when moving items to the Recycle Bin. Permanent deletes always ask, whatever this says.', tog(true)) +
      row('Skip the Recycle Bin on network drives', 'Windows cannot recycle there anyway. This only stops the extra warning.', tog(true))
    );
    return win('General', c, 760, [2, 330]);
  }

  /* ================================================================ Appearance */
  // [name, dark?, bg, panel, surface, text, muted, accent]. Swatches are other apps' public palettes (from hopper's theme list); the two Tsubame cards copy tokens.json.
  var THEMES = {
    tsubame: [['Tsubame Dark', 1, '#0b0f16', '#121722', '#0e131b', '#e9e6df', '#a3adbf', '#6aa0ff'], ['Tsubame Light', 0, '#dde2ea', '#eef0f4', '#ffffff', '#161b26', '#525d72', '#1b52bf']],
    dark: [
      ['Catppuccin Mocha', 1, '#1e1e2e', '#181825', '#262637', '#cdd6f4', '#a6adc8', '#cba6f7'], ['Catppuccin Macchiato', 1, '#24273a', '#1e2030', '#2b2e43', '#cad3f5', '#a5adcb', '#c6a0f6'],
      ['Ros\u00E9 Pine', 1, '#191724', '#16141f', '#1f1d2e', '#e0def4', '#908caa', '#ebbcba'], ['Ros\u00E9 Pine Moon', 1, '#232136', '#1d1b2e', '#2a273f', '#e0def4', '#908caa', '#ea9a97'],
      ['Tokyo Night', 1, '#1a1b26', '#16161e', '#1f2335', '#c0caf5', '#9aa5ce', '#7aa2f7'], ['Dracula', 1, '#282a36', '#21222c', '#303241', '#f8f8f2', '#a3a7c2', '#bd93f9'],
      ['Nord', 1, '#2e3440', '#272c36', '#353c4a', '#eceff4', '#a3acbf', '#88c0d0'], ['Gruvbox Dark', 1, '#282828', '#1d2021', '#32302f', '#ebdbb2', '#a89984', '#fabd2f'],
      ['Everforest', 1, '#2d353b', '#272e33', '#343f44', '#d3c6aa', '#9da9a0', '#a7c080'], ['One Dark', 1, '#282c34', '#21252b', '#2c313a', '#abb2bf', '#8b92a0', '#61afef'],
      ['Ristretto', 1, '#2c2525', '#211c1c', '#352e2e', '#fff1f3', '#c3b7b8', '#f9cc6c'], ['Kanagawa', 1, '#1f1f28', '#16161d', '#2a2a37', '#dcd7ba', '#a09c8c', '#7e9cd8']],
    light: [
      ['Catppuccin Latte', 0, '#eff1f5', '#e6e9ef', '#e6e9ef', '#4c4f69', '#6c6f85', '#8839ef'], ['Ros\u00E9 Pine Dawn', 0, '#faf4ed', '#f2e9e1', '#fffaf3', '#575279', '#797593', '#d7827e'],
      ['Tokyo Night Day', 0, '#e1e2e7', '#d0d5e3', '#e9e9ed', '#3760bf', '#6172b0', '#2e7de9'], ['Gruvbox Light', 0, '#fbf1c7', '#f2e5bc', '#f9f5d7', '#3c3836', '#7c6f64', '#af3a03'],
      ['Solarized Light', 0, '#fdf6e3', '#eee8d5', '#f5efdc', '#475b62', '#657b83', '#268bd2']]
  };
  function hex(h) { return [parseInt(h.substr(1, 2), 16), parseInt(h.substr(3, 2), 16), parseInt(h.substr(5, 2), 16)]; }
  function mix(a, b, t) { var A = hex(a), B = hex(b); return '#' + [0, 1, 2].map(function (i) { return ('0' + Math.round(A[i] * t + B[i] * (1 - t)).toString(16)).slice(-2); }).join(''); }
  function themeCard(t, cur, hover) {
    var bg = t[2], pn = t[3], sf = t[4], tx = t[5], mu = t[6], ac = t[7];
    var p = '<div class="thp" style="background:' + sf + '">' +
      '<div class="side" style="background:' + pn + '"><i class="acc" style="top:8px;background:' + ac + '"></i><i class="ln" style="left:4px;top:19px;width:22px;background:' + mu + '"></i><i class="ln" style="left:4px;top:28px;width:16px;background:' + mu + '"></i><i class="ln" style="left:4px;top:37px;width:19px;background:' + mu + '"></i></div>' +
      '<div class="head" style="background:' + pn + '"></div>' +
      '<i class="ln" style="left:42px;top:19px;width:54px;background:' + tx + '"></i>' +
      '<div class="sel" style="top:26px;background:' + mix(ac, sf, 0.34) + '"></div><i class="ln" style="left:42px;top:29.5px;width:68px;background:' + tx + '"></i>' +
      '<i class="ln" style="left:42px;top:46px;width:44px;background:' + tx + '"></i><i class="ln" style="left:42px;top:56px;width:60px;background:' + mu + '"></i>' +
      '<i class="dotc" style="background:' + ac + '"></i></div>';
    return '<div class="thc' + (cur ? ' cur' : '') + (hover ? ' hover' : '') + '">' + p + '<div class="nm"><span class="t">' + esc(t[0]) + '</span>' + (cur ? ic('check', 14) : '') + '</div></div>';
  }
  function themeGrid(list, curName, hoverName) { return '<div class="th-grid">' + list.map(function (t) { return themeCard(t, t[0] === curName, t[0] === hoverName); }).join('') + '</div>'; }

  var DENS = [['Compact', 22, 'height-list-row'], ['Normal', 26, 'height-list-row-normal'], ['Comfortable', 30, 'height-list-row-comfortable']];
  function densPreview(cur, zebra) {
    var D = Tsu.data;
    var h = '<div class="dens">';
    DENS.forEach(function (d, i) {
      var rows = '';
      [D[0], D[6], D[14], D[16], D[22]].forEach(function (r, ri) {
        var cls = (zebra && ri % 2 ? 'alt ' : '') + (ri === 2 ? 'sel' : '');
        rows += '<div class="row ' + cls + '" style="height:var(--' + d[2] + ');line-height:var(--' + d[2] + ')"><div class="cell name c-name">' + (r.k === 'dir' ? T.folderIcon(16) : T.fileIcon(r.k)) + '<span class="t">' + esc(r.n) + '</span></div><div class="cell r c-size"><span class="t">' + esc(T.fmtSize(r.s)) + '</span></div></div>';
      });
      h += '<div class="dc' + (i === cur ? ' cur' : '') + '"><div class="dh"><b>' + d[0] + '</b>' + d[1] + ' px rows' + (i === 0 ? ' \u00B7 default' : '') + '</div><div class="dl' + (zebra ? '' : ' zebra-off') + '">' + rows + '</div></div>';
    });
    return h + '</div>';
  }
  function iconOpt(name, desc, samples, cur) { return '<div class="icopt' + (cur ? ' cur' : '') + '"><div class="samples">' + samples + '</div><div class="nm">' + name + (cur ? ic('check', 14) : '') + '</div><div class="ds">' + desc + '</div></div>'; }

  function appearance() {
    var c = ph('Appearance', 'Changes show immediately in every open window. Hover a theme to preview it, click to keep it.');
    c += sh('Theme') + grp(
      row('Colour mode', 'Follow Windows, or stay light or dark. Picking a theme below sets this to match it.', seg(['Match Windows', 'Light', 'Dark'], 2)) +
      '<div class="srow col"><div><div class="th-sec">Tsubame</div>' + themeGrid(THEMES.tsubame, 'Tsubame Dark') +
      '<div class="th-sec">Dark</div>' + themeGrid(THEMES.dark, 'Tsubame Dark', 'Nord') +
      '<div class="th-sec">Light</div>' + themeGrid(THEMES.light, 'Tsubame Dark') + '</div>' +
      '<div class="note">' + ic('info', 14) + '<div>Your own themes: drop a <code>.toml</code> pack into <span class="mono">%APPDATA%\\Tsubame\\themes</span>; it shows up here under \u201CYours\u201D. A pack can also add file-type colours.</div></div>' +
      '<div style="display:flex;gap:8px">' + btn('Open themes folder', '', 'folder') + btn('Reload', '', 'refresh') + '</div></div>'
    );
    var accs = ['#6aa0ff', '#f97316', '#e0492c', '#cba6f7', '#f38ba8', '#2fb3a2', '#a7c080', '#fabd2f'];
    c += sh('Accent colour') + grp(row('Accent', 'Selection bar, focus ring, links, toggles. \u201CTheme default\u201D uses the accent of the theme you picked.',
      '<div class="acc-row"><span class="accsw theme cur">' + ic('check', 12) + '</span>' + accs.map(function (a) { return '<span class="accsw" style="background:' + a + '"></span>'; }).join('') + '</div>'));
    c += sh('Lists') + grp(
      row('Row density', 'Switches on the fly, in every folder. Cycle with Ctrl+Shift+D.', seg(['Compact', 'Normal', 'Comfortable'], 0)) +
      '<div class="srow col"><div class="nm" style="font-size:var(--font-size-xs);color:var(--c-text-dim)">Live preview: the same rows at each density. The chosen one is outlined.</div>' + densPreview(0, true) + '</div>' +
      row('Zebra rows', 'Shade every second row slightly. Ctrl+Shift+Z.', tog(true)) +
      row('Font size', 'List and menu text. Windows text scaling is applied on top.', '<span class="sval" style="width:auto;color:var(--c-text)">Aa</span>' + slider(20) + '<span class="sval">12 px</span>') +
      '<div class="srow" style="border-bottom:0"><div class="lab"><div class="nm">Icon style</div><div class="ds">How files and folders are drawn in lists, the sidebar and tabs.</div></div></div>' +
      '<div class="srow col" style="border-top:0;padding-top:0"><div class="icons3">' +
        iconOpt('System icons', 'Real shell icons and thumbnails. Default.', T.folderIcon(20) + T.fileIcon('pdf') + T.fileIcon('img') + T.fileIcon('exe'), true) +
        iconOpt('Tsubame outline', 'Our own line icons, one colour per type.', ic('open', 20) + ic('docs', 20) + ic('pics', 20) + ic('shell', 20), false) +
        iconOpt('Monochrome', 'Dim single-colour icons, quietest.', '<span style="color:var(--c-text-dim);display:flex;gap:8px">' + ic('open', 20) + ic('docs', 20) + ic('pics', 20) + ic('shell', 20) + '</span>', false) +
      '</div></div>'
    );
    return win('Appearance', c, 1280, [2, 270]);
  }

  /* ================================================================ Context menu */
  // k: item | sub | sep ; src: Built-in | Windows | Custom | Nilesoft ; state flags: hidden, drag, hover, rename, child
  var CM = [
    { n: 'Open', ic: 'open', src: 'Built-in', sc: 'Enter' },
    { n: 'Open in new tab', ic: 'newtab', src: 'Built-in', sc: 'Ctrl+Enter' },
    { n: 'Open with', ic: 'openwith', src: 'Windows', arrow: true },
    { sep: true },
    { n: 'Cut', ic: 'cut', src: 'Built-in', sc: 'Ctrl+X' },
    { n: 'Copy', ic: 'copy', src: 'Built-in', sc: 'Ctrl+C' },
    { n: 'Paste', ic: 'paste', src: 'Built-in', sc: 'Ctrl+V' },
    { n: 'Copy as path', ic: 'path', src: 'Built-in', sc: 'Ctrl+Shift+C', drag: true },
    { sep: true },
    { n: 'Rename', ic: 'rename', src: 'Built-in', sc: 'F2' },
    { drop: true },
    { n: 'Delete', ic: 'trash', src: 'Built-in', sc: 'Del', danger: true },
    { sep: true },
    { n: 'Tools', ic: 'term', sub: true, count: 4, arrow: true },
    { n: 'Open terminal here', ic: 'term', src: 'Built-in', sc: 'Ctrl+Alt+T', child: true },
    { n: 'Open in VS Code', ic: 'path', src: 'Custom', child: true, rename: true },
    { n: 'Git Bash here', ic: 'term', src: 'Nilesoft', child: true, hover: true },
    { n: 'Compress to ZIP', ic: 'archive', src: 'Nilesoft', child: true, hidden: true },
    { sep: true },
    { n: 'Show more options', ic: 'shell', src: 'Windows', sc: 'Shift+F10' },
    { n: 'Properties', ic: 'info', src: 'Built-in', sc: 'Alt+Enter' }
  ];
  function cmRow(r) {
    if (r.drop) return '<div class="drop"></div>';
    if (r.sep) return '<div class="cmr sep"><span class="grip">' + ic('grip', 14) + '</span><i class="line"></i>' + '<span class="tag">Separator</span><span class="ibt">' + ic('eye', 14) + '</span><span class="ibt">' + ic('x', 12) + '</span></div>';
    var cls = 'cmr' + (r.hidden ? ' hidden' : '') + (r.drag ? ' drag' : '') + (r.hover ? ' hover' : '') + (r.sub ? ' sub' : '') + (r.child ? ' child' : '');
    var srcTag = '<span class="tag' + (r.src === 'Custom' ? ' acc' : '') + '">' + (r.src === 'Nilesoft' ? 'From Nilesoft' : r.src) + '</span>';
    var name = r.rename ? '<span class="fld" style="flex:1"><span class="v t">Open in VS Code<span class="caret"></span></span></span>' : '<span class="nm t">' + esc(r.n) + (r.sub ? ' <span style="color:var(--c-text-dim);font-size:var(--font-size-xs)">\u2014 submenu, ' + r.count + ' items</span>' : '') + '</span>';
    return '<div class="' + cls + '"><span class="grip">' + ic('grip', 14) + '</span>' + (r.sub ? ic('chevd', 12) : '') + ic(r.ic) + name +
      (r.sc ? '<span class="mono" style="color:var(--c-text-dim)">' + r.sc + '</span>' : '') + (r.sub ? '' : srcTag) +
      '<span class="ibt' + (r.hover ? ' hot' : '') + '">' + ic(r.hidden ? 'eyeoff' : 'eye', 14) + '</span>' +
      '<span class="ibt">' + ic(r.rename ? 'check' : 'dots', 14) + '</span></div>';
  }
  function cmPreview() {
    var h = '<div class="popup" style="width:100%">';
    var prevSep = true;
    CM.forEach(function (r) {
      if (r.drop || r.hidden) return;
      if (r.child) { return; }
      if (r.sep) { if (!prevSep) h += '<div class="msep"><i></i></div>'; prevSep = true; return; }
      prevSep = false;
      h += '<div class="mi ' + (r.danger ? 'danger' : '') + '">' + ic(r.ic) + '<span class="t">' + esc(r.n) + '</span><span class="k">' + (r.arrow ? '\u25B8' : (r.sc || '')) + '</span></div>';
    });
    return h + '</div>';
  }
  function ctxTools() {
    return '<div class="cm-tools">' + btn('Add custom command', 'pri', 'plus') + btn('New submenu', '', 'plus') + btn('Add separator', '', 'plus') + btn('Import from Nilesoft Shell\u2026', '', 'download') +
      '<span style="margin-left:auto">' + btn('Reset menu', 'dng', 'rollback') + '</span></div>';
  }
  function context() {
    var c = ph('Context menu', 'The menu that opens when you right-click a file or folder in Tsubame. Drawn by Tsubame, so you can edit it. Windows shell extensions stay available under \u201CShow more options\u201D.');
    c += ctxTools();
    c += '<div class="cm-wrap"><div class="cm-list">' + grp(CM.map(cmRow).join('')) +
      '<div class="note" style="margin-top:8px">' + ic('info', 14) + '<div>Drag the grip to reorder (the accent line shows where it lands), click the eye to hide, double-click a name to rename, drag onto a submenu to move inside it. Built-in items can be hidden and renamed but not deleted.</div></div></div>' +
      '<div class="cm-prev"><div class="sg-h" style="margin-top:0">Preview</div>' + cmPreview() + '<div class="note" style="margin-top:8px">' + ic('info', 14) + '<div>Hidden items and the contents of submenus are not shown here. Matches what right-click shows now.</div></div></div></div>';
    return win('Context menu', c, 860, [2, 440]);
  }
  function contextAdd() {
    var c = ph('Context menu', 'Add your own commands, or bring them over from a Nilesoft Shell config.');
    c += ctxTools();
    c += sh('New custom command') + grp(
      '<div class="cm-form" style="padding:12px 16px">' +
        '<span class="l">Name</span>' + field('Open in VS Code', 'focus grow', { caret: true }) +
        '<span class="l">Command</span><span style="display:flex;gap:8px">' + field('C:\\Users\\Alex\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe', 'grow', { icon: 'term' }) + btn('Browse\u2026') + '</span>' +
        '<span class="l">Arguments</span>' + field('--reuse-window "{path}"', 'grow') +
        '<span class="hintline"><code>{path}</code> the clicked item, or the open folder when you right-click the background<br><code>{selection}</code> every selected item, each in quotes, separated by spaces</span>' +
        '<span class="l">Show for</span><span class="chips"><label>' + chk(true) + 'Files</label><label>' + chk(true) + 'Folders</label><label>' + chk(false) + 'Folder background</label></span>' +
        '<span class="l">Place in</span>' + dd('Tools (submenu)', false, null, 0, 188) +
        '<span class="l"></span><span style="display:flex;gap:8px">' + btn('Add to menu', 'pri') + btn('Cancel') + '<span style="margin-left:auto;font-size:var(--font-size-xs);color:var(--c-text-dim);align-self:center">Runs as you, never elevated. Shows up in the command palette too.</span></span>' +
      '</div>');
    c += sh('Import from Nilesoft Shell') + '<div class="imp"><div class="hd">' + ic('check', 18) + '<div class="t2"><b style="font-weight:600">shell.nss</b>: 14 imported, 2 skipped<span class="s">C:\\Program Files\\Nilesoft Shell\\shell.nss \u00B7 imported just now into the \u201CTools\u201D submenu, tagged \u201CFrom Nilesoft\u201D</span></div>' + btn('Undo import', '', 'undo') + btn('Show imported') + '</div>' +
      '<div class="sk">' + ic('warn', 14) + '<div><b>Line 41: </b><code>item(title=\'Take ownership\' admin cmd=\u2026)</code> \u2014 needs administrator rights. Tsubame never runs menu items elevated.</div></div>' +
      '<div class="sk">' + ic('warn', 14) + '<div><b>Line 77: </b><code>item(title=if(reg.exists(\u2026), \u2026))</code> \u2014 uses a dynamic expression (<code>if</code>, <code>reg</code>). Tsubame entries are fixed text; add it as a custom command and use \u201CShow for\u201D instead.</div></div>' +
      '</div>';
    c += '<div style="margin-top:10px">' + note('info', 'Read from <code>.nss</code>: <code>item()</code> and <code>menu()</code> titles, <code>image</code> (png, ico, exe), <code>cmd</code> and <code>args</code> with Nilesoft variables mapped to <code>{path}</code> / <code>{selection}</code>, <code>type</code> and <code>find</code> filters, separators. Themes and anything dynamic are skipped and listed above. Your original file is never changed.') + '</div>';
    return win('Context menu', c, 780, [2, 400]);
  }

  /* ================================================================ Keys */
  var K2 = { 'nav.up': 'Backspace', 'nav.refresh': 'Ctrl+R', 'tab.next': 'Ctrl+PgDn', 'tab.prev': 'Ctrl+PgUp', 'file.open': 'Double-click' };
  var MODIFIED = { 'file.newfile': 'Ctrl+N', 'edit.invert': 'Ctrl+Shift+I', 'view.hidden': 'Ctrl+.' };
  function keyRow(c, o) {
    o = o || {};
    var k = MODIFIED[c.id] || c.k, mod = !!MODIFIED[c.id];
    var sl = keys(k) + (K2[c.id] ? '<span style="color:var(--c-text-disabled);font-size:var(--font-size-xs);margin:0 4px">or</span>' + keys(K2[c.id]) : '');
    var acts = o.hover ? '<span class="ibt hot" title="Record">' + ic('rename', 14) + '</span><span class="ibt">' + ic('x', 12) + '</span><span class="ibt">' + ic('rollback', 14) + '</span>' : (mod ? '<span class="ibt">' + ic('rollback', 14) + '</span>' : '');
    return '<div class="kr' + (mod ? ' mod' : '') + (o.hover ? ' hover' : '') + (o.open ? ' open' : '') + '">' + ic(c.ic) + '<span class="nm t">' + esc(c.n) + (c.custom ? ' <span class="tag acc" style="margin-left:6px">Custom</span>' : '') + '</span><span class="sl">' + sl + '</span><span class="acts">' + acts + '</span></div>';
  }
  function keysHead(profileCur) {
    return ph('Keys', 'Every command has a name and at most two shortcuts. Click a row, then press the keys you want. The command palette (Ctrl+Shift+P) shows the same shortcuts.') +
      '<div class="ktool">' + field('', 'grow', { w: 300, icon: 'search', ph: 'Search commands or shortcuts' }) +
      '<span style="display:flex;align-items:center;gap:8px"><span style="color:var(--c-text-dim);font-size:var(--font-size-xs)">Profile</span>' + seg(['Tsubame default', 'Explorer-like', 'Total Commander-like'], profileCur || 0) + '</span>' +
      '<span class="push">' + btn('Reset all', 'dng', 'rollback') + '</span></div>';
  }
  function catHead(cat, n) { return '<div class="kh">' + ic('chevd', 12) + cat + ' <span class="n">' + n + '</span></div>'; }
  function keysPage() {
    var by = {}; U.cmds.forEach(function (c) { (by[c.cat] = by[c.cat] || []).push(c); });
    var c = keysHead(0), g = '';
    ['File', 'Edit', 'Navigate'].forEach(function (cat) {
      g += catHead(cat, by[cat].length);
      by[cat].forEach(function (cm) { g += keyRow(cm, { hover: cm.id === 'file.rename' }); });
    });
    c += grp(g);
    c += '<div style="margin-top:8px;font-size:var(--font-size-xs);color:var(--c-text-dim)">3 shortcuts changed from the profile (accent bar). Tabs, Panes, View, Search, Tools and App follow below.</div>';
    return win('Keys', c, 900, [2, 250]);
  }
  function keysRecord() {
    var by = {}; U.cmds.forEach(function (c) { (by[c.cat] = by[c.cat] || []).push(c); });
    var c = keysHead(0), g = '';
    g += catHead('Tabs', by.Tabs.length);
    by.Tabs.forEach(function (cm) {
      if (cm.id === 'tab.dup') {
        g += keyRow({ id: 'tab.dup', n: 'Duplicate tab', ic: 'copy', k: null }, { open: true }).replace('<span class="sl">' + keys(null) + '</span>', '<span class="sl">' + '<span class="keys">' + kc('Ctrl', 'rec') + kc('Shift', 'rec') + kc('T', 'rec') + '</span>' + '</span>');
        g += '<div class="krec"><div class="field"><span class="keys">' + kc('Ctrl', 'rec') + kc('Shift', 'rec') + kc('T', 'rec') + '</span><span class="caret"></span><span class="ph">Recording: press the shortcut you want</span></div>' +
          '<div class="conf">' + ic('warn', 16) + '<div><b>Ctrl+Shift+T</b> is already used by <b>Reopen closed tab</b> (Tabs). If you assign it here it is removed from there, so <b>Reopen closed tab</b> will have no shortcut.</div></div>' +
          '<div class="btns">' + btn('Assign here, unbind there', 'pri') + btn('Pick another') + btn('Cancel') + '<span class="hint">' + kc('Esc') + ' cancel ' + kc('Backspace') + ' clear this shortcut ' + kc('Enter') + ' accept</span></div></div>';
      } else g += keyRow(cm, {});
    });
    g += catHead('Panes', by.Panes.length);
    by.Panes.forEach(function (cm) { g += keyRow(cm, {}); });
    c += grp(g);
    return win('Keys', c, 820, [150, 250]);
  }

  /* ================================================================ Updates */
  function updates(available) {
    var logo = T.logo().replace('<svg class="logo"', '<svg style="width:44px;height:44px;flex:none"');
    var c = ph('Updates', 'Tsubame checks for a newer version and tells you. It never installs anything without you clicking, unless you turn automatic updates on.');
    if (available) {
      c += '<div class="banner" style="flex-wrap:wrap">' + ic('download', 16) + '<div class="t2"><b style="font-weight:600">Version 0.5.0 is available</b><br><span>Released 2026-11-17 \u00B7 14.2 MB \u00B7 signature verified before install</span></div>' +
        btn('Release notes') + btn('Download and restart', 'pri') + '<div style="flex-basis:100%;display:flex;align-items:center;gap:10px;padding-left:28px"><span class="pbar"><i style="width:62%"></i></span><span style="font-size:var(--font-size-xs);color:var(--c-text-dim)">Downloading 8.8 of 14.2 MB</span>' + btn('Cancel', 'sm') + '</div></div>';
    }
    c += sh('This copy') + grp(
      '<div class="srow"><div class="verbig" style="flex:1">' + logo + '<div><div class="vv">Tsubame 0.4.2</div><div class="vs">64-bit \u00B7 installed for this user \u00B7 built 2026-11-10</div>' +
        '<div class="okline" style="margin-top:8px">' + (available ? ic('info', 14) + 'Update available: 0.5.0' : ic('check', 14) + 'You\u2019re up to date') + '<span style="color:var(--c-text-dim)">\u00B7 last checked today at 09:41</span></div></div></div>' +
        '<div class="ctl2">' + btn('Check now', available ? '' : 'pri', 'refresh') + '</div></div>' +
      row('Update channel', 'Stable: a release about every two weeks. Beta: fixes first, may break. Switching to Stable from Beta waits for the next stable release.', dd('Stable', false, null, 0, 168))
    );
    c += sh('How updates happen') + grp(
      row('Check for updates when Tsubame starts', 'At most once a day. One small request to the release server, no personal data.', tog(true)) +
      row('Install updates automatically', 'Download in the background and install when you next restart Tsubame. Off: you click \u201CDownload and restart\u201D yourself.', tog(false)) +
      row('Roll back to 0.4.1', 'The previous version is kept for 14 days after an update. If Tsubame crashes right after updating it offers this itself.', btn('Roll back\u2026', '', 'rollback'))
    );
    c += sh('Integrity') + grp(
      '<div class="srow col"><div class="okline">' + ic('shield', 16) + '<b style="font-weight:600">Verified signature</b><span style="color:var(--c-text-dim)">This build and every update are checked before use.</span></div>' +
      '<div class="facts"><span class="k">Signed by</span><span>Tsubame release key 3F2A\u2026 9C1D (Ed25519)</span><span class="k">This build</span><span class="mono">sha256 7be1 40c9 \u2026 a6d2 \u2014 matches the signed manifest</span><span class="k">Updates come from</span><span>github.com/tsubame/tsubame/releases (https only)</span></div></div>'
    );
    return win('Updates', c, 760, [2, 380], { dot: available ? 'Updates' : null });
  }

  /* ================================================================ Default file manager */
  function defaultMgr() {
    var c = ph('Default file manager', 'Optional. Off by default. Nothing here changes Windows until you switch it on, and one click puts everything back.');
    c += '<div class="grp pad" style="display:flex;flex-direction:column;gap:10px">' +
      '<div style="display:flex;align-items:center;gap:12px"><div class="stat" style="flex:1"><i class="led"></i><span>Tsubame handles folder opens: <b>Off</b></span></div>' + btn('Refresh', '', 'refresh') + '</div>' +
      '<div class="facts"><span class="k">Windows currently opens folders with</span><span>File Explorer</span><span class="k">Checked in the registry</span><span class="mono">HKCU\\Software\\Classes\\Directory\\shell</span></div></div>';
    c += sh('Use Tsubame instead of File Explorer for folders') + grp(
      '<div class="srow col"><div class="nm" style="color:var(--c-text-dim);font-size:var(--font-size-sm);line-height:1.5">Windows has no supported setting for this. Tsubame makes a <b style="color:var(--c-text);font-weight:600">per-user registry change</b> so that folders open in Tsubame. It only affects your Windows account, needs no administrator rights, and the button below undoes it.</div></div>' +
      row('Open folders in Tsubame', 'Double-clicking a folder, the Run dialog and \u201Copen folder\u201D in other apps will open Tsubame.', tog(false)) +
      row('Also take over Win+E', 'Experimental on Windows 11 24H2 and later. If Windows ignores it, use the setting above.', tog(false), 'dis') +
      row('Add \u201COpen in Tsubame\u201D to Explorer\u2019s folder menu', 'Harmless and independent of the above. Lets you try Tsubame without switching.', tog(true))
    );
    c += '<div style="margin-top:10px">' + note('info', '<b>What still opens File Explorer:</b> drives, This PC, the Recycle Bin, Control Panel, the taskbar Explorer icon and every app\u2019s Open/Save dialog. Windows does not allow replacing those.') + '</div>';
    c += sh('Undo and safety') + grp(
      row('Restore Windows Explorer', 'Removes every change this page made, then re-reads the registry to check. Always safe to press, even if nothing was changed.', btn('Restore Windows Explorer', 'pri', 'rollback')) +
      '<div class="srow col"><div class="note" style="border:0;padding:0">' + ic('shield', 16) + '<div><b>Fail-safe.</b> If Tsubame cannot start (deleted, quarantined, broken), a tiny helper opens File Explorer instead and switches this setting off by itself, so you never end up with folders that will not open. If you are ever stuck: press <span class="kcap" style="display:inline-flex">Win</span>+<span class="kcap" style="display:inline-flex">R</span>, type <span class="mono">explorer.exe</span>, or run <span class="mono">restore-explorer.reg</span> from the install folder. Uninstalling restores Explorer first. If another app changes this later, Tsubame says so here and never re-applies it.</div></div></div>'
    );
    return win('Default file manager', c, 940, [2, 400]);
  }

  /* ================================================================ Advanced */
  function advanced() {
    var c = ph('Advanced', 'Limits and diagnostics. The defaults are fine for nearly everyone.');
    c += banner('<b style="font-weight:600">Restart needed.</b> <span>GPU rendering is applied when Tsubame starts again. Your tabs and panes are restored.</span>', btn('Restart now', 'pri') + btn('Later'));
    c += sh('Rendering') + grp(row('Use the GPU for drawing', 'Direct2D on the graphics card. Turn off only if you see glitches; the software renderer uses more CPU.', tog(false)));
    c += sh('Caches') + grp(
      row('Thumbnails', '212 MB of 512 MB used. Oldest are dropped first.', '<span class="cap-bar"><span class="pbar"><i style="width:41%"></i></span>' + dd('512 MB', false, null, 0, 96) + btn('Clear') + '</span>') +
      row('Icons', '624 of 1,024 entries \u00B7 2.5 MB. Rebuilt on demand.', '<span class="cap-bar"><span class="pbar"><i style="width:61%"></i></span>' + btn('Clear') + '</span>') +
      row('Folder sizes', '18 MB. Computed in the background, refreshed when a folder changes.', '<span class="cap-bar"><span class="pbar"><i style="width:9%"></i></span>' + btn('Clear') + '</span>')
    );
    c += sh('Memory (live)') + grp('<div class="srow"><div class="facts" style="flex:1;grid-template-columns:auto 1fr auto 1fr auto 1fr"><span class="k">Private bytes</span><span>96 MB</span><span class="k">Open tabs</span><span>3 (118,204 entries, 4.2 MB)</span><span class="k">Handles</span><span>107</span></div>' + btn('Copy diagnostics', '', 'copy') + '</div>');
    c += sh('Logging') + grp(
      row('Log level', 'Log file: <span class="mono">%LOCALAPPDATA%\\Tsubame\\logs\\tsubame.log</span>, at most 5 MB \u00D7 3 files. Never contains file names unless Verbose.', dd('Errors only', false, null, 0, 140) + btn('Open log folder', '', 'folder'))
    );
    c += sh('Reset') + grp(row('Reset all settings', 'Restores every setting, shortcut, context-menu edit and theme choice. Your files, tabs and session are not touched. The current <span class="mono">settings.toml</span> is kept as <span class="mono">settings.toml.bak</span>.', btn('Reset all\u2026', 'dng')));
    return win('Advanced', c, 900, [2, 330]);
  }

  /* ================================================================ Small states */
  function line(lb, html) { return '<div class="line"><span class="lb">' + lb + '</span>' + html + '</div>'; }
  function card(title, body, wide) { return '<div class="card' + (wide ? ' wide' : '') + '"><h3>' + title + '</h3>' + body + '</div>'; }
  function states() {
    var s = '<div class="sheet">';
    s += card('Toggle', line('off', tog(false)) + line('on', tog(true)) + line('keyboard focus', tog(true, 'focus') + '<span style="color:var(--c-text-dim);font-size:var(--font-size-xs)">1 px <code>focus</code> ring, 2 px gap</span>') + line('disabled', tog(false, 'off') + '<span style="color:var(--c-text-disabled);font-size:var(--font-size-xs)">greyed with its label</span>'));
    s += card('Segmented control', line('normal', seg(['Compact', 'Normal', 'Comfortable'], 1)) + line('hover on another', seg(['Compact', 'Normal', 'Comfortable'], 1, 2)) + line('two options', seg(['Move to Recycle Bin', 'Delete permanently'], 0)));
    s += card('Dropdown', line('closed', dd('Stable', false, null, 0, 168)) + '<div class="line" style="min-height:112px;align-items:flex-start"><span class="lb">open (popup, 1 px border, current item checked)</span>' + dd('Stable', true, ['Stable', 'Beta', 'Nightly (unsigned, off)'], 0, 168) + '</div>');
    s += card('Slider', line('20% (12 px)', slider(20) + '<span class="sval">12 px</span>') + line('60%, keyboard focus', slider(60, 'focus') + '<span class="sval">14 px</span>') + line('hint', '<span style="color:var(--c-text-dim);font-size:var(--font-size-xs)">Arrows move by 1, PgUp/PgDn by 5. Value changes the window live.</span>'));
    s += card('Text field', line('empty', field('', 'fixed', { w: 260, ph: 'Folder path' })) + line('focused, caret on', field('C:\\Users\\Alex', 'focus fixed', { w: 260, caret: true })) +
      line('focused, caret off', field('C:\\Users\\Alex', 'focus fixed', { w: 260 }) ) +
      '<div class="line" style="align-items:flex-start"><span class="lb">error</span><div style="width:260px">' + field('C:\\Nope\\Missing', 'err', { w: 260 }) + '<div class="ferr">' + ic('warn', 12) + 'This folder does not exist.</div></div></div>' + line('disabled', field('Managed by your admin', 'dis', { w: 260 })));
    s += card('Caret blink rule', '<div style="font-size:var(--font-size-xs);color:var(--c-text-dim);line-height:1.5">The caret blinks (530 ms on, 530 ms off) <b style="color:var(--c-text);font-weight:600">only while a text field has focus</b>. After about 10 s without typing the timer stops and the caret stays solid, so an idle window wakes the CPU zero times. Typing or clicking restarts the 10 s.</div>' +
      line('0 s, on phase', field('inv', 'focus', { w: 150, caret: true })) + line('0 s, off phase', field('inv', 'focus', { w: 150 })) + line('after about 10 s', field('inv', 'focus', { w: 150, caret: true }) + '<span style="color:var(--c-text-dim);font-size:var(--font-size-xs)">solid, timer stopped</span>'));
    s += card('Buttons', line('default', btn('Check now')) + line('primary', btn('Download and restart', 'pri')) + line('hover', btn('Browse\u2026', 'hot')) + line('danger (outline)', btn('Reset all\u2026', 'dng')) + line('disabled', btn('Roll back\u2026', 'off')) + line('keyboard focus', btn('Cancel', 'focus')));
    s += card('Destructive confirm: Reset all', line('stage 1', btn('Reset all\u2026', 'dng')) +
      '<div class="line" style="align-items:stretch"><span class="lb" style="padding-top:8px">stage 2 (click replaces the button in place)</span><div class="note warn" style="flex:1;flex-direction:column;gap:8px"><div style="display:flex;gap:8px">' + ic('warn', 14) + '<div><b>Reset every setting?</b> All settings, shortcuts, context-menu edits and the theme go back to the defaults. Files and session are untouched; <span class="mono">settings.toml.bak</span> keeps your old settings.</div></div><div style="display:flex;gap:8px;justify-content:flex-end">' + btn('Cancel', 'focus') + btn('Yes, reset everything', 'dngfill') + '</div></div></div>');
    s += card('Banner: restart needed (settings apply live, so there is no \u201CSaved\u201D toast)',
      banner('<b style="font-weight:600">Restart needed.</b> <span>GPU rendering is applied when Tsubame starts again. Your tabs and panes are restored.</span>', btn('Restart now', 'pri') + btn('Later')).replace('margin-bottom', 'margin-bottom') +
      '<div class="note">' + ic('info', 14) + '<div>Another app changed the default folder handler. Tsubame will not change it back. <b>Refresh</b> to check.</div></div>' +
      '<div style="font-size:var(--font-size-xs);color:var(--c-text-dim);line-height:1.5">The banner sits at the top of the page that needs it and in the nav as a dot. It disappears after the restart or when the change is reverted.</div>', true);
    return '<div class="win" style="width:1280px;height:1240px;background:var(--c-row)">' + sheetHeader() + s + '</div></div>';
  }
  function sheetHeader() {
    return '<div style="padding:18px 32px 0;background:var(--c-row)"><h1 class="pg-h" style="margin:0 0 4px">Control states</h1><p class="pg-s" style="margin:0">Every control the settings pages use, in each state. Same flat fills and 1 px strokes as the rest of the kit.</p></div><div style="display:contents">';
  }

  function page(name) {
    switch (name) {
      case 'general': return general();
      case 'appearance': return appearance();
      case 'context': return context();
      case 'context-add': return contextAdd();
      case 'keys': return keysPage();
      case 'keys-record': return keysRecord();
      case 'updates': return updates(false);
      case 'updates-available': return updates(true);
      case 'default': return defaultMgr();
      case 'advanced': return advanced();
      case 'states': return states();
    }
    return '';
  }
  window.Tsu4S = { page: page, themes: THEMES };
})();
