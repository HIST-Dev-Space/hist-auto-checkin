'use strict';
// Apply the saved theme before the stylesheet renders the introduction page.
(() => {
  let theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  try {
    const preference = localStorage.getItem('hist-theme');
    if (preference === 'light' || preference === 'dark') theme = preference;
  } catch {}
  document.documentElement.dataset.theme = theme;
})();
