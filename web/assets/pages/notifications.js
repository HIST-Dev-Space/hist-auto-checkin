import { api } from '../common/api.js';
import { $, busy, showError } from '../common/dom.js';
import { createPollingSession } from '../common/polling.js';
import { celebrateSetup } from '../components/celebration.js';
import { drawQR } from '../components/qr-code.js';

export async function init() {
  const polling = createPollingSession();
  let session = polling.restart();
  let loginId;
  let expiresAt;
  let hasBinding = false;
  let finished = false;
  const params = new URLSearchParams(location.search);
  const setup = params.get('setup') === '1' && !params.has('manage');
  const showStatus = (message) => {
    $('#notify-qr-status').textContent = message;
  };

  function complete() {
    if (finished) return;
    finished = true;
    polling.stop();
    if (setup) celebrateSetup();
    else location.replace('/account?saved=notifications');
  }

  async function checkBinding(current = session) {
    if (!current.isCurrent() || finished) return;
    current.clearTimer();
    const binding = await api('/api/weixin/binding', { signal: current.signal });
    if (!current.isCurrent() || finished) return;
    hasBinding = binding.status !== 'unbound';
    $('#bind-weixin').hidden = false;
    $('#bind-weixin').textContent = hasBinding ? '重新绑定微信' : '获取二维码';
    if (binding.status === 'active' && binding.has_context) {
      showStatus('微信通知已连接');
      $('#notify-login').hidden = true;
      $('#notify-verify').hidden = true;
      $('#finish-notification').hidden = false;
      $('#finish-notification').textContent = '确认';
      if (setup) complete();
    } else if (binding.status === 'active') {
      showStatus('请向微信机器人发送一条消息');
      $('#notify-login').hidden = true;
      $('#notify-verify').hidden = true;
      $('#finish-notification').hidden = false;
      $('#finish-notification').textContent = '检查连接';
      current.schedule(checkConnection, 2000);
    }
    return binding;
  }

  async function checkConnection(current) {
    try {
      await checkBinding(current);
    } catch (error) {
      if (current.isCurrent() && error.name !== 'AbortError') {
        showError('#page-error', error.message);
        current.schedule(checkConnection, 4000);
      }
    }
  }

  async function poll(current, verifyCode) {
    if (!current.isCurrent() || finished) return;
    current.clearTimer();
    if (Date.now() >= expiresAt) {
      showStatus('二维码已过期');
      return;
    }
    try {
      const result = await api(`/api/weixin/login/${loginId}/poll`, {
        method: 'POST',
        body: verifyCode ? { verify_code: verifyCode } : {},
        signal: current.signal,
      });
      if (!current.isCurrent() || finished) return;
      if (result.status === 'confirmed') {
        await checkBinding(current);
        return;
      }
      if (result.status === 'need_verifycode') {
        $('#notify-verify').hidden = false;
        showStatus('请输入微信验证码');
        $('#notify-verify-code').focus();
        return;
      }
      if (result.status === 'binded_redirect') {
        const binding = await checkBinding(current);
        if (!current.isCurrent() || finished) return;
        $('#notify-login').hidden = true;
        $('#notify-verify').hidden = true;
        showStatus(
          binding?.status === 'active'
            ? '此微信 Bot 已绑定，本次扫码未更换绑定'
            : '此微信 Bot 已绑定，请重新连接',
        );
        return;
      }
      if (result.status === 'expired') {
        showStatus('二维码已过期，请重新获取');
        return;
      }
      if (result.status === 'verify_code_blocked') {
        showStatus('验证码错误次数过多，请稍后重新获取二维码');
        return;
      }
      showStatus(result.status === 'scaned' ? '请在微信中确认' : '使用微信扫码');
      current.schedule(poll, 1800);
    } catch (error) {
      if (!current.isCurrent() || error.name === 'AbortError') return;
      showStatus('正在重新连接');
      current.schedule((next) => poll(next, verifyCode), 4000);
    }
  }

  async function begin() {
    const current = (session = polling.restart());
    showError('#page-error', '');
    $('#notify-verify').hidden = true;
    $('#finish-notification').hidden = true;
    $('#notify-login').hidden = false;
    $('#notify-qr').innerHTML = '<div class="spinner" aria-label="正在获取二维码"></div>';
    showStatus('正在获取二维码');
    try {
      await busy($('#bind-weixin'), '获取中', async () => {
        const result = await api('/api/weixin/login', {
          method: 'POST',
          body: { rebind: hasBinding },
          signal: current.signal,
        });
        if (!current.isCurrent()) return;
        loginId = result.session_id;
        expiresAt = Date.parse(result.expires_at);
        await drawQR($('#notify-qr'), result.qrcode_img_content, current.signal);
        if (!current.isCurrent()) return;
        showStatus('使用微信扫码');
        current.schedule(poll, 1500);
      });
    } catch (error) {
      if (current.isCurrent() && error.name !== 'AbortError') {
        showError('#page-error', error.message);
        showStatus('获取失败');
      }
    }
    if (current.isCurrent()) $('#bind-weixin').textContent = '重新获取二维码';
  }

  $('#bind-weixin').addEventListener('click', begin);
  $('#submit-notify-verify').addEventListener('click', async () => {
    const code = $('#notify-verify-code').value.trim();
    if (!code) {
      $('#notify-verify-code').focus();
      return;
    }
    const current = session;
    await busy($('#submit-notify-verify'), '确认中', () => poll(current, code));
  });
  $('#finish-notification').addEventListener('click', async () => {
    const current = session;
    try {
      const binding = await checkBinding(current);
      if (binding?.status === 'active' && binding.has_context) complete();
    } catch (error) {
      if (current.isCurrent() && error.name !== 'AbortError')
        showError('#page-error', error.message);
    }
  });

  await api('/api/me', { signal: session.signal });
  if (setup) {
    const rules = await api('/api/schedules', { signal: session.signal });
    if (!rules.length) {
      location.replace('/tasks?setup=1');
      return;
    }
    $('#notification-back').href = `/tasks?setup=1&edit=${encodeURIComponent(rules.at(-1).id)}`;
    $('#notification-back').hidden = false;
  }
  const binding = await checkBinding();
  if (session.isCurrent() && binding?.status !== 'active') await begin();
}
