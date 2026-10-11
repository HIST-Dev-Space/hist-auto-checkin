// Browser verification only. All APIs are intercepted; no real login or check-in is sent.
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const base = process.env.WEB_TEST_URL || 'http://127.0.0.1:38100';
const output = process.env.WEB_TEST_OUTPUT || '/tmp/hist-web-qa';
fs.mkdirSync(output, {recursive:true});
const accountId = '00000000-0000-4000-8000-000000000001';
const scheduleId = '00000000-0000-4000-8000-000000000002';
const guestId = '00000000-0000-4000-8000-000000000003';
const locations = JSON.parse(fs.readFileSync(path.join(__dirname,'../web/assets/locations.json'),'utf8'));

async function chooseLocation(page, name) {
  await page.getByRole('combobox',{name:/签到地点/}).click();
  await page.getByRole('option',{name,exact:true}).click();
}

async function appearance(page) {
  await page.mouse.move(0,0);
  if (await page.locator('.theme-toggle').count()) {
    await page.locator('.theme-toggle').evaluate(async element => {
      await Promise.all(element.getAnimations({subtree:true}).map(animation => animation.finished.catch(() => {})));
    });
  }
  return page.evaluate(() => {
    const body = getComputedStyle(document.body);
    const switchElement = document.querySelector('.theme-toggle');
    const toggle = switchElement ? getComputedStyle(switchElement) : null;
    const tokens = ['bg','surface','soft','text','muted','line','blue','blue-dark','tint','green','green-bg','red','red-bg','radius','link-hover','secondary-hover','switch-track','switch-border','switch-hover-border','switch-thumb','switch-icon','feature-bg','feature-hover-border','feature-title','feature-description','footer-line'];
    return {
      theme: document.documentElement.dataset.theme,
      palette: Object.fromEntries(tokens.map(name => [name, body.getPropertyValue(`--${name}`).trim()])),
      background: body.backgroundColor, color: body.color, font: body.fontFamily,
      themeColor: document.querySelector('meta[name=theme-color]').content,
      switch: toggle ? {role:switchElement.getAttribute('role'),checked:switchElement.getAttribute('aria-checked'),width:toggle.width,height:toggle.height,border:toggle.border,borderRadius:toggle.borderRadius,background:toggle.backgroundColor} : null,
    };
  });
}

async function assertLoginAppearance(page, reference) {
  const actual = await appearance(page);
  assert.equal(actual.switch,null,'Login must not display a theme switch');
  assert.deepEqual(actual,{...reference,switch:null},'Login must still follow the homepage theme');
}

async function chromeAppearance(page) {
  return page.evaluate(() => {
    const header = document.querySelector('.site-header').cloneNode(true);
    header.querySelectorAll('[aria-current]').forEach(element => element.removeAttribute('aria-current'));
    const selectors = ['.site-header','.header-inner','.brand','.brand-mark','.header-actions','.site-footer'];
    const properties = ['height','fontSize','fontWeight','color','backgroundColor','borderTop','borderBottom','paddingTop','paddingBottom','paddingLeft','paddingRight','gap'];
    return {
      header:header.outerHTML,footer:document.querySelector('.site-footer').outerHTML,
      styles:Object.fromEntries(selectors.map(selector => {const style=getComputedStyle(document.querySelector(selector));return [selector,Object.fromEntries(properties.map(property => [property,style[property]]))];})),
    };
  });
}

async function scenario(browser, viewport, qrLogin) {
  const options = {viewport,colorScheme:'light',locale:'zh-CN'};
  if (process.env.WEB_TEST_VIDEO === '1') options.recordVideo = {dir:path.join(output,'videos'),size:viewport};
  const context = await browser.newContext(options);
  const page = await context.newPage(), started = Date.now(), errors = [], navigations = [];
  let celebrations = 0;
  await page.exposeFunction('recordSetupCelebration',() => {celebrations++;});
  await page.addInitScript(() => {
    const showModal = HTMLDialogElement.prototype.showModal;
    HTMLDialogElement.prototype.showModal = function (...args) {
      if (this.id === 'setup-success') window.recordSetupCelebration();
      return showModal.apply(this,args);
    };
  });
  page.on('response',response => {
    const request=response.request();
    if(request.isNavigationRequest() && request.frame()===page.mainFrame() && response.status()>=200 && response.status()<300)
      navigations.push(new URL(response.url()).pathname);
  });
  page.on('pageerror',error => errors.push(error.message));
  page.on('console',message => {if (message.type() === 'error' && /Content Security Policy|violates|Refused to/.test(message.text())) errors.push(message.text());});
  let authenticated = false, scanReady = false, rules = [], binding = {status:'unbound',has_context:false}, total = 12, today = false;
  let createdPayload, updatedPayload, loginMethod = 'wechat', failScheduleUpdate = false;
  let scheduleReadGate, bindingReadGate;
  let entryGate, schoolLoginStarts = 0;
  const daily = body => ({course_ids:[],timezone:'Asia/Shanghai',weekdays:[1,2,3,4,5,6,7],retry_window_minutes:10,enabled:true,...body});
  const dashboard = () => ({total_successful_checkins:total,today_successful_checkins:today?1:0,today_status:today?'success':rules.some(rule=>rule.enabled)?'waiting':'paused',auto_checkin_enabled:rules.some(rule=>rule.enabled),has_schedule:!!rules.length,has_active_account:true,account_label:'张三'});
  await page.route('**/api/**',async route => {
    const request=route.request(), endpoint=new URL(request.url()).pathname, method=request.method();
    let body;try {body=request.postDataJSON();} catch {}
    if(method!=='GET') assert.equal(request.headers()['x-hist-request'],'1');
    let result,status=200;
    if(endpoint==='/api/me'){status=authenticated?200:401;result=authenticated?{id:accountId,username:'web-qa'}:{error:'需要登录'};}
    else if(endpoint==='/api/school-login/start'){schoolLoginStarts++;loginMethod=body.method;result={method:body.method,expires_at:new Date(Date.now()+300000).toISOString(),qr_content:'weixin://school/browser-check',captcha_data_url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII='};}
    else if(endpoint==='/api/school-login/captcha')result={captcha_data_url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII='};
    else if(endpoint==='/api/school-login/identity'){assert.equal(loginMethod,'identity');authenticated=true;result={status:'authenticated',school_account_id:accountId};}
    else if(endpoint==='/api/school-login/poll'){if(scanReady)authenticated=true;result={status:scanReady?'authenticated':'waiting',school_account_id:accountId};}
    else if(endpoint==='/api/school-accounts')result=[{id:accountId,label:'张三',student_no:'2026001',credential_status:'active'}];
    else if(endpoint==='/api/locations')result=locations;
    else if(endpoint==='/api/schedules'&&method==='POST'){createdPayload=body;status=201;rules=[{id:scheduleId,...daily(body),next_run_at:new Date(Date.now()+86400000).toISOString()}];result={id:scheduleId,next_run_at:rules[0].next_run_at};}
    else if(endpoint==='/api/schedules'){if(scheduleReadGate)await scheduleReadGate;result=rules;}
    else if(endpoint===`/api/schedules/${scheduleId}`){
      if(failScheduleUpdate){status=409;result={error:'该规则正在执行，请稍后修改'};}
      else{updatedPayload=body;rules=[{...rules[0],...daily(body)}];result={id:scheduleId,next_run_at:rules[0].next_run_at};}
    }
    else if(endpoint==='/api/dashboard')result=dashboard();
    else if(endpoint==='/api/auto-checkin'){rules=rules.map(rule=>({...rule,enabled:body.enabled}));result=dashboard();}
    else if(endpoint==='/api/weixin/binding'){if(bindingReadGate)await bindingReadGate;result=binding;}
    else if(endpoint==='/api/weixin/login'){status=201;result={session_id:guestId,qrcode_img_content:'weixin://notifications/browser-check',expires_at:new Date(Date.now()+300000).toISOString()};}
    else if(endpoint===`/api/weixin/login/${guestId}/poll`){if(!body.verify_code)result={status:'need_verifycode'};else{assert.equal(body.verify_code,'1234');binding={status:'active',has_context:false};result={status:'confirmed'};}}
    else if(endpoint==='/api/auth/logout'){authenticated=false;status=204;}
    else throw new Error(`Unexpected browser API: ${method} ${endpoint}`);
    await route.fulfill({status,contentType:'application/json',body:status===204?'':JSON.stringify(result)});
  });
  // Document redirects follow the mocked API session; Rust integration tests verify real sessions.
  await page.route(url=>url.pathname==='/start',async route => {
    if(entryGate)await entryGate;
    await route.fulfill({status:303,headers:{location:authenticated ? rules.length ? '/account' : '/tasks' : '/login','cache-control':'no-store'}});
  });
  await page.route(url=>url.pathname==='/login' && !url.search,async route => {
    if(authenticated)await route.fulfill({status:303,headers:{location:rules.length ? '/account' : '/tasks','cache-control':'no-store'}});
    else await route.continue();
  });
  const size=viewport.width<600?'mobile':'desktop';
  await page.goto(base,{waitUntil:'networkidle'});
  const light=await appearance(page),lightChrome=await chromeAppearance(page);
  await page.locator('.theme-toggle').click();const dark=await appearance(page),darkChrome=await chromeAppearance(page);
  await page.locator('.theme-toggle').click();
  await page.screenshot({path:path.join(output,`home-${size}.png`),fullPage:true});
  await page.getByRole('link',{name:'社区',exact:true}).click();await page.waitForURL('**/community');
  await page.waitForFunction(()=>{const image=document.querySelector('.community-poster img');return image?.complete&&image.naturalWidth>0;});
  assert.deepEqual(await appearance(page),light);assert.deepEqual(await chromeAppearance(page),lightChrome);
  await page.locator('.brand').click();await page.waitForURL(`${base}/`);
  assert.equal(await page.locator('.hero-actions a.primary').getAttribute('href'),'/start');
  await page.locator('.hero-actions a.primary').click();await page.waitForURL('**/login');await page.locator('#school-qr img').waitFor();
  await assertLoginAppearance(page,light);assert.equal(await page.locator('#school-qr-status').innerText(),'等待扫码');
  await page.locator('#refresh-school-qr').click();await page.locator('#school-qr img').waitFor();
  if(qrLogin){scanReady=true;}else{
    await page.locator('[data-login-method=identity]').click();await page.locator('#captcha-image').waitFor({state:'visible'});
    await page.locator('#login-name').fill('张三');await page.locator('#student-no').fill('2026001');await page.locator('#captcha-code').fill('1234');await page.locator('#submit-school-login').click();
  }
  await page.waitForURL('**/tasks');await page.locator('#checkin-location option[value=dorm-1]').waitFor({state:'attached'});
  assert.deepEqual(await appearance(page),light);assert.deepEqual(await chromeAppearance(page),lightChrome);
  assert.equal(await page.locator('#schedule-form input,#schedule-form select').count(),2,'Task setup must only configure time and location');
  assert.equal(await page.locator('#courses-list,.weekdays,.advanced-settings,.form-hint').count(),0,'Setup must not contain extra controls or small explanatory copy');
  assert.equal(await page.locator('#local-time').inputValue(),'21:10','New schedules must default to 21:10');
  assert.equal(await page.locator('#save-schedule').innerText(),'下一步');
  assert.equal(await page.locator('#schedule-back').count(),0);
  await page.locator('#local-time').fill('21:15');
  const locationPicker = page.getByRole('combobox',{name:/签到地点/});
  await locationPicker.click();
  assert.deepEqual(await page.getByRole('option').allTextContents(),locations.filter(place=>place.kind==='dorm').map(place=>place.name),'Only dormitory locations may appear');
  await page.keyboard.press('Escape');assert.equal(await locationPicker.getAttribute('aria-expanded'),'false');
  await chooseLocation(page,'1号宿舍楼');
  await page.screenshot({path:path.join(output,`tasks-${size}.png`),fullPage:true});
  await page.locator('#save-schedule').click();await page.waitForURL('**/notifications?setup=1');await page.locator('#notify-qr img').waitFor();
  assert.equal(createdPayload.local_time,'21:15');assert.equal(createdPayload.location_name,'1号宿舍楼');assert.equal(createdPayload.latitude,35.281256);
  assert.equal(createdPayload.course_ids,undefined);assert.equal(createdPayload.weekdays,undefined);assert.deepEqual(rules[0].weekdays,[1,2,3,4,5,6,7]);
  assert.deepEqual(await appearance(page),light);assert.deepEqual(await chromeAppearance(page),lightChrome);
  await page.locator('#notification-back').click();await page.waitForURL('**/tasks?setup=1&edit=*');
  await page.locator('.select-trigger:not(:disabled)').waitFor();
  assert.equal(await page.locator('#local-time').inputValue(),'21:15');
  assert.equal(await page.locator('#checkin-location').inputValue(),'dorm-1');
  assert.equal(await page.locator('#save-schedule').innerText(),'下一步','Returning during setup must continue the setup flow');
  await page.locator('#local-time').fill('21:20');
  await page.locator('#save-schedule').click();await page.waitForURL('**/notifications?setup=1');await page.locator('#notify-qr img').waitFor();
  assert.equal(updatedPayload.local_time,'21:20');
  await page.screenshot({path:path.join(output,`notifications-${size}.png`),fullPage:true});
  await page.locator('#notify-verify').waitFor({state:'visible'});await page.locator('#notify-verify-code').fill('1234');await page.locator('#submit-notify-verify').click();
  await page.getByText('请向微信机器人发送一条消息',{exact:true}).waitFor();
  assert.equal(await page.locator('#setup-success').isVisible(),false,'Success must wait until notifications can actually be delivered');
  binding.has_context=true;await page.locator('#finish-notification').click();await page.locator('#setup-success[open]').waitFor();
  const animationAt=(Date.now()-started)/1000;
  await page.waitForTimeout(350);await page.screenshot({path:path.join(output,`success-${size}.png`),fullPage:true});
  await page.waitForURL('**/account');await page.waitForFunction(()=>document.querySelector('#total-checkins').textContent==='12');
  assert.equal(celebrations,1,'The first setup must celebrate exactly once');
  assert.deepEqual(await appearance(page),light);assert.deepEqual(await chromeAppearance(page),lightChrome);
  assert.equal(await page.locator('#today-status').innerText(),'等待签到');assert.equal(await page.locator('#auto-checkin-toggle').getAttribute('aria-checked'),'true');
  await page.screenshot({path:path.join(output,`account-${size}.png`),fullPage:true});
  await page.locator('.brand').click();await page.waitForURL(`${base}/`);
  const returningStarts=schoolLoginStarts, returningNavigation=navigations.length;
  let releaseEntry;
  entryGate=new Promise(resolve=>{releaseEntry=resolve;});
  const entryRequest=page.waitForRequest('**/start');
  await page.locator('.hero-actions a.primary').click({noWaitAfter:true});
  await entryRequest;
  assert.equal(new URL(page.url()).pathname,'/','The homepage must stay visible while the server checks the session');
  assert.deepEqual(navigations.slice(returningNavigation),[],'No page may render before the session check completes');
  releaseEntry();entryGate=undefined;
  await page.waitForURL('**/account');await page.locator('#schedule-time').waitFor({state:'visible'});
  assert.deepEqual(navigations.slice(returningNavigation),['/account'],'Quick start must never render the login page for a returning user');
  assert.equal(schoolLoginStarts,returningStarts,'Quick start must not create another school-login session');
  await page.locator('#auto-checkin-toggle').click();await page.waitForFunction(()=>document.querySelector('#auto-checkin-toggle').getAttribute('aria-checked')==='false');
  assert.equal(rules[0].enabled,false);assert.equal(await page.locator('#today-status').innerText(),'已关闭');
  await page.reload();await page.waitForFunction(()=>document.querySelector('#total-checkins').textContent==='12');
  assert.equal(await page.locator('#auto-checkin-toggle').getAttribute('aria-checked'),'false','Off state must persist after refresh');
  await page.locator('#auto-checkin-toggle').click();await page.waitForFunction(()=>document.querySelector('#auto-checkin-toggle').getAttribute('aria-checked')==='true');
  today=true;total=13;await page.reload();await page.getByText('已签到',{exact:true}).waitFor();
  await page.screenshot({path:path.join(output,`account-success-${size}.png`),fullPage:true});
  assert.equal(await page.locator('#total-checkins').innerText(),'13');
  await page.locator('.theme-toggle').click();assert.deepEqual(await appearance(page),dark);assert.deepEqual(await chromeAppearance(page),darkChrome);
  await page.screenshot({path:path.join(output,`account-dark-${size}.png`),fullPage:true});
  await page.locator('#auto-checkin-toggle').click();await page.waitForFunction(()=>document.querySelector('#auto-checkin-toggle').getAttribute('aria-checked')==='false');
  rules[0]={...rules[0],course_ids:['12'],weekdays:[1,3,5],location_name:'原签到地点',latitude:35.2805,longitude:113.9365};
  let releaseScheduleRead;
  scheduleReadGate=new Promise(resolve=>{releaseScheduleRead=resolve;});
  await page.locator('#edit-schedule').click();await page.waitForURL('**/tasks?edit=*');
  assert.equal(await page.locator('#save-schedule').innerText(),'确认','The editor must use its final label while schedule data is loading');
  assert.equal(await page.locator('#save-schedule').isDisabled(),true);
  assert.equal(await page.locator('#local-time').inputValue(),'','The editor must not flash the new-schedule default time');
  assert.equal(await page.locator('#local-time').isDisabled(),true);
  assert.equal(await page.locator('#checkin-location').isDisabled(),true);
  releaseScheduleRead();scheduleReadGate=undefined;
  await page.locator('.select-trigger:not(:disabled)').waitFor();
  assert.equal(await page.locator('#checkin-location').inputValue(),'');
  assert.equal(await page.locator('#save-schedule').isDisabled(),true,'An old non-dormitory location must be replaced before saving');
  assert.equal(await page.locator('#schedule-form input,#schedule-form select').count(),2);
  assert.equal(await page.locator('#save-schedule').innerText(),'确认');
  assert.equal(await page.locator('#schedule-back').count(),0);
  await chooseLocation(page,'2号宿舍楼');
  await page.screenshot({path:path.join(output,`tasks-dark-${size}.png`),fullPage:true});
  const editNavigation = navigations.length;
  failScheduleUpdate=true;
  await page.locator('#save-schedule').click();await page.locator('#page-error').waitFor({state:'visible'});
  assert.equal(await page.locator('#page-error').innerText(),'该规则正在执行，请稍后修改');
  assert.equal(new URL(page.url()).pathname,'/tasks','A failed save must remain in the editor');
  assert.equal(await page.locator('#save-schedule').innerText(),'确认');
  failScheduleUpdate=false;
  await page.locator('#save-schedule').click();await page.waitForURL('**/account');
  assert.deepEqual(navigations.slice(editNavigation),['/account'],'Editing must return directly to the account page');
  assert.equal(await page.locator('#toast').innerText(),'签到设置已保存');
  assert.equal(await page.locator('#schedule-location').innerText(),'2号宿舍楼');
  assert.equal(celebrations,1,'Editing must not repeat the setup animation');
  assert.equal(updatedPayload.location_name,'2号宿舍楼');assert.equal(updatedPayload.latitude,35.281414);assert.equal(updatedPayload.longitude,113.936898);
  assert.deepEqual(rules[0].course_ids,[]);assert.deepEqual(rules[0].weekdays,[1,2,3,4,5,6,7]);
  assert.equal(updatedPayload.enabled,false,'Editing must preserve a paused schedule');
  assert.equal(await page.locator('#auto-checkin-toggle').getAttribute('aria-checked'),'false');
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'User page must fit the viewport');
  await page.reload();await page.locator('#schedule-location').waitFor({state:'visible'});
  assert.equal(await page.locator('#toast').isVisible(),false,'A save notice must not repeat on reload');
  let releaseBindingRead;
  bindingReadGate=new Promise(resolve=>{releaseBindingRead=resolve;});
  await page.locator('#notification-settings').click();await page.waitForURL('**/notifications?manage=1');
  assert.equal(await page.locator('#notify-qr-status').innerText(),'正在检查');
  assert.equal(await page.locator('#bind-weixin').isVisible(),false,'QR actions must remain hidden until the initial binding check finishes');
  assert.equal(await page.locator('#finish-notification').isVisible(),false);
  assert.equal(await page.locator('a[href="/account"]').count(),0,'Management pages must not add a return-to-account link');
  assert.equal(await page.locator('#notification-back').isVisible(),false);
  releaseBindingRead();bindingReadGate=undefined;
  await page.getByText('微信通知已连接',{exact:true}).waitFor();
  assert.equal(await page.locator('#setup-success').isVisible(),false);
  assert.equal(await page.locator('#bind-weixin').isVisible(),true);
  assert.equal(await page.locator('#finish-notification').innerText(),'确认');
  await page.locator('#finish-notification').click();await page.waitForURL('**/account');
  assert.equal(celebrations,1,'Completing notification management must not celebrate setup');
  binding={status:'unbound',has_context:false};
  await page.locator('#edit-schedule').click();await page.waitForURL('**/tasks?edit=*');
  await page.locator('.select-trigger:not(:disabled)').waitFor();await page.locator('#local-time').fill('21:30');
  const disconnectedEditNavigation=navigations.length;
  await page.locator('#save-schedule').click();await page.waitForURL('**/account');
  assert.deepEqual(navigations.slice(disconnectedEditNavigation),['/account'],'Editing must not require Weixin notifications');
  assert.equal(await page.locator('#schedule-time').innerText(),'21:30');
  assert.equal(await page.locator('#notification-status').innerText(),'未连接');
  const loginNavigation=navigations.length;
  await page.goto(`${base}/login`);await page.waitForURL('**/account');
  assert.deepEqual(navigations.slice(loginNavigation),['/account'],'A returning login visit must redirect before rendering the login page');
  await page.goto(`${base}/login?reconnect=1`);await page.locator('#school-qr img').waitFor();
  await page.locator('[data-login-method=identity]').click();await page.locator('#captcha-image').waitFor({state:'visible'});
  await page.locator('#login-name').fill('张三');await page.locator('#student-no').fill('2026001');await page.locator('#captcha-code').fill('1234');
  const reconnectNavigation=navigations.length;
  await page.locator('#submit-school-login').click();await page.waitForURL('**/account');
  assert.deepEqual(navigations.slice(reconnectNavigation),['/account'],'Reconnecting must return to the existing account');
  await page.locator('#schedule-time').waitFor({state:'visible'});
  assert.equal(await page.locator('#schedule-time').innerText(),'21:30');
  binding={status:'active',has_context:false};
  await page.goto(`${base}/login`);await page.waitForURL('**/account');
  await page.getByText('待完成连接',{exact:true}).waitFor();
  assert.equal(await page.locator('#notification-status').innerText(),'待完成连接');
  await page.goto(`${base}/tasks?edit=missing`);await page.locator('#page-error').waitFor({state:'visible'});
  assert.match(await page.locator('#page-error').innerText(),/不存在或已删除/);
  assert.equal(await page.locator('#save-schedule').isDisabled(),true,'An invalid edit ID must not select another schedule');
  assert.equal(await page.locator('#schedule-back').count(),0);
  await page.goto(`${base}/account`);
  await page.goto(`${base}/tasks`);await page.locator('.select-trigger:not(:disabled)').waitFor();
  assert.equal(await page.locator('#save-schedule').innerText(),'确认','An existing schedule opened without an edit ID is still an edit');
  await page.goto(`${base}/account`);
  rules=[];binding={status:'unbound',has_context:false};
  await page.reload();await page.locator('#configure-task').waitFor({state:'visible'});
  await page.locator('.brand').click();await page.waitForURL(`${base}/`);
  const unconfiguredStarts=schoolLoginStarts, unconfiguredNavigation=navigations.length;
  await page.locator('.hero-actions a.primary').click();await page.waitForURL('**/tasks');
  await page.locator('.select-trigger:not(:disabled)').waitFor();
  assert.deepEqual(navigations.slice(unconfiguredNavigation),['/tasks'],'An authenticated user without a schedule must enter setup directly');
  assert.equal(schoolLoginStarts,unconfiguredStarts);
  await page.goto(`${base}/account`);await page.locator('#configure-task').waitFor({state:'visible'});
  await page.locator('#notification-settings').click();await page.waitForURL('**/notifications?manage=1');await page.locator('#notify-qr img').waitFor();
  await page.locator('#notify-verify').waitFor({state:'visible'});await page.locator('#notify-verify-code').fill('1234');await page.locator('#submit-notify-verify').click();
  await page.getByText('请向微信机器人发送一条消息',{exact:true}).waitFor();
  binding.has_context=true;await page.getByText('微信通知已连接',{exact:true}).waitFor();
  assert.equal(new URL(page.url()).pathname,'/notifications','Notification management must remain open after connecting');
  assert.equal(await page.locator('#setup-success').isVisible(),false);
  await page.locator('#finish-notification').click();await page.waitForURL('**/account');
  assert.equal(celebrations,1,'Rebinding without a schedule must not run initial setup');
  await page.goto(`${base}/notifications`);await page.getByText('微信通知已连接',{exact:true}).waitFor();
  assert.equal(await page.locator('#setup-success').isVisible(),false,'Direct notification visits must use management mode');
  assert.equal(await page.locator('#notification-back').isVisible(),false);
  await page.locator('#finish-notification').click();await page.waitForURL('**/account');
  await page.goto(`${base}/notifications?setup=1`);await page.waitForURL('**/tasks?setup=1');
  await page.locator('.select-trigger:not(:disabled)').waitFor();
  assert.equal(await page.locator('#save-schedule').innerText(),'下一步');
  await chooseLocation(page,'1号宿舍楼');await page.locator('#save-schedule').click();
  await page.waitForURL('**/notifications?setup=1');await page.locator('#setup-success[open]').waitFor();
  await page.locator('#enter-account').click();await page.waitForURL('**/account');
  assert.equal(celebrations,2,'Explicit new setup must still celebrate when notifications are already connected');
  await page.locator('#logout').click();await page.waitForURL('**/login');await page.locator('#school-qr img').waitFor();
  await assertLoginAppearance(page,dark);await page.locator('.mini-brand').click();assert.equal(new URL(page.url()).pathname,'/login');
  assert.deepEqual(errors,[]);
  const video=page.video();await context.close();
  if (video) fs.writeFileSync(path.join(output,`animation-${size}.json`),JSON.stringify({video:await video.path(),animationAt}));
  console.log(`${size}: anonymous and returning quick start, delayed entry without login flashes, direct login redirects, setup, editing, notification management and reconnect passed`);
}

async function systemThemeScenario(browser) {
  const context=await browser.newContext({viewport:{width:320,height:568},colorScheme:'dark'});
  const page=await context.newPage();
  await page.goto(base,{waitUntil:'networkidle'});
  assert.equal(await page.locator('html').getAttribute('data-theme'),'dark');
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),'Shared header must fit a narrow mobile viewport');
  await page.emulateMedia({colorScheme:'light'});
  await page.waitForFunction(() => document.documentElement.dataset.theme === 'light');
  await page.locator('.theme-toggle').click();
  await page.emulateMedia({colorScheme:'dark'});await page.emulateMedia({colorScheme:'light'});
  assert.equal(await page.locator('html').getAttribute('data-theme'),'dark','Manual preference must override the system');
  const second=await context.newPage();
  await second.goto(`${base}/community`,{waitUntil:'networkidle'});
  assert.equal(await second.locator('html').getAttribute('data-theme'),'dark');
  await second.locator('.theme-toggle').click();
  await page.waitForFunction(() => document.documentElement.dataset.theme === 'light');
  assert.equal(await page.locator('.theme-toggle').getAttribute('aria-checked'),'false','Theme changes must synchronize between tabs');
  await page.reload();
  assert.equal(await page.locator('html').getAttribute('data-theme'),'light');
  console.log('System theme, saved preference, narrow mobile header and cross-tab synchronization passed');
  await context.close();
}
async function failedExchangeScenario(browser) {
  const context=await browser.newContext({viewport:{width:1000,height:800}});const page=await context.newPage();
  await page.route('**/api/**',async route=>{
    const endpoint=new URL(route.request().url()).pathname;
    const payload=endpoint==='/api/school-login/start'
      ? {method:'wechat',expires_at:new Date(Date.now()+300000).toISOString(),qr_content:'weixin://school/exchange-error-check'}
      : {error:'学校身份已确认，但登录凭证获取失败：签到系统返回 HTTP 403，尚未取得登录凭证',code:'school_session_exchange_failed',scan_confirmed:true};
    await route.fulfill({status:endpoint==='/api/school-login/start'?200:502,contentType:'application/json',body:JSON.stringify(payload)});
  });
  await page.goto(`${base}/login?reconnect=1`);await page.locator('#login-error').waitFor({state:'visible'});
  assert.match(await page.locator('#login-error').innerText(),/HTTP 403/);
  assert.match(await page.locator('#school-qr-status').innerText(),/微信已确认登录/);
  assert.ok(page.url().includes('/login'));
  console.log('Confirmed scan + failed cookie exchange: exact error remains visible, login is not falsely reported successful');
  await context.close();
}
async function moduleLoadingScenario(browser) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.route('**/api/**', async route => {
    const endpoint = new URL(route.request().url()).pathname;
    const results = {
      '/api/me': {id: accountId},
      '/api/school-accounts': [{id: accountId, credential_status: 'active'}],
      '/api/schedules': [],
      '/api/locations': locations,
    };
    assert.ok(endpoint in results, `Unexpected API: ${endpoint}`);
    await route.fulfill({contentType: 'application/json', body: JSON.stringify(results[endpoint])});
  });
  const resources = () => page.evaluate(() => performance.getEntriesByType('resource').map(entry => new URL(entry.name).pathname));
  await page.goto(base, {waitUntil: 'networkidle'});
  assert.equal((await resources()).some(path => path.startsWith('/assets/pages/') || path === '/assets/qrcode.js'), false, 'Homepage must not load page controllers or QR library');
  await page.goto(`${base}/tasks`, {waitUntil: 'networkidle'});
  await page.locator('.select-trigger:not(:disabled)').waitFor();
  const taskResources = await resources();
  assert.deepEqual(taskResources.filter(path => path.startsWith('/assets/pages/')), ['/assets/pages/tasks.js']);
  assert.equal(taskResources.includes('/assets/qrcode.js'), false, 'Task setup must not load the QR library');
  assert.deepEqual(errors, []);
  await context.close();
  console.log('Native modules load only the current page; static pages and task setup do not load QR code');
}

async function loginCacheScenario(browser) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    const now = Date.now.bind(Date);
    window.loginClockOffset = 0;
    Date.now = () => now() + window.loginClockOffset;
  });
  const starts = {wechat:0,identity:0}, expiry = {};
  let captchaReads = 0, identitySubmits = 0, failCaptcha = false;
  const captcha = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII=';
  await page.route('**/api/**', async route => {
    const endpoint = new URL(route.request().url()).pathname;
    const now = await page.evaluate(() => Date.now());
    let result, status = 200;
    if (endpoint === '/api/school-login/start') {
      const method = route.request().postDataJSON().method;
      starts[method]++;
      expiry[method] = now + 300000;
      result = {expires_at:new Date(expiry[method]).toISOString(),reusable_until:new Date(now+60000).toISOString(),qr_content:`weixin://school/cache-${starts.wechat}`,captcha_data_url:captcha};
    } else if (endpoint === '/api/school-login/captcha') {
      captchaReads++;
      if (failCaptcha) {
        failCaptcha = false;
        status = 502;
        result = {error:'验证码获取失败'};
      } else result = {expires_at:new Date(expiry.identity).toISOString(),reusable_until:new Date(now+60000).toISOString(),captcha_data_url:captcha};
    } else if (endpoint === '/api/school-login/poll') result = {status:'waiting'};
    else if (endpoint === '/api/school-login/identity') {
      identitySubmits++;
      if (identitySubmits === 1) {status=400;result={error:'验证码错误，请刷新验证码后重试'};}
      else result = {status:'need_wechat',expires_at:new Date(expiry.identity).toISOString(),qr_content:'weixin://school/identity-converted',message:'该账号已绑定微信，请扫码完成验证'};
    } else throw new Error(`Unexpected login API: ${endpoint}`);
    await route.fulfill({status,contentType:'application/json',body:JSON.stringify(result)});
  });
  const wechatTab = page.locator('[data-login-method=wechat]');
  const identityTab = page.locator('[data-login-method=identity]');
  const readyCaptcha = () => page.locator('#submit-school-login:not(:disabled)').waitFor();
  await page.goto(`${base}/login?reconnect=1`);
  await page.locator('#school-qr img').waitFor();
  const originalQR = await page.locator('#school-qr img').getAttribute('src');
  await identityTab.click();await readyCaptcha();
  await page.locator('#login-name').fill('张三');
  await page.locator('#student-no').fill('2026001');
  await page.locator('#captcha-code').fill('1234');
  for (let i=0;i<3;i++) {
    await wechatTab.click();
    assert.equal(await page.locator('#school-qr img').getAttribute('src'),originalQR);
    await identityTab.click();await readyCaptcha();
    assert.equal(await page.locator('#captcha-code').inputValue(),'1234');
  }
  assert.deepEqual(starts,{wechat:1,identity:1});assert.equal(captchaReads,0);
  await wechatTab.click();await page.locator('#refresh-school-qr').click();
  await page.locator('#school-qr img').waitFor();
  assert.notEqual(await page.locator('#school-qr img').getAttribute('src'),originalQR);
  assert.equal(starts.wechat,2);
  await identityTab.click();await readyCaptcha();
  assert.equal(await page.locator('#captcha-code').inputValue(),'1234');
  await page.locator('#refresh-captcha').click();await readyCaptcha();
  assert.equal(captchaReads,1);assert.equal(await page.locator('#captcha-code').inputValue(),'');
  failCaptcha=true;
  await page.locator('#refresh-captcha').click();
  await page.getByText('获取失败，点击重试',{exact:true}).waitFor();
  assert.equal(await page.locator('#captcha-image').isVisible(),false);
  assert.equal(await page.locator('#submit-school-login').isDisabled(),true);
  await wechatTab.click();await identityTab.click();await readyCaptcha();
  assert.equal(captchaReads,3,'A failed refresh must not restore the old captcha');
  await page.locator('#captcha-code').fill('1234');
  await page.evaluate(() => {window.loginClockOffset=65000;});
  await page.locator('#submit-school-login').click();await readyCaptcha();
  await page.getByText('验证码已过期，请输入新的验证码',{exact:true}).waitFor();
  assert.equal(identitySubmits,0);assert.equal(captchaReads,4);assert.equal(starts.identity,1);
  await page.locator('#captcha-code').fill('1234');
  await page.locator('#submit-school-login').click();await readyCaptcha();
  await page.getByText('验证码错误，请刷新验证码后重试',{exact:true}).waitFor();
  assert.equal(identitySubmits,1);assert.equal(captchaReads,5);
  assert.equal(await page.locator('#captcha-code').inputValue(),'');
  const previousQR = await page.locator('#school-qr img').getAttribute('src');
  await page.locator('#captcha-code').fill('1234');
  await page.locator('#submit-school-login').click();
  await page.getByText('该账号已绑定微信，请扫码完成验证',{exact:true}).waitFor();
  assert.notEqual(await page.locator('#school-qr img').getAttribute('src'),previousQR);
  assert.equal(starts.wechat,2,'Identity conversion must display the returned QR');
  await identityTab.click();await readyCaptcha();
  assert.equal(starts.identity,2,'The converted identity session must be recreated');
  assert.equal(await page.locator('#login-name').inputValue(),'张三');
  assert.equal(await page.locator('#student-no').inputValue(),'2026001');
  await page.evaluate(() => {window.loginClockOffset=400000;});
  await wechatTab.click();await page.locator('#school-qr img').waitFor();
  await identityTab.click();await readyCaptcha();
  assert.deepEqual(starts,{wechat:3,identity:3});
  assert.deepEqual(errors,[]);
  await context.close();
  console.log('Login tabs reuse challenges and inputs; refresh, failure, expiry and identity conversion invalidate the correct cache');
}

async function qrReuseLimitScenario(browser) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror',error => errors.push(error.message));
  await page.addInitScript(() => {
    const now = Date.now();
    window.loginClockOffset = 0;
    Date.now = () => now + window.loginClockOffset;
  });
  let qrStarts = 0;
  await page.route('**/api/**',async route => {
    const endpoint = new URL(route.request().url()).pathname;
    let result;
    if (endpoint === '/api/school-login/start') {
      if (route.request().postDataJSON().method === 'wechat') qrStarts++;
      const now = await page.evaluate(() => Date.now());
      result = {expires_at:new Date(now+300000).toISOString(),qr_content:`weixin://school/reuse-limit-${qrStarts}`,captcha_data_url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII='};
    } else if (endpoint === '/api/school-login/poll') result = {status:'waiting'};
    else throw new Error(`Unexpected login API: ${endpoint}`);
    await route.fulfill({contentType:'application/json',body:JSON.stringify(result)});
  });
  const wechatTab = page.locator('[data-login-method=wechat]');
  const identityTab = page.locator('[data-login-method=identity]');
  await page.goto(`${base}/login?reconnect=1`);
  await page.locator('#school-qr img').waitFor();
  const originalQR = await page.locator('#school-qr img').getAttribute('src');
  await page.evaluate(() => {window.loginClockOffset=179000;});
  await identityTab.click();await page.locator('#submit-school-login:not(:disabled)').waitFor();
  await wechatTab.click();
  assert.equal(qrStarts,1,'Switching within three minutes must reuse the QR');
  assert.equal(await page.locator('#school-qr img').getAttribute('src'),originalQR);
  const continuingPoll = page.waitForRequest('**/api/school-login/poll');
  await page.evaluate(() => {window.loginClockOffset=181000;});
  await continuingPoll;
  assert.equal(qrStarts,1,'Reaching the reuse limit must not end the active five-minute session');
  assert.equal(await page.locator('#school-qr img').getAttribute('src'),originalQR);
  await identityTab.click();await wechatTab.click();
  await page.locator('#school-qr img').waitFor();
  assert.equal(qrStarts,2,'Returning after three minutes must fetch a new QR even while the old session is valid');
  assert.notEqual(await page.locator('#school-qr img').getAttribute('src'),originalQR);
  assert.deepEqual(errors,[]);
  await context.close();
  console.log('QR cache reuse is limited to three minutes; active login sessions still last five minutes');
}

async function retryableLoginScenario(browser) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror',error => errors.push(error.message));
  let starts = 0, polls = 0, submissions = 0, captchaReads = 0;
  await page.route('**/api/**',async route => {
    const endpoint = new URL(route.request().url()).pathname;
    let status = 200, result;
    if (endpoint === '/api/school-login/start') {
      starts++;
      result = {expires_at:new Date(Date.now()+300000).toISOString(),qr_content:'weixin://school/retryable',captcha_data_url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII='};
    } else if (endpoint === '/api/school-login/poll') {
      polls++;
      if (polls === 1) {status=409;result={code:'login_session_busy',error:'登录正在处理，请稍后重试'};}
      else if (polls === 2) {status=502;result={code:'school_session_exchange_failed',scan_confirmed:true,error:'学校身份已确认，但登录凭证获取失败'};}
      else result = {status:'waiting'};
    } else if (endpoint === '/api/school-login/identity') {
      submissions++;
      status = submissions === 1 ? 409 : 429;
      result = submissions === 1 ? {code:'login_session_busy',error:'登录正在处理，请稍后重试'} : {error:'请求过于频繁，请稍后重试'};
    } else if (endpoint === '/api/school-login/captcha') {
      captchaReads++;
      throw new Error('Retryable identity failures must not refresh the captcha');
    } else throw new Error(`Unexpected login API: ${endpoint}`);
    await route.fulfill({status,contentType:'application/json',body:JSON.stringify(result)});
  });
  await page.goto(`${base}/login?reconnect=1`);
  await page.getByText('正在重新连接学校登录',{exact:true}).waitFor();
  const qr = await page.locator('#school-qr img').getAttribute('src');
  await page.locator('[data-login-method=identity]').click();
  await page.locator('#submit-school-login:not(:disabled)').waitFor();
  await page.locator('#login-name').fill('张三');
  await page.locator('#student-no').fill('2026001');
  await page.locator('#captcha-code').fill('1234');
  for (const message of ['登录正在处理，请稍后重试','请求过于频繁，请稍后重试']) {
    await page.locator('#submit-school-login').click();
    await page.getByText(message,{exact:true}).waitFor();
    assert.equal(await page.locator('#captcha-code').inputValue(),'1234');
    assert.equal(await page.locator('#captcha-image').isVisible(),true);
  }
  await page.locator('[data-login-method=wechat]').click();
  await page.getByText('微信已确认登录，学校凭证尚未取得',{exact:true}).waitFor();
  await page.locator('[data-login-method=identity]').click();
  await page.locator('[data-login-method=wechat]').click();
  await page.getByText('学校身份已确认，但登录凭证获取失败',{exact:true}).waitFor();
  assert.equal(await page.locator('#school-qr img').getAttribute('src'),qr);
  assert.equal(starts,2);assert.equal(captchaReads,0);
  assert.deepEqual(errors,[]);
  await context.close();
  console.log('Busy and rate-limited login requests preserve challenges; switching preserves confirmed-scan recovery');
}

async function pendingLoginPollScenario(browser, authenticated, failRedirect = false) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  let releasePoll, pollStarted, pollFinished, scheduleReads = 0, starts = 0;
  const gate = new Promise(resolve => {releasePoll = resolve;});
  const started = new Promise(resolve => {pollStarted = resolve;});
  const finished = new Promise(resolve => {pollFinished = resolve;});
  await page.route('**/api/**', async route => {
    const endpoint = new URL(route.request().url()).pathname;
    let result, status = 200;
    if (endpoint === '/api/school-login/start') {
      starts++;
      result = {
        expires_at: new Date(Date.now() + 300000).toISOString(),
        qr_content: 'weixin://school/stale-poll-check',
        captcha_data_url: 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII=',
      };
    } else if (endpoint === '/api/school-login/poll') {
      pollStarted();
      await gate;
      result = {status: authenticated ? 'authenticated' : 'waiting'};
    } else if (endpoint === '/api/schedules') {
      scheduleReads++;
      if (failRedirect && scheduleReads === 1) {status=503;result={error:'登录成功，暂时无法获取签到设置'};}
      else result = [];
    } else throw new Error(`Unexpected API: ${endpoint}`);
    await route.fulfill({status,contentType: 'application/json', body: JSON.stringify(result)});
    if (endpoint === '/api/school-login/poll') pollFinished();
  });
  await page.route(`${base}/tasks`,route => route.fulfill({contentType:'text/html',body:'<!doctype html><title>Logged in</title>'}));
  await page.goto(`${base}/login?reconnect=1`);
  await page.locator('#school-qr img').waitFor();
  await started;
  await page.locator('[data-login-method=identity]').click();
  assert.equal(await page.locator('#identity-panel').isVisible(),true);
  assert.equal(starts,1,'A new login session must wait for the pending poll');
  releasePoll();
  await finished;
  if (authenticated) {
    if (failRedirect) await page.getByText('登录成功，暂时无法获取签到设置',{exact:true}).waitFor();
    await page.waitForURL('**/tasks');
    assert.equal(scheduleReads,failRedirect ? 2 : 1);
    assert.equal(starts,1,'A successful pending poll must finish login without creating another session');
  } else {
    await page.locator('#captcha-image').waitFor({state:'visible'});
    assert.equal(new URL(page.url()).pathname,'/login');
    assert.equal(scheduleReads,0);
    assert.equal(await page.locator('#identity-panel').isVisible(),true);
  }
  assert.deepEqual(errors, []);
  await context.close();
  console.log(`Switching tabs drains pending polling without overlapping sessions (${authenticated ? 'authenticated' : 'waiting'})`);
}

async function slowDashboardScenario(browser) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  // Accelerate only the account refresh interval, including the former setInterval implementation.
  await page.addInitScript(() => {
    for (const name of ['setTimeout', 'setInterval']) {
      const original = window[name].bind(window);
      window[name] = (action, delay, ...args) => original(action, delay === 15000 ? 25 : delay, ...args);
    }
  });
  let enabled = true, reads = 0, releaseRead, readStarted, readFinished;
  const gate = new Promise(resolve => {releaseRead = resolve;});
  const started = new Promise(resolve => {readStarted = resolve;});
  const finished = new Promise(resolve => {readFinished = resolve;});
  const dashboard = () => ({total_successful_checkins: 12, today_status: enabled ? 'waiting' : 'paused', auto_checkin_enabled: enabled, has_schedule: true, has_active_account: true});
  await page.route('**/api/**', async route => {
    const endpoint = new URL(route.request().url()).pathname;
    let result, delayed = false;
    if (endpoint === '/api/me') result = {id: accountId};
    else if (endpoint === '/api/schedules') result = [{id: scheduleId, local_time: '21:10:00', location_name: '1号宿舍楼'}];
    else if (endpoint === '/api/weixin/binding') result = {status: 'active', has_context: true};
    else if (endpoint === '/api/dashboard') {
      result = dashboard();
      reads++;
      if (reads === 2) {
        delayed = true;
        readStarted();
        await gate;
      }
    } else if (endpoint === '/api/auto-checkin') {
      enabled = route.request().postDataJSON().enabled;
      result = dashboard();
    } else throw new Error(`Unexpected API: ${endpoint}`);
    await route.fulfill({contentType: 'application/json', body: JSON.stringify(result)});
    if (delayed) readFinished();
  });
  await page.goto(`${base}/account`);
  await page.getByText('等待签到', {exact: true}).waitFor();
  await started;
  await page.waitForTimeout(100);
  assert.equal(reads, 2, 'A slow refresh must not start overlapping dashboard reads');
  await page.locator('#auto-checkin-toggle').click();
  await page.waitForFunction(() => document.querySelector('#auto-checkin-toggle').getAttribute('aria-checked') === 'false');
  releaseRead();
  await finished;
  await page.waitForTimeout(100);
  assert.equal(await page.locator('#auto-checkin-toggle').getAttribute('aria-checked'), 'false', 'A stale refresh must not overwrite the confirmed toggle state');
  assert.equal(await page.locator('#today-status').innerText(), '已关闭');
  assert.deepEqual(errors, []);
  await context.close();
  console.log('Slow dashboard refreshes stay serial and do not overwrite a newer toggle result');
}

async function notificationRebindingScenario(browser) {
  for (const initialStatus of ['binded_redirect', 'expired', 'verify_code_blocked']) {
    const context = await browser.newContext({viewport:{width:390,height:844},locale:'zh-CN'});
    const page = await context.newPage(), errors = [];
    page.on('pageerror', error => errors.push(error.message));
    let binding = initialStatus === 'binded_redirect'
      ? {status:'active',has_context:true}
      : {status:'unbound',has_context:false};
    let starts = 0, polls = 0;
    await page.route('**/api/**', async route => {
      const request = route.request(), endpoint = new URL(request.url()).pathname;
      let result, status = 200;
      if (endpoint === '/api/me') result = {id:accountId};
      else if (endpoint === '/api/weixin/binding') result = binding;
      else if (endpoint === '/api/weixin/login') {
        assert.equal(request.postDataJSON().rebind, binding.status !== 'unbound');
        starts++;
        status = 201;
        result = {session_id:guestId,qrcode_img_content:'weixin://notifications/rebind-check',expires_at:new Date(Date.now()+300000).toISOString()};
      } else if (endpoint === `/api/weixin/login/${guestId}/poll`) {
        polls++;
        if (starts === 1) result = {status:initialStatus};
        else {
          binding = {status:'active',has_context:false};
          result = {status:'confirmed'};
        }
      } else throw new Error(`Unexpected notification API: ${endpoint}`);
      await route.fulfill({status,contentType:'application/json',body:JSON.stringify(result)});
    });
    await page.goto(`${base}/notifications?manage=1`);
    if (initialStatus === 'binded_redirect') {
      await page.getByText('微信通知已连接',{exact:true}).waitFor();
      assert.equal(await page.locator('#bind-weixin').innerText(),'重新绑定微信');
      await page.locator('#bind-weixin').click();
    }
    const expected = {
      binded_redirect:'此微信 Bot 已绑定，本次扫码未更换绑定',
      expired:'二维码已过期，请重新获取',
      verify_code_blocked:'验证码错误次数过多，请稍后重新获取二维码',
    }[initialStatus];
    await page.getByText(expected,{exact:true}).waitFor();
    assert.equal(polls,1,'Terminal QR states must stop QR polling');
    assert.equal(starts,1,'A terminal QR state must not automatically replace the QR code');
    if (initialStatus === 'binded_redirect') {
      assert.equal(await page.locator('#notify-login').isVisible(),false);
      assert.equal(await page.locator('#bind-weixin').innerText(),'重新绑定微信');
    }
    await page.locator('#bind-weixin').click();
    await page.getByText('请向微信机器人发送一条消息',{exact:true}).waitFor();
    assert.equal(starts,2);
    assert.equal(await page.locator('#notify-login').isVisible(),false);
    binding = {status:'active',has_context:true};
    await page.getByText('微信通知已连接',{exact:true}).waitFor();
    assert.equal(await page.locator('#finish-notification').innerText(),'确认');
    assert.deepEqual(errors,[]);
    await context.close();
  }
  console.log('Weixin rebinding preserves connection state and distinguishes already-bound, expired and blocked scans');
}

(async () => {
  const options = {headless:true,args:['--no-sandbox']};
  if (process.env.CHROMIUM_PATH) options.executablePath = process.env.CHROMIUM_PATH;
  const browser = await chromium.launch(options);
  try {await scenario(browser,{width:1440,height:1000},false);await scenario(browser,{width:390,height:844},true);await systemThemeScenario(browser);await failedExchangeScenario(browser);await moduleLoadingScenario(browser);await loginCacheScenario(browser);await qrReuseLimitScenario(browser);await retryableLoginScenario(browser);await pendingLoginPollScenario(browser,false);await pendingLoginPollScenario(browser,true);await pendingLoginPollScenario(browser,true,true);await slowDashboardScenario(browser);await notificationRebindingScenario(browser);} finally {await browser.close();}
})().catch(error => {console.error(error);process.exitCode = 1;});
