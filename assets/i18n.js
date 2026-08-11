/* =====================================================================
   ReviewEngine · landing i18n loader
   Zero dependencies. Reads dictionaries from window.LANDING_I18N
   (loaded statically so it also works over file:// — no fetch/CORS).
   Fallback chain per key:  request-lang dict → zh-CN → key string.
   Language resolution:    ?lang=  >  localStorage  >  navigator.language
                           (zh-TW/zh-HK/zh-Hant→zh-TW; en/fr/ja/ko/ru→own;
                            everything else → zh-CN)
   No JS / reduced-motion: the page stays as authored (zh-CN static
   full text); the switcher simply never appears.
   ===================================================================== */
(function () {
  'use strict';

  var STORAGE_KEY = 'reng-lang';
  var LANGS = ['zh-CN', 'zh-TW', 'en', 'fr', 'ja', 'ko', 'ru'];
  var NATIVE_NAMES = {
    'zh-CN': '简体中文',
    'zh-TW': '繁體中文',
    'en': 'English',
    'fr': 'Français',
    'ja': '日本語',
    'ko': '한국어',
    'ru': 'Русский'
  };
  // extra Google Fonts subsets injected only when that language is active
  var FONT_SETS = {
    'zh-TW': 'family=Noto+Sans+TC:wght@400;500;700&family=Noto+Serif+TC:wght@500;600;700',
    'ja': 'family=Noto+Sans+JP:wght@400;500;700&family=Noto+Serif+JP:wght@500;600;700',
    'ko': 'family=Noto+Sans+KR:wght@400;500;700&family=Noto+Serif+KR:wght@500;600;700'
  };
  var loadedFonts = {};

  function normalize(code) {
    if (!code) return null;
    var c = String(code).toLowerCase().replace('_', '-');
    if (c === 'zh-tw' || c === 'zh-hk' || c === 'zh-mo' || c === 'zh-hant') return 'zh-TW';
    if (c.indexOf('zh') === 0) return 'zh-CN';
    if (c.indexOf('en') === 0) return 'en';
    if (c.indexOf('fr') === 0) return 'fr';
    if (c.indexOf('ja') === 0) return 'ja';
    if (c.indexOf('ko') === 0) return 'ko';
    if (c.indexOf('ru') === 0) return 'ru';
    return null;
  }

  function resolveLang() {
    try {
      var params = new URLSearchParams(window.location.search);
      var urlLang = normalize(params.get('lang'));
      if (urlLang) return urlLang;
    } catch (e) {}
    try {
      var saved = normalize(localStorage.getItem(STORAGE_KEY));
      if (saved) return saved;
    } catch (e) {}
    var nav = normalize(navigator.language || navigator.userLanguage);
    if (nav) return nav;
    return 'zh-CN';
  }

  var dicts = window.LANDING_I18N || {};
  var lang = resolveLang();

  function t(key) {
    var d = dicts[lang];
    if (d && Object.prototype.hasOwnProperty.call(d, key)) return d[key];
    var z = dicts['zh-CN'];
    if (z && Object.prototype.hasOwnProperty.call(z, key)) return z[key];
    return key;
  }

  function ensureFonts(l) {
    var q = FONT_SETS[l];
    if (!q || loadedFonts[l]) return;
    loadedFonts[l] = true;
    var link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = 'https://fonts.googleapis.com/css2?' + q + '&display=swap';
    document.head.appendChild(link);
  }

  function apply() {
    document.documentElement.lang = lang;

    // textContent on leaf [data-i18n] nodes (never clobber a parent that
    // wraps static inline children — those are split into their own leaves)
    var nodes = document.querySelectorAll('[data-i18n]');
    for (var i = 0; i < nodes.length; i++) {
      if (nodes[i].querySelector('[data-i18n]')) continue; // non-leaf: skip
      nodes[i].textContent = t(nodes[i].getAttribute('data-i18n'));
    }

    // attribute translations: data-i18n-attr="attr:key;attr2:key2"
    var attrNodes = document.querySelectorAll('[data-i18n-attr]');
    for (var j = 0; j < attrNodes.length; j++) {
      var el = attrNodes[j];
      var pairs = el.getAttribute('data-i18n-attr').split(';');
      for (var k = 0; k < pairs.length; k++) {
        var pair = pairs[k].trim();
        if (!pair) continue;
        var sep = pair.indexOf(':');
        if (sep === -1) continue;
        el.setAttribute(pair.slice(0, sep).trim(), t(pair.slice(sep + 1).trim()));
      }
    }

    // title + meta description (page is marked with <body data-page="…">)
    var page = document.body.getAttribute('data-page') || 'index';
    document.title = t('meta.' + page + '.title');
    var desc = document.querySelector('meta[name="description"]');
    if (desc) desc.setAttribute('content', t('meta.' + page + '.desc'));

    ensureFonts(lang);

    document.dispatchEvent(new CustomEvent('landing:i18n', { detail: { lang: lang } }));
  }

  /* ---------- switcher UI ---------- */
  function buildSwitcher() {
    var headerInner = document.querySelector('.header-inner');
    if (!headerInner || headerInner.querySelector('.lang-switch')) return;

    var wrap = document.createElement('div');
    wrap.className = 'lang-switch';

    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'lang-btn';
    btn.setAttribute('aria-haspopup', 'listbox');
    btn.setAttribute('aria-expanded', 'false');

    // inline <svg> with fill=currentColor — an <img>/CSS-mask reference to
    // the external SVG rasterizes to zero alpha (icon was invisible); inline
    // keeps the glyph painted and inheriting the button's text color
    var NS = 'http://www.w3.org/2000/svg';
    var icon = document.createElementNS(NS, 'svg');
    icon.setAttribute('class', 'lang-icon');
    icon.setAttribute('viewBox', '0 0 1024 1024');
    icon.setAttribute('aria-hidden', 'true');
    icon.setAttribute('focusable', 'false');
    var iconPath = document.createElementNS(NS, 'path');
    iconPath.setAttribute('fill', 'currentColor');
    iconPath.setAttribute('d', 'M213.317818 640v85.317818a85.317818 85.317818 0 0 0 78.941091 85.178182l6.423273 0.186182h128v85.317818h-128a170.682182 170.682182 0 0 1-170.682182-170.682182V640h85.317818z m512-512a170.682182 170.682182 0 0 1 170.682182 170.682182V384h-85.317818V298.682182a85.364364 85.364364 0 0 0-85.364364-85.364364h-128V128h128zM785.035636 465.454545l187.717819 469.317819h-91.927273l-51.246546-128h-174.545454l-51.106909 128H512L699.671273 465.454545h85.364363z m-42.682181 123.112728L689.152 721.454545h106.309818l-53.061818-132.887272zM349.090909 93.090909v85.317818h170.682182v298.682182H349.090909V605.090909H263.773091v-128H93.090909V178.408727h170.682182V93.090909H349.090909zM263.773091 263.773091H178.408727v128h85.364364v-128z m170.635636 0H349.090909v128h85.317818v-128z');
    icon.appendChild(iconPath);

    var current = document.createElement('span');
    current.className = 'lang-current';

    var caret = document.createElement('span');
    caret.className = 'lang-caret';
    caret.setAttribute('aria-hidden', 'true');

    btn.appendChild(icon);
    btn.appendChild(current);
    btn.appendChild(caret);

    var menu = document.createElement('ul');
    menu.className = 'lang-menu';
    menu.setAttribute('role', 'listbox');
    menu.setAttribute('aria-label', t('lang.aria'));

    var optionEls = [];
    LANGS.forEach(function (code) {
      var li = document.createElement('li');
      li.setAttribute('role', 'option');
      li.setAttribute('data-lang', code);
      li.setAttribute('id', 'lang-opt-' + code);
      li.textContent = NATIVE_NAMES[code];
      li.addEventListener('click', function () { setLang(code, true); });
      li.addEventListener('mousedown', function (e) { e.preventDefault(); }); // keep focus on button
      menu.appendChild(li);
      optionEls.push(li);
    });

    wrap.appendChild(btn);
    wrap.appendChild(menu);
    headerInner.appendChild(wrap);

    function syncMenu() {
      current.textContent = NATIVE_NAMES[lang];
      btn.setAttribute('aria-label', t('lang.aria') + ' · ' + NATIVE_NAMES[lang]);
      for (var i = 0; i < optionEls.length; i++) {
        var on = optionEls[i].getAttribute('data-lang') === lang;
        if (on) optionEls[i].setAttribute('aria-selected', 'true');
        else optionEls[i].removeAttribute('aria-selected');
        optionEls[i].classList.toggle('is-current', on);
      }
      menu.setAttribute('aria-label', t('lang.aria'));
    }

    function setOpen(open, moveFocus) {
      wrap.classList.toggle('is-open', open);
      btn.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (open && moveFocus) {
        var active = optionEls.filter(function (o) { return o.classList.contains('is-current'); })[0] || optionEls[0];
        active.focus();
      }
    }

    function close() {
      if (wrap.classList.contains('is-open')) setOpen(false, false);
    }

    btn.addEventListener('click', function () {
      setOpen(!wrap.classList.contains('is-open'), true);
    });

    document.addEventListener('click', function (e) {
      if (wrap.classList.contains('is-open') && !wrap.contains(e.target)) close();
    });
    // APG menu-button: Tab moving focus out of the widget closes the menu
    wrap.addEventListener('focusout', function (e) {
      if (wrap.classList.contains('is-open') && !wrap.contains(e.relatedTarget)) close();
    });
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape') { close(); btn.focus(); }
      if (!wrap.classList.contains('is-open')) return;
      var idx = optionEls.indexOf(document.activeElement);
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        var next = e.key === 'ArrowDown'
          ? (idx + 1) % optionEls.length
          : (idx - 1 + optionEls.length) % optionEls.length;
        optionEls[next].focus();
      } else if (e.key === 'Home') { e.preventDefault(); optionEls[0].focus(); }
      else if (e.key === 'End') { e.preventDefault(); optionEls[optionEls.length - 1].focus(); }
      else if (e.key === 'Enter' && idx >= 0) {
        e.preventDefault();
        setLang(optionEls[idx].getAttribute('data-lang'), true);
      }
    });

    window.addEventListener('resize', function () {
      if (window.innerWidth > 900) close();
    });

    syncMenu();
    return wrap;
  }

  function setLang(code, persist) {
    if (LANGS.indexOf(code) === -1) return;
    lang = code;
    if (persist) {
      try { localStorage.setItem(STORAGE_KEY, code); } catch (e) {}
    }
    apply();
    var wrap = document.querySelector('.lang-switch');
    if (wrap) {
      var current = wrap.querySelector('.lang-current');
      if (current) current.textContent = NATIVE_NAMES[code];
      wrap.classList.remove('is-open');
      var btn = wrap.querySelector('.lang-btn');
      if (btn) btn.setAttribute('aria-expanded', 'false');
      var menu = wrap.querySelector('.lang-menu');
      if (menu) {
        menu.setAttribute('aria-label', t('lang.aria'));
        var opts = menu.querySelectorAll('[role="option"]');
        for (var i = 0; i < opts.length; i++) {
          var on = opts[i].getAttribute('data-lang') === code;
          if (on) opts[i].setAttribute('aria-selected', 'true');
          else opts[i].removeAttribute('aria-selected');
          opts[i].classList.toggle('is-current', on);
        }
      }
    }
  }

  // expose for site.js (copy buttons, overflow re-sync)
  window.LANDING_I18N_T = t;
  window.LANDING_I18N_CURRENT = function () { return lang; };

  buildSwitcher();
  apply();
})();
