'use strict';
const $ = (selector, scope = document) => scope.querySelector(selector);
const $$ = (selector, scope = document) => [...scope.querySelectorAll(selector)];
const page = document.body.dataset.page;
const svgIcon = paths => `<svg class="inline-icon" aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${paths}</svg>`;
const icons = {'⌖':svgIcon('<path d="M20 10c0 6-8 12-8 12S4 16 4 10a8 8 0 1 1 16 0Z"/><circle cx="12" cy="10" r="2.5"/>'),'▧':svgIcon('<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/><path d="M12 14v3"/>'),'◷':svgIcon('<circle cx="12" cy="12" r="9"/><path d="M12 6v6l4 2"/>'),'▤':svgIcon('<path d="M5 4h14v12H9l-4 4V4Z"/><path d="M8 8h8M8 12h5"/>'),'☷':svgIcon('<path d="M9 5h11M9 12h11M9 19h11"/><path d="m3 5 1 1 2-2m-3 8 1 1 2-2m-3 8 1 1 2-2"/>')};
function decorateIcons(scope = document) {$$('span[aria-hidden=true]',scope).forEach(element => {const icon = icons[element.textContent.trim()];if (icon) element.innerHTML = icon;});}
decorateIcons();
const escapeHTML = value => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));
let toastTimer;
function toast(message, error = false) {
  const element = $('#toast'); if (!element) return;
  element.textContent = message; element.classList.toggle('error-toast', error); element.hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { element.hidden = true; }, 5000);
}
function showError(selector, message) { const element = $(selector); element.textContent = message; element.hidden = !message; }
async function api(path, options = {}) {
  let response;
  try { response = await fetch(path, {credentials:'same-origin', ...options, headers:{'Content-Type':'application/json','X-Hist-Request':'1',...(options.headers || {})}, body:options.body === undefined ? undefined : JSON.stringify(options.body)}); }
  catch (error) { if (error.name === 'AbortError') throw error; throw new Error('暂时无法连接服务，请检查网络后重试。'); }
  const body = response.status === 204 ? null : await response.json().catch(() => null);
  if (!response.ok) {
    const error = new Error((body?.error || `请求未完成（${response.status}），请稍后重试。`).replace(/Cookie/gi, '登录凭证'));
    error.status = response.status;
    error.code = body?.code;
    error.scanConfirmed = body?.scan_confirmed === true;
    if (response.status === 401 && page === 'tasks') location.replace('/login');
    throw error;
  }
  return body;
}
async function busy(button, label, action) {
  const original = button.innerHTML; button.disabled = true; button.textContent = label;
  try { return await action(); } finally { button.innerHTML = original; button.disabled = false; }
}
async function drawQR(container, content) {
  if (!window.QRCode || !content) throw new Error('二维码暂时无法显示，请刷新后重试。');
  const image = new Image(); image.alt = '微信扫码二维码';
  image.src = await window.QRCode.toDataURL(content, {width:440,margin:2,errorCorrectionLevel:'M',color:{dark:'#172641',light:'#ffffff'}});
  container.replaceChildren(image);
}
function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  const label = theme === 'dark' ? '切换浅色模式' : '切换深色模式';
  $$('.theme-toggle').forEach(button => {
    if (button.getAttribute('role') === 'switch') button.setAttribute('aria-checked', String(theme === 'dark'));
    else {button.textContent = theme === 'dark' ? '☾' : '☼';button.setAttribute('aria-label', label);}
    button.title = label;
  });
  const themeColor = $('meta[name=theme-color]');
  if (themeColor) themeColor.content = getComputedStyle(document.body).getPropertyValue('--bg').trim();
}
const systemTheme = matchMedia('(prefers-color-scheme: dark)');
let themePreference;
try {const saved = localStorage.getItem('hist-theme');if (saved === 'light' || saved === 'dark') themePreference = saved;} catch {}
setTheme(themePreference || (systemTheme.matches ? 'dark' : 'light'));
systemTheme.addEventListener('change', event => {if (!themePreference) setTheme(event.matches ? 'dark' : 'light');});
$$('.theme-toggle').forEach(button => button.addEventListener('click', () => {
  themePreference = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
  setTheme(themePreference);
  try {localStorage.setItem('hist-theme', themePreference);} catch {}
}));

async function loginPage() {
  if (!new URLSearchParams(location.search).has('reconnect')) {
    try {await api('/api/me');location.replace('/tasks');return;} catch (error) {if (error.status !== 401) showError('#login-error', error.message);}
  }
  let generation = 0, timer, expiresAt = 0, requestController, method = 'wechat', scanConfirmed = false;
  function selectTab(selected) {
    method = selected;
    $$('[data-login-method]').forEach(button => {const active = button.dataset.loginMethod === selected;button.classList.toggle('active', active);button.setAttribute('aria-selected', String(active));});
    $('#wechat-panel').hidden = selected !== 'wechat';$('#identity-panel').hidden = selected !== 'identity';
  }
  function stop() {clearTimeout(timer);requestController?.abort();}
  function loadingQR() {$('#school-qr').innerHTML = '<div class="spinner" aria-label="正在获取二维码"></div>';$('#school-qr-status').textContent = '正在获取二维码…';}
  async function begin(selected) {
    stop();const current = ++generation;scanConfirmed = false;selectTab(selected);showError('#login-error','');
    $$('[data-login-method]').forEach(button => {button.disabled = true;});$('#refresh-school-qr').disabled = true;
    if (selected === 'wechat') loadingQR(); else {$('#captcha-image').hidden = true;$('#captcha-placeholder').hidden = false;$('#submit-school-login').disabled = true;}
    try {
      const result = await api('/api/school-login/start',{method:'POST',body:{method:selected}});
      if (current !== generation) return;
      expiresAt = Date.parse(result.expires_at);
      if (selected === 'wechat') {await drawQR($('#school-qr'),result.qr_content);$('#school-qr-status').textContent = '等待微信扫码';timer = setTimeout(() => poll(current),1800);}
      else {$('#captcha-image').src = result.captcha_data_url;$('#captcha-image').hidden = false;$('#captcha-placeholder').hidden = true;$('#captcha-code').value = '';$('#submit-school-login').disabled = false;}
    } catch (error) {if (current === generation) {showError('#login-error',error.message);$('#school-qr-status').textContent = '获取失败，请重新获取二维码';}}
    finally {if (current === generation) {$$('[data-login-method]').forEach(button => {button.disabled = false;});$('#refresh-school-qr').disabled = false;}}
  }
  async function poll(current) {
    if (current !== generation) return;
    if (Date.now() >= expiresAt) {$('#school-qr-status').textContent = '二维码已过期，请重新获取';return;}
    requestController = new AbortController();
    try {
      const result = await api('/api/school-login/poll',{method:'POST',body:{},signal:requestController.signal});
      if (current !== generation) return;
      if (result.status === 'authenticated') {$('#school-qr-status').textContent = '登录成功，正在进入任务设置…';stop();location.replace('/tasks');return;}
      $('#school-qr-status').textContent = scanConfirmed ? '微信已确认登录，正在获取学校凭证…' : '等待扫码 · 请在手机上确认登录';
      timer = setTimeout(() => poll(current),2000);
    } catch (error) {
      if (current !== generation || error.name === 'AbortError') return;
      if (error.status === 401 || error.status === 409) {$('#school-qr-status').textContent = '登录会话已过期，请重新获取二维码';return;}
      if (error.scanConfirmed || error.code === 'school_session_exchange_failed') {
        scanConfirmed = true;showError('#login-error',error.message);$('#school-qr-status').textContent = '微信已确认登录，学校凭证尚未取得';
      } else {$('#school-qr-status').textContent = '正在重新连接学校登录…';}
      timer = setTimeout(() => poll(current),5000);
    }
  }
  $$('[data-login-method]').forEach(button => {
    button.addEventListener('click',() => {if (method !== button.dataset.loginMethod) begin(button.dataset.loginMethod);});
    button.addEventListener('keydown', event => {if (['ArrowLeft','ArrowRight'].includes(event.key)) {event.preventDefault();const next = method === 'wechat' ? 'identity' : 'wechat';$(`[data-login-method="${next}"]`).focus();begin(next);}});
  });
  $('#refresh-school-qr').addEventListener('click',() => begin('wechat'));
  $('#refresh-captcha').addEventListener('click',async () => {
    const button = $('#refresh-captcha');button.disabled = true;
    try {const result = await api('/api/school-login/captcha',{method:'POST',body:{}});$('#captcha-image').src = result.captcha_data_url;$('#captcha-code').value = '';}
    catch (error) {showError('#login-error',error.message);}
    finally {button.disabled = false;}
  });
  $('#school-login-form').addEventListener('submit',async event => {
    event.preventDefault();showError('#login-error','');
    try {
      await busy($('#submit-school-login'),'正在验证学校身份…',async () => {
        const result = await api('/api/school-login/identity',{method:'POST',body:{name:$('#login-name').value.trim(),student_no:$('#student-no').value.trim(),captcha_code:$('#captcha-code').value.trim()}});
        if (result.status === 'authenticated') {location.replace('/tasks');return;}
        if (result.status === 'need_wechat') {stop();const current = ++generation;selectTab('wechat');await drawQR($('#school-qr'),result.qr_content);$('#school-qr-status').textContent = result.message;timer = setTimeout(() => poll(current),1500);}
      });
    } catch (error) {showError('#login-error',error.message);}
  });
  window.addEventListener('beforeunload',stop);await begin('wechat');
}

async function tasksPage() {
  let accounts = [], account, places = [], selectedPlace = null, placeKind = 'dorm', schedules = [], editing = null;
  let notifyGeneration = 0, notifyTimer, notifyController, notifyLoginId, notifyExpiresAt = 0;
  const days = ['一','二','三','四','五','六','日'];
  const formatDate = (value, timezone = 'Asia/Shanghai') => new Intl.DateTimeFormat('zh-CN',{timeZone:timezone,month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hour12:false}).format(new Date(value));
  const daysText = values => values.length === 7 ? '每天' : `周${values.map(value => days[value - 1]).join('、')}`;
  function formReady() {$('#save-schedule').disabled = !account || account.credential_status !== 'active' || !$$('#courses-list input:checked').length || !$$('input[name=weekday]:checked').length || !(placeKind === 'custom' ? $('#custom-name').value.trim() && $('#custom-latitude').value !== '' && $('#custom-longitude').value !== '' : selectedPlace);}
  function renderPlaces() {
    const search = $('#building-search').value.trim();
    const visible = places.filter(place => place.kind === placeKind && place.name.includes(search));
    $('#building-list').innerHTML = visible.length ? visible.map(place => `<button class="building-option${selectedPlace?.id === place.id ? ' selected' : ''}" type="button" data-place="${escapeHTML(place.id)}" aria-pressed="${selectedPlace?.id === place.id}">${escapeHTML(place.name)}</button>`).join('') : '<p class="empty-text">没有找到匹配的楼宇</p>';
  }
  function locationSummary() {
    let name = selectedPlace?.name, lat = selectedPlace?.latitude, lng = selectedPlace?.longitude;
    if (placeKind === 'custom') {name = $('#custom-name').value.trim();lat = $('#custom-latitude').value;lng = $('#custom-longitude').value;}
    $('#selected-location').innerHTML = `<span aria-hidden="true">⌖</span><div><strong>${escapeHTML(name || '尚未选择位置')}</strong><span>${name && lat !== undefined && lat !== '' && lng !== '' ? escapeHTML(`${lat}, ${lng}`) : '选择后将用于本任务的签到'}</span></div>`;
    decorateIcons($('#selected-location'));
    formReady();
  }
  function selectPlaceKind(kind) {
    placeKind = kind;$('#building-panel').hidden = kind === 'custom';$('#custom-location').hidden = kind !== 'custom';
    ['#custom-name','#custom-latitude','#custom-longitude'].forEach(selector => {$(selector).required = kind === 'custom';});
    $$('[data-location-kind]').forEach(button => {const active = button.dataset.locationKind === kind;button.classList.toggle('active',active);button.setAttribute('aria-selected',String(active));});
    renderPlaces();locationSummary();
  }
  async function loadCourses(selected = []) {
    $('#courses-list').innerHTML = '<p class="loading-text">正在获取你的课程…</p>';formReady();
    const result = await api(`/api/school-accounts/${account.id}/courses`);
    const courses = result.courses || result.course_ids.map(id => ({id,name:`课程 ${id}`}));
    $('#courses-list').innerHTML = courses.length ? courses.map(course => `<label class="course-choice"><input type="checkbox" value="${escapeHTML(course.id)}"${selected.includes(course.id) ? ' checked' : ''}><div><strong>${escapeHTML(course.name)}</strong><small>课程 ${escapeHTML(course.id)}</small></div></label>`).join('') : '<p class="empty-text">当前账号暂未找到课程。请确认已加入课程后刷新。</p>';
    formReady();
  }
  function schedulePayload(schedule) {return {school_account_id:schedule.school_account_id,course_ids:schedule.course_ids,local_time:schedule.local_time,timezone:schedule.timezone,weekdays:schedule.weekdays,latitude:schedule.latitude,longitude:schedule.longitude,location_name:schedule.location_name,retry_window_minutes:schedule.retry_window_minutes,enabled:schedule.enabled};}
  function renderSchedules() {
    $('#schedules-list').innerHTML = schedules.length ? schedules.map(schedule => `<article class="schedule-item${schedule.enabled ? '' : ' paused'}"><div class="schedule-time">${escapeHTML(schedule.local_time.slice(0,5))}<small>${escapeHTML(daysText(schedule.weekdays))}</small></div><div class="schedule-details"><strong>${icons["⌖"]} ${escapeHTML(schedule.location_name)}</strong><p>${escapeHTML(schedule.course_ids.map(id => `课程 ${id}`).join(' · '))}</p><p>${schedule.enabled ? `下次执行 ${escapeHTML(formatDate(schedule.next_run_at,schedule.timezone))}` : '任务已暂停，启用后恢复执行'}</p></div><div class="schedule-actions"><span class="badge${schedule.enabled ? '' : ' invalid'}">${schedule.enabled ? '已启用' : '已暂停'}</span><button type="button" data-schedule-action="edit" data-id="${schedule.id}">修改</button><button type="button" data-schedule-action="toggle" data-id="${schedule.id}">${schedule.enabled ? '暂停' : '启用'}</button>${schedule.enabled ? `<button type="button" data-schedule-action="run" data-id="${schedule.id}">立即执行</button>` : ''}</div></article>`).join('') : '<p class="empty-text">还没有签到任务。设置时间和位置后，保存你的第一条计划。</p>';
  }
  async function refreshSchedules() {schedules = await api('/api/schedules');renderSchedules();}
  async function refreshJobs() {
    const {jobs} = await api('/api/jobs?limit=20');
    const statusLabels = {pending:'等待执行',running:'执行中',succeeded:'已完成',failed:'未完成',cancelled:'已取消'};
    const notifyLabels = {pending:'通知待发送',sending:'通知发送中',sent:'微信已发送',failed:'通知未发送'};
    $('#jobs-list').innerHTML = jobs.length ? jobs.map(job => `<div class="history-row"><time datetime="${escapeHTML(job.scheduled_at)}">${escapeHTML(formatDate(job.scheduled_at,schedules.find(schedule => schedule.id === job.schedule_id)?.timezone))}</time><span class="result-badge${job.status === 'succeeded' ? ' success' : job.status === 'failed' ? ' failure' : ''}">${statusLabels[job.status] || escapeHTML(job.status)}</span><span class="history-message">${escapeHTML((job.message || '等待服务器执行').replace(/Cookie/gi,'登录凭证'))}</span><span class="history-notification" title="${escapeHTML(job.notification_error || '')}">${notifyLabels[job.notification_status] || '—'}</span></div>`).join('') : '<p class="empty-text">暂无执行记录。任务执行后，结果会显示在这里。</p>';
  }
  function setAccountView() {
    $('#header-account').textContent = account.label;$('#connected-account').textContent = account.student_no.startsWith('uid:') ? account.label : `${account.label} · ${account.student_no}`;
    const active = account.credential_status === 'active';$('#account-status').textContent = active ? '✓ 学校账号已连接' : '登录失效，请重新登录';$('#account-status').classList.toggle('invalid',!active);
    $('.connection-card h2').textContent = active ? '学校账号已连接' : '学校登录已失效';formReady();
  }
  function resetEditor() {
    editing = null;$('#editor-title').textContent = '新建签到任务';$('#cancel-edit').hidden = true;
    $('#schedule-form').reset();$('#local-time').value = '20:30';$('#retry-window').value = '10';selectedPlace = null;$('#building-search').value = '';
    $$('#courses-list input').forEach(input => {input.checked = false;});$('.time-description span').textContent = '北京时间 · Asia/Shanghai';selectPlaceKind('dorm');
  }
  async function editSchedule(schedule) {
    editing = schedule;account = accounts.find(value => value.id === schedule.school_account_id) || account;$('#account-picker').value = account.id;setAccountView();
    await loadCourses(schedule.course_ids);$('#editor-title').textContent = '修改签到任务';$('#cancel-edit').hidden = false;
    $('#local-time').value = schedule.local_time.slice(0,5);$('#retry-window').value = String(schedule.retry_window_minutes);
    $$('input[name=weekday]').forEach(input => {input.checked = schedule.weekdays.includes(Number(input.value));});
    selectedPlace = places.find(place => place.name === schedule.location_name && Math.abs(place.latitude - schedule.latitude) < 0.000001 && Math.abs(place.longitude - schedule.longitude) < 0.000001) || null;
    if (selectedPlace) {$('#building-search').value = '';selectPlaceKind(selectedPlace.kind);} else {$('#custom-name').value = schedule.location_name;$('#custom-latitude').value = schedule.latitude;$('#custom-longitude').value = schedule.longitude;selectPlaceKind('custom');}
    $('.time-description span').textContent = schedule.timezone === 'Asia/Shanghai' ? '北京时间 · Asia/Shanghai' : `规则时区 · ${schedule.timezone}`;
    formReady();$('.task-editor').scrollIntoView({behavior:'smooth',block:'start'});
  }
  async function bindingStatus() {
    const binding = await api('/api/weixin/binding');const element = $('#weixin-binding-status');
    const connected = binding.status === 'active' && binding.has_context;element.classList.toggle('connected',connected);
    element.textContent = connected ? '✓ 已连接，可以接收微信通知' : binding.status === 'active' ? '已绑定，请向微信 Bot 发送一条消息以建立会话。' : binding.status === 'expired' ? '微信会话已失效，请重新连接通知。' : '尚未连接微信通知';
    $('#bind-weixin').textContent = binding.status === 'unbound' ? '连接微信通知 ↗' : '重新连接通知 ↻';$('#unbind-weixin').hidden = binding.status === 'unbound';
  }
  function stopNotify() {clearTimeout(notifyTimer);notifyController?.abort();}
  async function notifyPoll(current, verifyCode) {
    if (current !== notifyGeneration) return;
    if (Date.now() >= notifyExpiresAt) {$('#notify-qr-status').textContent = '二维码已过期，请重新连接';return;}
    notifyController = new AbortController();
    try {
      const result = await api(`/api/weixin/login/${notifyLoginId}/poll`,{method:'POST',body:verifyCode ? {verify_code:verifyCode} : {},signal:notifyController.signal});
      if (current !== notifyGeneration) return;
      if (result.status === 'confirmed') {stopNotify();$('#notify-login').hidden = true;await bindingStatus();toast('微信已绑定，请向 Bot 发送一条消息。');return;}
      if (result.status === 'need_verifycode') {$('#notify-verify').hidden = false;$('#notify-qr-status').textContent = '请输入手机上显示的验证码';$('#notify-verify-code').focus();return;}
      if (['expired','verify_code_blocked','binded_redirect'].includes(result.status)) {$('#notify-qr-status').textContent = result.status === 'binded_redirect' ? '微信已存在绑定，请查看连接状态或重新扫码' : '二维码已失效，请重新连接';await bindingStatus();return;}
      $('#notify-qr-status').textContent = result.status === 'scaned' ? '已扫码，请在手机上确认' : '请使用微信扫描二维码';notifyTimer = setTimeout(() => notifyPoll(current),2000);
    } catch (error) {if (current !== notifyGeneration || error.name === 'AbortError') return;$('#notify-qr-status').textContent = '连接暂时中断，正在重试…';notifyTimer = setTimeout(() => notifyPoll(current,verifyCode),5000);}
  }
  $('#logout').addEventListener('click',async () => {try {await api('/api/auth/logout',{method:'POST',body:{}});location.replace('/login');} catch (error) {toast(error.message,true);}});
  $$('[data-location-kind]').forEach(button => button.addEventListener('click',() => selectPlaceKind(button.dataset.locationKind)));
  $('#building-search').addEventListener('input',renderPlaces);
  $('#building-list').addEventListener('click',event => {const button = event.target.closest('[data-place]');if (button) {selectedPlace = places.find(place => place.id === button.dataset.place);renderPlaces();locationSummary();}});
  ['#custom-name','#custom-latitude','#custom-longitude'].forEach(selector => $(selector).addEventListener('input',locationSummary));
  $('#courses-list').addEventListener('change',formReady);$('.weekdays').addEventListener('change',formReady);
  $('#select-all-days').addEventListener('click',() => {$$('input[name=weekday]').forEach(input => {input.checked = true;});formReady();});
  $('#reload-courses').addEventListener('click',async () => {try {await busy($('#reload-courses'),'刷新中…',() => loadCourses($$('#courses-list input:checked').map(input => input.value)));} catch (error) {$('#courses-list').innerHTML = `<p class="empty-text">${escapeHTML(error.message)}</p>`;toast(error.message,true);}});
  $('#account-picker').addEventListener('change',async () => {account = accounts.find(value => value.id === $('#account-picker').value);resetEditor();setAccountView();try {await loadCourses();} catch (error) {toast(error.message,true);}});
  $('#cancel-edit').addEventListener('click',resetEditor);
  $('#schedule-form').addEventListener('submit',async event => {
    event.preventDefault();showError('#page-error','');
    try {
      const location = placeKind === 'custom' ? {name:$('#custom-name').value.trim(),latitude:Number($('#custom-latitude').value),longitude:Number($('#custom-longitude').value)} : selectedPlace;
      if (placeKind === 'custom' && ($('#custom-latitude').value === '' || $('#custom-longitude').value === '')) throw new Error('请填写指定位置的纬度和经度。');
      const courseIds = $$('#courses-list input:checked').map(input => input.value), weekdays = $$('input[name=weekday]:checked').map(input => Number(input.value));
      if (!location?.name || !courseIds.length || !weekdays.length || courseIds.length > 20 || !Number.isFinite(location.latitude) || !Number.isFinite(location.longitude)) throw new Error('请选择有效课程、星期和签到位置（最多 20 个课程）。');
      const payload = {school_account_id:account.id,course_ids:courseIds,local_time:$('#local-time').value,timezone:editing?.timezone || 'Asia/Shanghai',weekdays,latitude:location.latitude,longitude:location.longitude,location_name:location.name,retry_window_minutes:Number($('#retry-window').value),enabled:true};
      await busy($('#save-schedule'),'正在保存计划…',async () => {await api(editing ? `/api/schedules/${editing.id}` : '/api/schedules',{method:editing ? 'PUT' : 'POST',body:payload});await refreshSchedules();});
      resetEditor();toast('签到任务已保存，计划已启用。');$('.saved-tasks').scrollIntoView({behavior:'smooth',block:'start'});
    } catch (error) {showError('#page-error',error.message);toast(error.message,true);} finally {formReady();}
  });
  $('#schedules-list').addEventListener('click',async event => {
    const button = event.target.closest('[data-schedule-action]');if (!button) return;
    const schedule = schedules.find(value => value.id === button.dataset.id);if (!schedule) return;
    try {
      await busy(button,'处理中…',async () => {
        if (button.dataset.scheduleAction === 'edit') await editSchedule(schedule);
        else if (button.dataset.scheduleAction === 'toggle') {await api(`/api/schedules/${schedule.id}`,{method:'PUT',body:{...schedulePayload(schedule),enabled:!schedule.enabled}});await refreshSchedules();toast(schedule.enabled ? '签到任务已暂停。' : '签到任务已启用。');}
        else {await api(`/api/schedules/${schedule.id}/run`,{method:'POST',body:{}});await refreshJobs();toast('已提交执行任务，可在下方查看结果。');}
      });
    } catch (error) {toast(error.message,true);}
  });
  $('#reload-tasks').addEventListener('click',async () => {try {await busy($('#reload-tasks'),'刷新中…',async () => {await refreshSchedules();await refreshJobs();});} catch (error) {toast(error.message,true);}});
  $('#bind-weixin').addEventListener('click',async () => {
    stopNotify();const current = ++notifyGeneration;$('#notify-verify').hidden = true;
    try {await busy($('#bind-weixin'),'正在获取二维码…',async () => {const result = await api('/api/weixin/login',{method:'POST',body:{}});notifyLoginId = result.session_id;notifyExpiresAt = Date.parse(result.expires_at);$('#notify-login').hidden = false;await drawQR($('#notify-qr'),result.qrcode_img_content);$('#notify-qr-status').textContent = '请使用微信扫描二维码';notifyTimer = setTimeout(() => notifyPoll(current),1500);});}
    catch (error) {toast(error.message,true);}
  });
  $('#submit-notify-verify').addEventListener('click',async () => {const code = $('#notify-verify-code').value.trim();if (!code) {toast('请输入手机上显示的验证码。',true);return;}try {await busy($('#submit-notify-verify'),'确认中…',() => notifyPoll(notifyGeneration,code));} catch (error) {toast(error.message,true);}});
  $('#unbind-weixin').addEventListener('click',async () => {try {stopNotify();++notifyGeneration;await api('/api/weixin/binding',{method:'DELETE'});$('#notify-login').hidden = true;await bindingStatus();toast('已解除微信通知绑定。');} catch (error) {toast(error.message,true);}});
  try {
    await api('/api/me');[accounts,places] = await Promise.all([api('/api/school-accounts'),api('/api/locations')]);
    if (!accounts.length) {location.replace('/login?reconnect=1');return;}
    account = accounts[0];$('#account-picker').innerHTML = accounts.map(value => `<option value="${value.id}">${escapeHTML(value.label)}</option>`).join('');$('#account-picker-field').hidden = accounts.length < 2;setAccountView();renderPlaces();locationSummary();
    await Promise.all([loadCourses().catch(error => {$('#courses-list').innerHTML = `<p class="empty-text">${escapeHTML(error.message)}</p>`;}),refreshSchedules(),refreshJobs(),bindingStatus()]);
  } catch (error) {if (error.status !== 401) showError('#page-error',error.message);}
  const timer = setInterval(() => {if (!document.hidden) Promise.all([refreshJobs(),bindingStatus()]).catch(() => {});},15000);
  window.addEventListener('beforeunload',() => {clearInterval(timer);stopNotify();});
}
if (page === 'login') loginPage().catch(error => showError('#login-error',error.message));
if (page === 'tasks') tasksPage().catch(error => showError('#page-error',error.message));
