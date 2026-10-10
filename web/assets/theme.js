'use strict';
// All pages apply the same theme before styles render and share one controller.
(() => {
  const systemTheme = matchMedia('(prefers-color-scheme: dark)');
  let preference;
  try {
    const saved = localStorage.getItem('hist-theme');
    if (saved === 'light' || saved === 'dark') preference = saved;
  } catch {}

  function applyTheme() {
    const theme = preference || (systemTheme.matches ? 'dark' : 'light');
    document.documentElement.dataset.theme = theme;
    document.querySelectorAll('.theme-toggle').forEach((button) => {
      button.setAttribute('aria-checked', String(theme === 'dark'));
      button.title = theme === 'dark' ? '切换浅色模式' : '切换深色模式';
    });
    const background = getComputedStyle(document.documentElement).getPropertyValue('--bg').trim();
    const themeColor = document.querySelector('meta[name=theme-color]');
    if (themeColor && background) themeColor.content = background;
  }

  applyTheme();
  document.addEventListener('DOMContentLoaded', applyTheme);
  document.addEventListener('click', (event) => {
    if (!event.target.closest('.theme-toggle')) return;
    preference = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
    applyTheme();
    try {
      localStorage.setItem('hist-theme', preference);
    } catch {}
  });
  systemTheme.addEventListener('change', () => {
    if (!preference) applyTheme();
  });
  window.addEventListener('storage', (event) => {
    if (event.key !== 'hist-theme' && event.key !== null) return;
    preference =
      event.newValue === 'light' || event.newValue === 'dark' ? event.newValue : undefined;
    applyTheme();
  });
})();
