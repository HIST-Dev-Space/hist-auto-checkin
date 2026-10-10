import { api } from '../common/api.js';
import { $, showError, toast } from '../common/dom.js';
import { createPollingSession } from '../common/polling.js';

const statuses = {
  success: '已签到',
  running: '签到中',
  failed: '未完成',
  waiting: '等待签到',
  paused: '已关闭',
  unconfigured: '未设置',
  login_required: '登录已失效',
};
const refreshInterval = 15000;

export async function init() {
  const polling = createPollingSession();
  const initial = polling.restart();
  let dashboard;
  let changing = false;
  const toggle = $('#auto-checkin-toggle');

  function render(data) {
    dashboard = data;
    $('#total-checkins').textContent = data.total_successful_checkins;
    const today = $('#today-status');
    today.textContent = statuses[data.today_status] || '等待签到';
    today.dataset.state = data.today_status;
    toggle.setAttribute('aria-checked', String(data.auto_checkin_enabled));
    toggle.disabled = changing || !data.has_schedule;
    $('#configure-task').hidden = data.has_schedule;
    $('#reconnect-account').hidden = data.has_active_account;
    $('#account-recovery').hidden = data.has_schedule && data.has_active_account;
  }

  async function refreshNotification(current) {
    const status = $('#notification-status');
    try {
      const binding = await api('/api/weixin/binding', { signal: current.signal });
      if (!current.isCurrent()) return;
      const connected = binding.status === 'active' && binding.has_context;
      status.textContent = connected
        ? '已连接'
        : binding.status === 'active'
          ? '待完成连接'
          : '未连接';
      status.dataset.state = connected ? 'connected' : 'unconnected';
    } catch (error) {
      if (!current.isCurrent() || error.name === 'AbortError') return;
      status.textContent = '暂时无法获取状态';
      status.dataset.state = 'unavailable';
    }
  }

  async function refresh(current) {
    if (!current.isCurrent()) return;
    try {
      if (!document.hidden && !changing) {
        await Promise.all([
          api('/api/dashboard', { signal: current.signal }).then((data) => {
            if (current.isCurrent()) render(data);
          }),
          refreshNotification(current),
        ]);
      }
    } catch (error) {
      if (current.isCurrent() && error.name !== 'AbortError' && error.status !== 401) {
        showError('#page-error', error.message);
      }
    } finally {
      // Schedule after completion so slow requests cannot overlap the next refresh.
      current.schedule(refresh, refreshInterval);
    }
  }

  toggle.addEventListener('click', async () => {
    if (changing || !dashboard?.has_schedule) return;
    polling.stop();
    changing = true;
    toggle.disabled = true;
    toggle.setAttribute('aria-busy', 'true');
    showError('#page-error', '');
    try {
      render(
        await api('/api/auto-checkin', {
          method: 'PUT',
          body: { enabled: !dashboard.auto_checkin_enabled },
        }),
      );
    } catch (error) {
      showError('#page-error', error.message);
    } finally {
      changing = false;
      toggle.disabled = !dashboard?.has_schedule;
      toggle.removeAttribute('aria-busy');
      polling.restart().schedule(refresh, refreshInterval);
    }
  });
  $('#logout').addEventListener('click', async () => {
    polling.stop();
    try {
      await api('/api/auth/logout', { method: 'POST', body: {} });
      location.replace('/login');
    } catch (error) {
      showError('#page-error', error.message);
      polling.restart().schedule(refresh, refreshInterval);
    }
  });

  await api('/api/me', { signal: initial.signal });
  const notificationReady = refreshNotification(initial);
  const [data, rules] = await Promise.all([
    api('/api/dashboard', { signal: initial.signal }),
    api('/api/schedules', { signal: initial.signal }),
  ]);
  if (!initial.isCurrent()) return;
  render(data);
  const rule = rules.at(-1);
  if (rule) {
    $('#current-schedule').hidden = false;
    $('#schedule-time').textContent = rule.local_time.slice(0, 5);
    $('#schedule-location').textContent = rule.location_name;
    $('#edit-schedule').href = `/tasks?edit=${encodeURIComponent(rule.id)}`;
  }
  await notificationReady;
  if (!initial.isCurrent()) return;

  const url = new URL(location.href);
  const saved = url.searchParams.get('saved');
  if (['schedule', 'notifications'].includes(saved)) {
    toast(saved === 'schedule' ? '签到设置已保存' : '微信通知已连接');
    url.searchParams.delete('saved');
    history.replaceState(null, '', url.pathname + url.search + url.hash);
  }
  initial.schedule(refresh, refreshInterval);
}
