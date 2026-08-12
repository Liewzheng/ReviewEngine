/* =====================================================================
   ReviewEngine · landing docs portal renderer
   Zero dependencies besides the vendored marked (assets/vendor/marked.min.js).
   Renders docs/ snapshots from docs-md/ (same-site) or, on a github.io
   host, tries raw.githubusercontent.com first (3s timeout) then falls
   back to the same-site snapshot. Docs are served verbatim — no
   translation, no data-i18n keys. Link rewriting maps relative .md
   links to ?doc= deep links and ../README|CHANGELOG|CONTRIBUTING to
   the GitHub repo (new tab).
   ===================================================================== */
(function () {
  'use strict';

  var IS_GITHUB_IO = /github\.io$/i.test(location.hostname);
  var REMOTE_BASE = 'https://raw.githubusercontent.com/Liewzheng/ReviewEngine/main/docs/';
  var GITHUB_REPO = 'https://github.com/Liewzheng/ReviewEngine';
  var MANIFEST = window.DOCS_MANIFEST || [];
  var DOC_PATHS = {};
  for (var mi = 0; mi < MANIFEST.length; mi++) DOC_PATHS[MANIFEST[mi].path] = true;

  /* ---------- current doc from ?doc= ---------- */
  function currentDoc() {
    try {
      var p = new URLSearchParams(window.location.search).get('doc');
      if (p) return p;
    } catch (e) {}
    return 'README.md';
  }

  /* ---------- fetch ---------- */
  function fetchLocal(path) {
    return fetch('docs-md/' + path).then(function (r) {
      if (!r.ok) throw new Error('http ' + r.status);
      return r.text();
    });
  }
  function fetchRemote(path) {
    var ctrl = ('AbortController' in window) ? new AbortController() : null;
    var timer = ctrl ? setTimeout(function () { ctrl.abort(); }, 3000) : null;
    var opts = ctrl ? { signal: ctrl.signal } : {};
    return fetch(REMOTE_BASE + path, opts).then(function (r) {
      if (!r.ok) throw new Error('http ' + r.status);
      return r.text();
    }).finally(function () { if (timer) clearTimeout(timer); });
  }
  function loadDoc(path) {
    if (IS_GITHUB_IO) return fetchRemote(path).catch(function () { return fetchLocal(path); });
    return fetchLocal(path);
  }

  /* ---------- markdown ---------- */
  function slugify(text) {
    var s = text.toLowerCase().replace(/[^\w\u4e00-\u9fff-]+/g, '-').replace(/^-+|-+$/g, '');
    return s || 'section';
  }
  function stripFrontmatter(text) {
    var m = text.match(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/);
    return m ? text.slice(m[0].length) : text;
  }
  function setupMarked() {
    var renderer = new marked.Renderer();
    renderer.heading = function (text, level) {
      var plain = text.replace(/<[^>]+>/g, '');
      return '<h' + level + ' id="' + slugify(plain) + '">' + text + '</h' + level + '>';
    };
    marked.setOptions({ gfm: true });
    marked.use({ renderer: renderer });
  }

  /* ---------- link rewriting ---------- */
  function normalizeRel(link, docPath) {
    var baseDir = docPath.indexOf('/') !== -1 ? docPath.slice(0, docPath.lastIndexOf('/') + 1) : '';
    var segs = (baseDir + link).split('/');
    var out = [];
    for (var i = 0; i < segs.length; i++) {
      var s = segs[i];
      if (s === '' || s === '.') continue;
      if (s === '..') {
        if (out.length && out[out.length - 1] !== '..') out.pop();
        else out.push('..');
        continue;
      }
      out.push(s);
    }
    return out.join('/');
  }
  function stripLink(a) {
    var span = document.createElement('span');
    span.textContent = a.textContent;
    a.replaceWith(span);
  }
  function rewriteLinks(root, docPath) {
    var links = root.querySelectorAll('a[href]');
    for (var i = 0; i < links.length; i++) {
      var a = links[i];
      var href = a.getAttribute('href');
      if (!href) continue;
      if (/^(https?:|mailto:|#)/.test(href)) {
        if (/^https?:/.test(href)) { a.setAttribute('target', '_blank'); a.setAttribute('rel', 'noopener noreferrer'); }
        continue; // external / same-page anchor
      }
      if (href.indexOf('?') !== -1) continue; // already a deep link
      var m = href.match(/^(.*?)(#.*)?$/);
      var clean = m[1] || '';
      var hash = m[2] || '';
      var norm = normalizeRel(clean, docPath);
      if (norm.indexOf('..') === 0) {
        var base = norm.replace(/^\.\.\/+/, '');
        var url = null;
        if (base === 'README.md' || base === 'README') url = GITHUB_REPO;
        else if (base === 'CHANGELOG.md') url = GITHUB_REPO + '/blob/main/CHANGELOG.md';
        else if (base === 'CONTRIBUTING.md') url = GITHUB_REPO + '/blob/main/CONTRIBUTING.md';
        if (url) {
          a.setAttribute('href', url + hash);
          a.setAttribute('target', '_blank');
          a.setAttribute('rel', 'noopener noreferrer');
        } else {
          stripLink(a); // dead link outside docs (e.g. ../.notes/*)
        }
        continue;
      }
      if (DOC_PATHS[norm] || /\.md$/i.test(norm)) {
        a.setAttribute('href', '?doc=' + encodeURIComponent(norm) + hash);
      } else {
        stripLink(a);
      }
    }
  }

  /* ---------- render ---------- */
  var contentEl = null;
  function syncTableOverflow() {
    if (!contentEl) return;
    var sc = contentEl.querySelectorAll('.table-scroll');
    for (var i = 0; i < sc.length; i++) {
      sc[i].classList.toggle('is-overflow', sc[i].scrollWidth > sc[i].clientWidth + 1);
    }
  }
  function renderDoc(raw, path) {
    contentEl = document.getElementById('doc-content');
    contentEl.innerHTML = '';
    if (/\.toml$/i.test(path)) {
      var pre = document.createElement('pre');
      var code = document.createElement('code');
      code.textContent = raw;
      pre.appendChild(code);
      contentEl.appendChild(pre);
    } else {
      contentEl.innerHTML = marked.parse(stripFrontmatter(raw));
    }
    rewriteLinks(contentEl, path);
    var tables = contentEl.querySelectorAll('table');
    for (var t = 0; t < tables.length; t++) {
      var wrap = document.createElement('div');
      wrap.className = 'table-scroll';
      tables[t].parentNode.insertBefore(wrap, tables[t]);
      wrap.appendChild(tables[t]);
    }
    syncTableOverflow();
    document.getElementById('doc-loading').hidden = true;
    document.getElementById('doc-error').hidden = true;
    contentEl.hidden = false;
    if (window.location.hash) {
      var target = document.querySelector(window.location.hash);
      if (target) target.scrollIntoView();
    }
  }

  /* ---------- states ---------- */
  function showLoading() {
    document.getElementById('doc-loading').hidden = false;
    document.getElementById('doc-error').hidden = true;
    document.getElementById('doc-content').hidden = true;
  }
  function showError() {
    document.getElementById('doc-loading').hidden = true;
    document.getElementById('doc-error').hidden = false;
    document.getElementById('doc-content').hidden = true;
  }

  /* ---------- document tree (from manifest) ---------- */
  function buildTree(current) {
    var groups = {};
    for (var i = 0; i < MANIFEST.length; i++) {
      var m = MANIFEST[i];
      (groups[m.group] = groups[m.group] || []).push(m);
    }
    var container = document.getElementById('doc-tree-groups');
    container.innerHTML = '';
    Object.keys(groups).forEach(function (g) {
      var h = document.createElement('p');
      h.className = 'doc-group';
      h.textContent = g;
      container.appendChild(h);
      groups[g].forEach(function (m) {
        var a = document.createElement('a');
        a.href = '?doc=' + encodeURIComponent(m.path);
        a.textContent = m.title;
        if (m.badge === 'design') {
          var b = document.createElement('span');
          b.className = 'doc-badge';
          b.textContent = '设计稿';
          a.appendChild(b);
        }
        if (m.path === current) a.setAttribute('aria-current', 'true');
        container.appendChild(a);
      });
    });
  }

  /* ---------- mobile tree drawer ---------- */
  function initTreeToggle() {
    var tree = document.getElementById('doc-tree');
    var btn = document.getElementById('doc-tree-toggle');
    if (!tree || !btn) return;
    function setOpen(open, moveFocus) {
      tree.classList.toggle('is-open', open);
      btn.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (open && moveFocus) {
        var first = tree.querySelector('a');
        if (first) first.focus();
      }
    }
    btn.addEventListener('click', function () {
      setOpen(!tree.classList.contains('is-open'), true);
    });
    document.addEventListener('click', function (e) {
      if (tree.classList.contains('is-open') && !tree.contains(e.target)) setOpen(false, false);
    });
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && tree.classList.contains('is-open')) { setOpen(false, true); }
    });
  }

  /* ---------- boot ---------- */
  function boot() {
    if (typeof marked === 'undefined' || !marked.parse) { showError(); return; }
    setupMarked();
    var path = currentDoc();
    buildTree(path);
    initTreeToggle();
    var retry = document.getElementById('doc-retry');
    if (retry) {
      retry.addEventListener('click', function () {
        showLoading();
        loadDoc(path).then(function (raw) { renderDoc(raw, path); }).catch(showError);
      });
    }
    window.addEventListener('resize', syncTableOverflow);
    showLoading();
    loadDoc(path).then(function (raw) { renderDoc(raw, path); }).catch(showError);
  }

  boot(); // defer scripts run after DOM parse
})();
