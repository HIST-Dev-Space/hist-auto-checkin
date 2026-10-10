const protectedPages = new Set(['tasks', 'notifications', 'account']);

export async function api(path, options = {}) {
  let response;
  try {
    response = await fetch(path, {
      credentials: 'same-origin',
      ...options,
      headers: {
        'Content-Type': 'application/json',
        'X-Hist-Request': '1',
        ...options.headers,
      },
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
  } catch (error) {
    if (error.name === 'AbortError') throw error;
    throw new Error('暂时无法连接服务，请检查网络后重试。');
  }

  const body = response.status === 204 ? null : await response.json().catch(() => null);
  options.signal?.throwIfAborted();
  if (!response.ok) {
    const error = new Error(
      (body?.error || `请求未完成（${response.status}），请稍后重试。`).replace(
        /Cookie/gi,
        '登录凭证',
      ),
    );
    error.status = response.status;
    error.code = body?.code;
    error.scanConfirmed = body?.scan_confirmed === true;
    if (response.status === 401 && protectedPages.has(document.body.dataset.page)) {
      location.replace('/login');
    }
    throw error;
  }
  return body;
}

export async function continueAfterLogin(signal) {
  const rules = await api('/api/schedules', { signal });
  signal?.throwIfAborted();
  location.replace(rules.length ? '/account' : '/tasks');
}
