export const $ = (selector, scope = document) => scope.querySelector(selector);
export const $$ = (selector, scope = document) => [...scope.querySelectorAll(selector)];

export const escapeHTML = (value) =>
  String(value ?? '').replace(
    /[&<>"']/g,
    (char) =>
      ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;',
      })[char],
  );

let toastTimer;

export function toast(message, error = false) {
  const element = $('#toast');
  if (!element) return;
  element.textContent = message;
  element.classList.toggle('error-toast', error);
  element.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    element.hidden = true;
  }, 5000);
}

export function showError(selector, message) {
  const element = $(selector);
  if (!element) return;
  element.textContent = message;
  element.hidden = !message;
}

export async function busy(button, label, action) {
  const original = button.innerHTML;
  const disabled = button.disabled;
  button.disabled = true;
  button.textContent = label;
  try {
    return await action();
  } finally {
    button.innerHTML = original;
    button.disabled = disabled;
  }
}
