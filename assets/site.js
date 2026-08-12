/* =====================================================================
   ReviewEngine · landing scripts
   Zero dependencies. Every enhancement degrades to static, readable HTML:
   no JS  → full terminal transcript + visible content (no hiding);
   reduced motion → static transcript, no reveal animation, seal visible.
   ===================================================================== */
(function () {
  'use strict';

  var docEl = document.documentElement;
  var REDUCED = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var HAS_IO = 'IntersectionObserver' in window;

  /* ---------- header: scrolled state ---------- */
  var header = document.querySelector('.site-header');
  var ticking = false;
  function updateHeader() {
    if (header) header.classList.toggle('is-scrolled', window.scrollY > 8);
    ticking = false;
  }
  window.addEventListener('scroll', function () {
    if (!ticking) {
      window.requestAnimationFrame(updateHeader);
      ticking = true;
    }
  }, { passive: true });
  updateHeader();

  /* ---------- mobile nav toggle ---------- */
  var navToggle = document.querySelector('.nav-toggle');
  var nav = document.querySelector('.site-nav');
  if (navToggle && nav) {
    var firstLink = nav.querySelector('a');
    function setOpen(open, moveFocus) {
      nav.classList.toggle('is-open', open);
      navToggle.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (moveFocus) {
        if (open && firstLink) firstLink.focus();
        else navToggle.focus();
      }
    }
    navToggle.addEventListener('click', function () {
      setOpen(!nav.classList.contains('is-open'), true);
    });
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && nav.classList.contains('is-open')) setOpen(false, true);
    });
    document.addEventListener('click', function (e) {
      if (nav.classList.contains('is-open') && !nav.contains(e.target) && !navToggle.contains(e.target)) {
        setOpen(false, false);
      }
    });
    nav.addEventListener('click', function (e) {
      if (e.target.closest('a')) setOpen(false, false);
    });
    window.addEventListener('resize', function () {
      if (window.innerWidth > 900 && nav.classList.contains('is-open')) setOpen(false, false);
    });
  }

  /* ---------- reveal on scroll ---------- */
  function revealAll() {
    var els = document.querySelectorAll('[data-reveal]');
    for (var i = 0; i < els.length; i++) els[i].classList.add('is-visible');
  }
  if (REDUCED || !HAS_IO) {
    revealAll();
  } else {
    var io = new IntersectionObserver(function (entries) {
      for (var i = 0; i < entries.length; i++) {
        if (entries[i].isIntersecting) {
          entries[i].target.classList.add('is-visible');
          io.unobserve(entries[i].target);
        }
      }
    }, { threshold: 0.12, rootMargin: '0px 0px -8% 0px' });
    var revealEls = document.querySelectorAll('[data-reveal]');
    for (var j = 0; j < revealEls.length; j++) {
      var d = revealEls[j].getAttribute('data-d');
      if (d) revealEls[j].style.transitionDelay = d + 's';
      io.observe(revealEls[j]);
    }
  }

  /* ---------- seals: stamp once, on trigger ---------- */
  function stampSeal(seal) {
    if (!seal || seal.classList.contains('is-stamped')) return;
    seal.classList.add('is-stamped');
  }
  // hero seal — stamped when the terminal finishes typing (or immediately)
  var heroSeal = document.querySelector('[data-seal="hero"]');
  // report seal — stamped when scrolled into view
  var reportSeal = document.querySelector('[data-seal="report"]');
  if (reportSeal) {
    if (REDUCED || !HAS_IO) {
      stampSeal(reportSeal);
    } else {
      var sealIO = new IntersectionObserver(function (entries) {
        for (var i = 0; i < entries.length; i++) {
          if (entries[i].isIntersecting) {
            stampSeal(entries[i].target);
            sealIO.unobserve(entries[i].target);
          }
        }
      }, { threshold: 0.35 });
      sealIO.observe(reportSeal);
    }
  }

  /* ---------- terminal typing ---------- */
  var terminal = document.querySelector('[data-terminal]');
  if (terminal) {
    if (REDUCED || !HAS_IO) {
      stampSeal(heroSeal); // static transcript is already in the DOM
    } else {
      var terminalScreen = terminal.querySelector('.terminal-screen');
      var tLines = Array.prototype.map.call(
        terminalScreen.querySelectorAll('.ln'),
        function (el) { return { cls: el.className, text: el.textContent }; }
      );
      terminalScreen.textContent = '';
      var tIdx = 0;
      var cursor = null;
      var finished = false;

      function scrollBottom() {
        terminalScreen.scrollTop = terminalScreen.scrollHeight;
      }
      function finalizeLine(ln, cls, text) {
        // re-inject the styled prompt / ok glyph after a textContent rebuild
        if (cls.indexOf('ln-cmd') !== -1 && text.slice(0, 2) === '$ ') {
          ln.innerHTML = '<span class="prompt">$ </span>' + text.slice(2);
        } else if (cls.indexOf('ln-ok') !== -1 && text.charAt(0) === '\u2713') {
          ln.innerHTML = '<span class="ok">\u2713</span>' + text.slice(1);
        } else {
          ln.textContent = text;
        }
      }
      function appendLine(cls, text) {
        var ln = document.createElement('span');
        ln.className = cls;
        finalizeLine(ln, cls, text);
        terminalScreen.appendChild(ln);
        scrollBottom();
      }
      function startTypingCmd() {
        var line = tLines[tIdx];
        var ln = document.createElement('span');
        ln.className = line.cls;
        terminalScreen.appendChild(ln);
        var chars = line.text;
        var k = 0;
        var timer = setInterval(function () {
          k += 1;
          ln.textContent = chars.slice(0, k);
          scrollBottom();
          if (k >= chars.length) {
            clearInterval(timer);
            finalizeLine(ln, line.cls, chars);
            tIdx += 1;
            setTimeout(nextLine, 260);
          }
        }, 18);
      }
      function nextLine() {
        if (finished) return;
        if (tIdx >= tLines.length) {
          finished = true;
          cursor = document.createElement('span');
          cursor.className = 'cursor';
          cursor.setAttribute('aria-hidden', 'true');
          terminalScreen.appendChild(cursor);
          scrollBottom();
          stampSeal(heroSeal);
          return;
        }
        var line = tLines[tIdx];
        if (line.cls.indexOf('ln-cmd') !== -1) {
          startTypingCmd();
          return;
        }
        appendLine(line.cls, line.text);
        tIdx += 1;
        var gap = line.cls.indexOf('ln-sep') !== -1 ? 140 : 190;
        setTimeout(nextLine, gap);
      }
      var termIO = new IntersectionObserver(function (entries) {
        for (var i = 0; i < entries.length; i++) {
          if (entries[i].isIntersecting) {
            termIO.unobserve(entries[i].target);
            // small beat before the first keystroke
            setTimeout(nextLine, 350);
            break;
          }
        }
      }, { threshold: 0.4 });
      termIO.observe(terminal);
    }
  }

  /* ---------- copy buttons on code blocks ---------- */
  var blocks = document.querySelectorAll('.code');
  var I18N_T = window.LANDING_I18N_T || null;
  function copyLabel() { return I18N_T ? I18N_T('common.copy') : '复制'; }
  function copiedLabel() { return I18N_T ? I18N_T('common.copied') : '已复制'; }
  function copyFailLabel() { return I18N_T ? I18N_T('common.copyFail') : '失败'; }
  function copyAria() { return I18N_T ? I18N_T('common.copyAria') : '复制命令'; }
  function legacyCopy(text) {
    return new Promise(function (resolve, reject) {
      var ta = document.createElement('textarea');
      ta.value = text;
      ta.setAttribute('readonly', '');
      ta.style.position = 'absolute';
      ta.style.left = '-9999px';
      document.body.appendChild(ta);
      ta.select();
      try { var ok = document.execCommand('copy'); ok ? resolve() : reject(new Error('execCommand copy failed')); }
      catch (e) { reject(e); }
      document.body.removeChild(ta);
    });
  }
  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      // async API may reject (no user activation / permission denied) — fall back
      return navigator.clipboard.writeText(text).catch(function () { return legacyCopy(text); });
    }
    return legacyCopy(text);
  }
  var copyButtons = [];
  for (var b = 0; b < blocks.length; b++) {
    var block = blocks[b];
    var label = block.querySelector('.code-label');
    var pre = block.querySelector('pre');
    if (!label || !pre) continue;
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'code-copy';
    btn.textContent = copyLabel();
    btn.setAttribute('aria-label', copyAria());
    btn.addEventListener('click', function (e) {
      var self = e.currentTarget;
      copyText(pre.textContent).then(function () {
        self.textContent = copiedLabel();
        self.classList.add('is-copied');
        setTimeout(function () {
          self.textContent = copyLabel();
          self.classList.remove('is-copied');
        }, 1400);
      }).catch(function () {
        self.textContent = copyFailLabel();
        setTimeout(function () { self.textContent = copyLabel(); }, 1400);
      });
    });
    label.appendChild(btn);
    copyButtons.push(btn);
  }

  /* ---------- overflow affordance: fade right edge when scrollable ----------
     adds .is-overflow to .code pre / .table-scroll only while their content
     actually overflows, so the fade never appears on fully-visible blocks */
  var scrollables = document.querySelectorAll('.code pre, .table-scroll');
  function syncOverflow() {
    for (var s = 0; s < scrollables.length; s++) {
      scrollables[s].classList.toggle('is-overflow', scrollables[s].scrollWidth > scrollables[s].clientWidth + 1);
    }
  }
  if (scrollables.length) {
    syncOverflow();
    window.addEventListener('resize', syncOverflow);
    if (document.fonts && document.fonts.ready) {
      document.fonts.ready.then(syncOverflow); // widths change once webfonts land
    }
  }

  /* ---------- react to language switches ---------- */
  document.addEventListener('landing:i18n', function () {
    for (var c = 0; c < copyButtons.length; c++) {
      if (!copyButtons[c].classList.contains('is-copied')) copyButtons[c].textContent = copyLabel();
      copyButtons[c].setAttribute('aria-label', copyAria());
    }
    syncOverflow();
  });
})();
