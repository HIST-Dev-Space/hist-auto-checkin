import { api, continueAfterLogin } from '../common/api.js';
import { $, $$, busy, showError } from '../common/dom.js';
import { createPollingSession } from '../common/polling.js';
import { drawQR } from '../components/qr-code.js';

export async function init() {
  const controller = new AbortController();
  window.addEventListener('pagehide', () => controller.abort());
  const polling = createPollingSession();
  let session = polling.restart();
  if (!new URLSearchParams(location.search).has('reconnect')) {
    try {
      await api('/api/me', { signal: controller.signal });
      await continueAfterLogin(controller.signal);
      return;
    } catch (error) {
      if (error.name === 'AbortError') throw error;
      if (error.status !== 401) showError('#login-error', error.message);
    }
  }

  let method = 'wechat';
  const cache = { wechat: null, identity: null };
  let loading = false;
  let finished = false;
  let pollRequest;

  function reusable(entry) {
    return entry && Date.now() < entry.expiresAt && Date.now() < entry.reusableUntil;
  }

  function setLoading(value) {
    loading = value;
    $$('[data-login-method]').forEach((button) => {
      button.disabled = value;
    });
    $('#refresh-school-qr').disabled = value;
    $('#refresh-captcha').disabled = value;
    $('#submit-school-login').disabled = value || !cache.identity?.captcha_data_url;
  }

  function remember(selected, result, previous) {
    const expiresAt = Date.parse(result.expires_at) || previous?.expiresAt || 0;
    return {
      ...result,
      expiresAt,
      reusableUntil:
        selected === 'identity'
          ? Math.min(Date.parse(result.reusable_until) || Date.now() + 60000, expiresAt)
          : Math.min(Date.now() + 180000, expiresAt),
      scanConfirmed: false,
      status: result.message || '等待扫码',
      error: '',
    };
  }

  async function finishLogin() {
    if (finished) return;
    finished = true;
    polling.stop();
    cache.wechat = cache.identity = null;
    setLoading(true);
    try {
      await continueAfterLogin(controller.signal);
    } catch (error) {
      if (error.name === 'AbortError') return;
      showError('#login-error', error.message);
      session = polling.restart();
      session.schedule(() => {
        finished = false;
        finishLogin();
      }, 5000);
    }
  }

  function selectTab(selected) {
    method = selected;
    $$('[data-login-method]').forEach((button) => {
      const active = button.dataset.loginMethod === selected;
      button.classList.toggle('active', active);
      button.setAttribute('aria-selected', String(active));
    });
    $('#wechat-panel').hidden = selected !== 'wechat';
    $('#identity-panel').hidden = selected !== 'identity';
  }

  async function display(selected, entry, current, delay) {
    if (selected === 'wechat') {
      if (!$('#school-qr img')) await drawQR($('#school-qr'), entry.qr_content, current.signal);
      if (!current.isCurrent()) return;
      $('#school-qr-status').textContent = entry.status;
      current.schedule(poll, delay);
    } else {
      $('#captcha-image').src = entry.captcha_data_url;
      $('#captcha-image').hidden = false;
      $('#captcha-placeholder').hidden = true;
    }
    showError('#login-error', entry.error);
    setLoading(false);
  }

  async function begin(selected, force = false) {
    if (loading || finished) return;
    const current = (session = polling.restart());
    selectTab(selected);
    showError('#login-error', '');
    const previous = cache[selected];
    if (!force && reusable(previous)) {
      try {
        await display(selected, previous, current, 0);
      } catch (error) {
        if (current.isCurrent() && error.name !== 'AbortError')
          showError('#login-error', error.message);
      }
      return;
    }
    setLoading(true);
    if (selected === 'wechat') {
      cache.wechat = null;
      $('#school-qr').innerHTML = '<div class="spinner" aria-label="正在获取二维码"></div>';
      $('#school-qr-status').textContent = '正在获取二维码';
    } else {
      // A refresh may replace the school's challenge even if its response is lost.
      if (previous) {
        previous.captcha_data_url = '';
        previous.reusableUntil = 0;
      }
      $('#captcha-image').hidden = true;
      $('#captcha-placeholder').hidden = false;
      $('#captcha-placeholder').textContent = '获取中';
      $('#submit-school-login').disabled = true;
    }
    try {
      // Let an in-flight poll finish before replacing or submitting a login session.
      await pollRequest;
      if (finished || !current.isCurrent()) return;
      const refreshCaptcha = selected === 'identity' && previous?.expiresAt > Date.now();
      const result = await api(
        refreshCaptcha ? '/api/school-login/captcha' : '/api/school-login/start',
        {
          method: 'POST',
          body: refreshCaptcha ? {} : { method: selected },
          signal: controller.signal,
        },
      );
      if (!current.isCurrent()) return;
      cache[selected] = remember(selected, result, previous);
      if (selected === 'identity') $('#captcha-code').value = '';
      await display(selected, cache[selected], current, 1800);
    } catch (error) {
      if (current.isCurrent() && error.name !== 'AbortError') {
        if (error.status === 401) cache[selected] = null;
        showError('#login-error', error.message);
        if (selected === 'wechat') {
          $('#school-qr-status').textContent = '获取失败，请重新获取二维码';
        } else {
          $('#captcha-placeholder').textContent = '获取失败，点击重试';
        }
      }
    } finally {
      if (current.isCurrent() && !finished) setLoading(false);
    }
  }

  async function poll(current) {
    if (!current.isCurrent() || loading || finished || pollRequest) return;
    const entry = cache.wechat;
    if (!entry || Date.now() >= entry.expiresAt) {
      $('#school-qr-status').textContent = '二维码已过期';
      return;
    }
    pollRequest = (async () => {
      let delay = 2000;
      try {
        const result = await api('/api/school-login/poll', {
          method: 'POST',
          body: {},
          signal: controller.signal,
        });
        if (result.status === 'authenticated') {
          await finishLogin();
          return;
        }
        entry.status = entry.scanConfirmed ? '微信已确认登录，正在获取学校凭证' : '等待扫码';
      } catch (error) {
        if (error.name === 'AbortError') return;
        delay = 5000;
        if (error.status === 401) {
          entry.expiresAt = 0;
          entry.status = '登录会话已过期，请重新获取二维码';
        } else if (error.scanConfirmed || error.code === 'school_session_exchange_failed') {
          entry.scanConfirmed = true;
          entry.error = error.message;
          entry.status = '微信已确认登录，学校凭证尚未取得';
        } else {
          entry.status = '正在重新连接学校登录';
        }
      } finally {
        pollRequest = undefined;
        if (!finished && !loading && method === 'wechat' && session.isCurrent()) {
          $('#school-qr-status').textContent = entry.status;
          showError('#login-error', entry.error);
          if (Date.now() < entry.expiresAt) session.schedule(poll, delay);
        }
      }
    })();
    await pollRequest;
  }

  $$('[data-login-method]').forEach((button) => {
    button.addEventListener('click', () => {
      if (method !== button.dataset.loginMethod) begin(button.dataset.loginMethod);
    });
    button.addEventListener('keydown', (event) => {
      if (button.disabled || !['ArrowLeft', 'ArrowRight'].includes(event.key)) return;
      event.preventDefault();
      const next = method === 'wechat' ? 'identity' : 'wechat';
      $(`[data-login-method="${next}"]`).focus();
      begin(next);
    });
  });

  $('#refresh-school-qr').addEventListener('click', () => begin('wechat', true));
  $('#refresh-captcha').addEventListener('click', () => begin('identity', true));

  $('#school-login-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (loading || finished) return;
    if (!reusable(cache.identity)) {
      await begin('identity', true);
      if (cache.identity?.captcha_data_url)
        showError('#login-error', '验证码已过期，请输入新的验证码');
      return;
    }
    const current = session;
    const entry = cache.identity;
    const reusableUntil = entry.reusableUntil;
    showError('#login-error', '');
    setLoading(true);
    let failure;
    let refreshAfterFailure = true;
    try {
      await busy($('#submit-school-login'), '正在验证身份', async () => {
        await pollRequest;
        if (finished || !current.isCurrent()) return;
        if (!reusable(entry)) throw new Error('验证码已过期，请输入新的验证码');
        cache.identity.reusableUntil = 0;
        const result = await api('/api/school-login/identity', {
          method: 'POST',
          body: {
            name: $('#login-name').value.trim(),
            student_no: $('#student-no').value.trim(),
            captcha_code: $('#captcha-code').value.trim(),
          },
          signal: controller.signal,
        });
        if (!current.isCurrent()) return;
        if (result.status === 'authenticated') {
          await finishLogin();
          return;
        }
        if (result.status === 'need_wechat') {
          const next = (session = polling.restart());
          cache.wechat = remember('wechat', result, cache.identity);
          cache.identity = null;
          selectTab('wechat');
          await drawQR($('#school-qr'), result.qr_content, next.signal);
          if (!next.isCurrent()) return;
          await display('wechat', cache.wechat, next, 1500);
        }
      });
    } catch (error) {
      if (error.name !== 'AbortError') {
        if (error.status === 401) cache.identity = null;
        if (error.code === 'login_session_busy' || error.status === 429) {
          entry.reusableUntil = reusableUntil;
          refreshAfterFailure = false;
        }
        failure = error.message;
      }
    } finally {
      if (!finished) setLoading(false);
    }
    if (failure && current.isCurrent()) {
      if (refreshAfterFailure) await begin('identity', true);
      showError('#login-error', failure);
    }
  });

  await begin('wechat');
}
