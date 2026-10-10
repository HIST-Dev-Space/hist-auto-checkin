import { escapeHTML } from '../common/dom.js';

export function enhanceSelect(select) {
  const control = select.closest('.select-control'),
    label = select.labels[0];
  const items = Array.from(select.options).filter((option) => option.value && !option.disabled);
  const trigger = document.createElement('button'),
    value = document.createElement('span'),
    list = document.createElement('ul');
  trigger.type = 'button';
  trigger.id = `${select.id}-trigger`;
  trigger.className = 'select-trigger';
  value.id = `${select.id}-value`;
  list.id = `${select.id}-options`;
  list.className = 'select-options';
  list.hidden = true;
  if (label) {
    label.id ||= `${select.id}-label`;
    label.htmlFor = trigger.id;
    trigger.setAttribute('aria-labelledby', `${label.id} ${value.id}`);
    list.setAttribute('aria-labelledby', label.id);
  }
  for (const [name, content] of Object.entries({
    role: 'combobox',
    'aria-haspopup': 'listbox',
    'aria-controls': list.id,
    'aria-expanded': 'false',
    'aria-required': String(select.required),
  }))
    trigger.setAttribute(name, content);
  trigger.disabled = select.disabled || !items.length;
  trigger.append(value);
  list.setAttribute('role', 'listbox');
  list.innerHTML = items
    .map(
      (option, index) =>
        `<li id="${list.id}-${index}" class="select-option" role="option" aria-selected="false" data-value="${escapeHTML(option.value)}"><span>${escapeHTML(option.textContent)}</span><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg></li>`,
    )
    .join('');
  select.hidden = true;
  select.required = false;
  select.tabIndex = -1;
  control.append(trigger, list);
  let active = -1,
    typed = '',
    typedAt = 0;
  const opened = () => !list.hidden;
  function sync() {
    value.textContent = select.selectedOptions[0]?.textContent || '请选择';
    trigger.dataset.empty = String(!select.value);
    Array.from(list.children).forEach((item) =>
      item.setAttribute('aria-selected', String(item.dataset.value === select.value)),
    );
  }
  function activate(index) {
    if (!items.length) return;
    active = Math.max(0, Math.min(index, items.length - 1));
    Array.from(list.children).forEach((item, i) =>
      item.classList.toggle('is-active', i === active),
    );
    const item = list.children[active];
    trigger.setAttribute('aria-activedescendant', item.id);
    if (item.offsetTop < list.scrollTop) list.scrollTop = item.offsetTop;
    else if (item.offsetTop + item.offsetHeight > list.scrollTop + list.clientHeight)
      list.scrollTop = item.offsetTop + item.offsetHeight - list.clientHeight;
  }
  function close() {
    list.hidden = true;
    control.dataset.open = 'false';
    trigger.setAttribute('aria-expanded', 'false');
    trigger.removeAttribute('aria-activedescendant');
    typed = '';
  }
  function open(index = items.findIndex((option) => option.value === select.value)) {
    if (trigger.disabled) return;
    const bounds = trigger.getBoundingClientRect(),
      below = innerHeight - bounds.bottom - 16,
      above = bounds.top - 16;
    const upwards = below < 180 && above > below;
    control.dataset.side = upwards ? 'top' : 'bottom';
    list.style.maxHeight = `${Math.max(80, Math.min(240, upwards ? above : below))}px`;
    list.hidden = false;
    control.dataset.open = 'true';
    trigger.setAttribute('aria-expanded', 'true');
    activate(index < 0 ? 0 : index);
  }
  function choose(index) {
    if (!items[index]) return;
    select.value = items[index].value;
    select.dispatchEvent(new Event('change', { bubbles: true }));
    close();
    trigger.focus();
  }
  trigger.addEventListener('click', () => (opened() ? close() : open()));
  trigger.addEventListener('keydown', (event) => {
    if (event.key === 'Tab') {
      close();
      return;
    }
    if (event.key === 'Escape') {
      if (opened()) {
        event.preventDefault();
        close();
      }
      return;
    }
    if (['ArrowDown', 'ArrowUp', 'Home', 'End', 'Enter', ' '].includes(event.key)) {
      event.preventDefault();
      if (event.key === 'Enter' || event.key === ' ') {
        if (opened()) choose(active);
        else open();
      } else if (event.key === 'Home' || event.key === 'End') {
        const index = event.key === 'Home' ? 0 : items.length - 1;
        if (opened()) activate(index);
        else open(index);
      } else if (opened()) activate(active + (event.key === 'ArrowDown' ? 1 : -1));
      else open(select.value ? undefined : event.key === 'ArrowUp' ? items.length - 1 : 0);
      return;
    }
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      if (!opened()) open();
      const now = Date.now();
      typed = now - typedAt > 700 ? event.key : typed + event.key;
      typedAt = now;
      const prefix = Array.from(typed).every((char) => char === typed[0]) ? typed[0] : typed;
      const start = prefix.length === 1 ? active + 1 : active;
      for (let offset = 0; offset < items.length; offset++) {
        const index = (start + offset) % items.length;
        if (items[index].textContent.toLowerCase().startsWith(prefix.toLowerCase())) {
          activate(index);
          break;
        }
      }
    }
  });
  trigger.addEventListener('blur', close);
  select.addEventListener('change', sync);
  list.addEventListener('pointerdown', (event) => event.preventDefault());
  list.addEventListener('click', (event) => {
    const item = event.target.closest('[role=option]');
    if (item && list.contains(item)) choose(Array.from(list.children).indexOf(item));
  });
  list.addEventListener('pointermove', (event) => {
    const item = event.target.closest('[role=option]');
    if (event.pointerType === 'mouse' && item && list.contains(item))
      activate(Array.from(list.children).indexOf(item));
  });
  document.addEventListener('pointerdown', (event) => {
    if (!control.contains(event.target)) close();
  });
  window.addEventListener('resize', close);
  sync();
}
