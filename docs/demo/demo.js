// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

'use strict';
(() => {
  const data = window.BAGEO_DEMO;
  if (!data || !data.en) return;
  const $ = id => document.getElementById(id);
  const params = new URLSearchParams(location.search);
  const browserLanguage = navigator.language || 'en';
  const preferred = /^zh-(TW|HK|Hant)/i.test(browserLanguage) ? 'zh-TW' : /^zh/i.test(browserLanguage) ? 'zh-CN' : browserLanguage.split('-')[0];
  let locale = Object.hasOwn(data, params.get('lang')) ? params.get('lang') : (Object.hasOwn(data, preferred) ? preferred : 'en');
  let scenario = ['delivery', 'sales', 'booking'].indexOf(params.get('scene'));
  if (scenario < 0) scenario = 0;
  let count = 0;
  let timer = null;
  const icons = ['↙', '∞', '↗'];
  const current = () => data[locale];
  const scene = () => current().scenes[scenario];
  const stop = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    $('autoplay').setAttribute('aria-pressed', 'false');
    $('autoplay').textContent = '▷ ' + current().auto;
  };
  function saveSelection() {
    try {
      const url = new URL(location.href);
      url.searchParams.set('lang', locale);
      url.searchParams.set('scene', scene().id);
      history.replaceState(null, '', url);
    } catch (_) { /* Downloaded local copies still work without history access. */ }
  }
  function controls() {
    const text = current();
    const total = scene().lines.length;
    const done = count === total;
    $('phone-title').textContent = done ? text.done : count ? text.active : scenario === 2 ? text.outgoing : text.incoming;
    $('count').textContent = String(count).padStart(2,'0') + ' / ' + String(total).padStart(2,'0');
    $('progress').setAttribute('aria-label', text.progress);
    $('progress').setAttribute('aria-valuemax', String(total));
    $('progress').setAttribute('aria-valuenow', String(count));
    $('progress').firstElementChild.style.width = (count / total * 100) + '%';
    $('advance-text').textContent = done ? text.replay : count ? text.next : scenario === 2 ? text.call : text.start;
    $('advance-icon').textContent = done ? '↺' : count ? '→' : '↗';
    $('reset').disabled = count === 0;
    $('autoplay').disabled = done;
  }
  function reset() {
    stop();
    count = 0;
    $('messages').replaceChildren();
    $('ring-screen').hidden = false;
    $('result').hidden = true;
    controls();
  }
  function next() {
    if (count === scene().lines.length) reset();
    $('ring-screen').hidden = true;
    const [role, text] = scene().lines[count];
    const item = document.createElement('li');
    item.className = 'message ' + role;
    const speaker = document.createElement('span');
    speaker.className = 'speaker';
    speaker.textContent = current()[role];
    const line = document.createElement('p');
    line.textContent = text;
    item.append(speaker, line);
    $('messages').append(item);
    count++;
    if (count === scene().lines.length) {
      stop();
      $('outcome').textContent = scene().outcome;
      $('result').hidden = false;
    }
    controls();
  }
  function autoplay() {
    if (timer !== null) { stop(); return; }
    const tick = () => {
      next();
      if (count < scene().lines.length) {
        const line = scene().lines[count - 1][1];
        // Leave enough time for longer translations; manual advance is always available.
        timer = setTimeout(tick, Math.max(2200, Math.min(5500, line.length * 55)));
        $('autoplay').textContent = 'Ⅱ ' + current().pause;
        $('autoplay').setAttribute('aria-pressed', 'true');
      }
    };
    tick();
  }
  function render() {
    reset();
    const text = current();
    document.documentElement.lang = locale;
    document.title = 'Bageo — ' + text.hero.join(' ');
    document.querySelectorAll('[data-copy]').forEach(el => { el.textContent = text[el.dataset.copy]; });
    $('hero-first').textContent = text.hero[0];
    $('hero-second').textContent = text.hero[1];
    $('greeting').textContent = text.greeting;
    $('scene-title').textContent = scene().teaser;
    $('scene-description').textContent = scene().description;
    $('caller-title').textContent = scene().title;
    $('caller-teaser').textContent = scene().teaser;
    $('task').textContent = scene().task;
    $('task').hidden = !scene().task;
    $('scenes').setAttribute('aria-label', text.choose);
    $('scenes').replaceChildren(...text.scenes.map((s, index) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'scenario';
      button.dataset.scene = s.id;
      button.setAttribute('aria-pressed', String(index === scenario));
      const icon = document.createElement('span');
      icon.className = 'scene-icon';
      icon.setAttribute('aria-hidden', 'true');
      icon.textContent = icons[index];
      const label = document.createElement('span');
      label.textContent = s.title;
      button.append(icon,label);
      button.addEventListener('click', () => {
        scenario = index;
        render();
        $('scenes').children[index].focus({preventScroll:true});
      });
      return button;
    }));
    $('languages').replaceChildren(...Object.entries(data).map(([key,value]) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'language';
      button.lang = key;
      button.dataset.locale = key;
      button.textContent = value.name;
      button.setAttribute('aria-pressed',String(key === locale));
      button.addEventListener('click', () => {
        locale = key;
        render();
        Array.from($('languages').children).find(b => b.dataset.locale === key).focus({preventScroll:true});
      });
      return button;
    }));
    const readme = locale === 'en' ? 'README.md' : 'README.' + locale + '.md';
    $('setup').href = 'https://github.com/h4lo-void/bageo-call/blob/main/' + readme;
    saveSelection();
    controls();
  }
  $('advance').addEventListener('click', () => { stop(); next(); });
  $('reset').addEventListener('click',reset);
  $('autoplay').addEventListener('click',autoplay);
  document.addEventListener('visibilitychange', () => { if (document.hidden) stop(); });
  window.addEventListener('pagehide',stop);
  render();
})();
