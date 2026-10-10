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

async function scenario(browser, viewport, qrLogin) {
  const context = await browser.newContext({viewport,colorScheme:'light',locale:'zh-CN'});
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {if (message.type() === 'error' && /Content Security Policy|violates|Refused to/.test(message.text())) errors.push(message.text());});
  let authenticated = false, scanReady = false, schedules = [], jobs = [], binding = {status:'unbound',has_context:false};
  let createdPayload, loginMethod = 'wechat';
  await page.route('**/api/**', async route => {
    const request = route.request(), endpoint = new URL(request.url()).pathname, method = request.method();
    let body = null;try {body = request.postDataJSON();} catch {}
    if (method !== 'GET') assert.equal(request.headers()['x-hist-request'],'1');
    let result, status = 200;
    if (endpoint === '/api/me') {status = authenticated ? 200 : 401;result = authenticated ? {id:accountId,username:'web-qa'} : {error:'需要登录或登录已过期'};}
    else if (endpoint === '/api/school-login/start') {loginMethod = body.method;result = {method:body.method,expires_at:new Date(Date.now()+300000).toISOString(),qr_content:'weixin://school/browser-check',captcha_data_url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII='};}
    else if (endpoint === '/api/school-login/captcha') result = {captcha_data_url:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII='};
    else if (endpoint === '/api/school-login/identity') {assert.equal(loginMethod,'identity');assert.equal(body.student_no,'2026001');authenticated = true;result = {status:'authenticated',school_account_id:accountId};}
    else if (endpoint === '/api/school-login/poll') {if (scanReady) authenticated = true;result = {status:scanReady ? 'authenticated' : 'waiting',school_account_id:accountId};}
    else if (endpoint === '/api/school-accounts') result = [{id:accountId,label:'页面验收账号',student_no:'2026001',credential_status:'active'}];
    else if (endpoint.endsWith('/courses')) result = {course_ids:['12','15'],courses:[{id:'12',name:'浏览器验收课程'},{id:'15',name:'第二门测试课程'}]};
    else if (endpoint === '/api/locations') result = locations;
    else if (endpoint === '/api/schedules' && method === 'POST') {createdPayload = body;status = 201;schedules = [{id:scheduleId,...body,next_run_at:new Date(Date.now()+86400000).toISOString()}];result = {id:scheduleId,next_run_at:schedules[0].next_run_at};}
    else if (endpoint === '/api/schedules') result = schedules;
    else if (endpoint === `/api/schedules/${scheduleId}`) {schedules = [{...schedules[0],...body}];result = {id:scheduleId,next_run_at:schedules[0].next_run_at};}
    else if (endpoint === `/api/schedules/${scheduleId}/run`) {status = 202;jobs = [{id:guestId,schedule_id:scheduleId,scheduled_at:new Date().toISOString(),status:'succeeded',message:'浏览器验收：签到成功',notification_status:null}];result = {job_id:guestId};}
    else if (endpoint === '/api/jobs') result = {jobs};
    else if (endpoint === '/api/weixin/binding') {if (method === 'DELETE') {binding = {status:'unbound',has_context:false};status = 204;} else result = binding;}
    else if (endpoint === '/api/weixin/login') {status = 201;result = {session_id:guestId,qrcode_img_content:'weixin://notification/browser-check',expires_at:new Date(Date.now()+300000).toISOString()};}
    else if (endpoint === `/api/weixin/login/${guestId}/poll`) {if (!body.verify_code) result = {status:'need_verifycode'};else {assert.equal(body.verify_code,'1234');binding = {status:'active',has_context:false};result = {status:'confirmed'};}}
    else if (endpoint === '/api/auth/logout') {status = 204;authenticated = false;}
    else throw new Error(`Unexpected browser API: ${method} ${endpoint}`);
    await route.fulfill({status,contentType:'application/json',body:status === 204 ? '' : JSON.stringify(result)});
  });
  await page.goto(base,{waitUntil:'networkidle'});
  const size = viewport.width < 600 ? 'mobile' : 'desktop';
  await page.screenshot({path:path.join(output,`home-${size}.png`),fullPage:true});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Home must fit viewport');
  assert.deepEqual(await page.locator('.site-header nav a').allTextContents(),['社区']);
  assert.equal(await page.getByRole('link',{name:'GitHub 项目仓库'}).getAttribute('href'),'https://github.com/HIST-Dev-Space/hist-auto-checkin');
  await page.getByRole('link',{name:'社区',exact:true}).click();
  await page.waitForURL('**/community');
  await page.waitForFunction(() => {const image = document.querySelector('.community-poster img');return image?.complete && image.naturalWidth > 0;});
  assert.equal(await page.locator('.community-poster img').getAttribute('src'),'/assets/hist-group.jpg');
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Community must fit viewport');
  await page.screenshot({path:path.join(output,`community-${size}.png`),fullPage:true});
  await page.locator('.brand').click();await page.waitForURL(`${base}/`);
  await page.locator('.hero-actions a.primary').click();
  await page.waitForURL('**/login');await page.locator('#school-qr img').waitFor();
  await page.screenshot({path:path.join(output,`login-${size}.png`),fullPage:true});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Login must fit viewport');
  if (qrLogin) {scanReady = true;} else {
    await page.locator('[data-login-method=identity]').click();await page.locator('#captcha-image').waitFor({state:'visible'});
    await page.locator('#refresh-captcha').click();await page.waitForTimeout(100);
    await page.locator('#login-name').fill('张三');await page.locator('#student-no').fill('2026001');await page.locator('#captcha-code').fill('1234');await page.locator('#submit-school-login').click();
  }
  await page.waitForURL('**/tasks');await page.locator('#courses-list input[value="12"]').waitFor();
  await page.screenshot({path:path.join(output,`tasks-initial-${size}.png`),fullPage:true});
  await page.locator('#courses-list input[value="12"]').check();await page.locator('#local-time').fill('21:15');
  await page.locator('[data-place=dorm-1]').click();await page.locator('#save-schedule').click();
  await page.locator('.schedule-item').waitFor();assert.equal(createdPayload.local_time,'21:15');assert.equal(createdPayload.location_name,'1号宿舍楼');assert.equal(createdPayload.latitude,35.281256);assert.deepEqual(createdPayload.course_ids,['12']);
  await page.evaluate(() => {document.activeElement?.blur();window.scrollTo({top:0,behavior:'instant'});});
  await page.waitForTimeout(100);
  await page.screenshot({path:path.join(output,`tasks-${size}.png`),fullPage:true});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Tasks must fit viewport');
  await page.locator('[data-schedule-action=toggle]').click();await page.locator('.schedule-item.paused').waitFor();assert.equal(schedules[0].enabled,false);
  await page.locator('[data-schedule-action=toggle]').click();await page.locator('.schedule-item:not(.paused)').waitFor();assert.equal(schedules[0].enabled,true);
  await page.locator('[data-schedule-action=edit]').click();await page.waitForFunction(() => document.querySelector('#editor-title').textContent === '修改签到任务');
  await page.locator('[data-location-kind=custom]').click();await page.locator('#custom-name').fill('自定义验收位置');await page.locator('#custom-latitude').fill('35.2805');await page.locator('#custom-longitude').fill('113.9365');await page.locator('#save-schedule').click();
  await page.waitForFunction(() => [...document.querySelectorAll('.schedule-details strong')].some(element => element.textContent.includes('自定义验收位置')));assert.equal(schedules[0].location_name,'自定义验收位置');
  await page.locator('[data-schedule-action=run]').click();await page.locator('.history-message').waitFor();assert.match(await page.locator('.history-message').innerText(),/签到成功/);
  await page.locator('#bind-weixin').click();await page.locator('#notify-qr img').waitFor();await page.locator('#notify-verify').waitFor({state:'visible'});
  await page.locator('#notify-verify-code').fill('1234');await page.locator('#submit-notify-verify').click();await page.waitForFunction(() => document.querySelector('#weixin-binding-status').textContent.includes('已绑定'));
  await page.locator('#unbind-weixin').click();await page.waitForFunction(() => document.querySelector('#weixin-binding-status').textContent.includes('尚未连接'));
  await page.locator('.theme-toggle').click();assert.equal(await page.locator('html').getAttribute('data-theme'),'dark');
  await page.locator('#logout').click();await page.waitForURL('**/login');
  assert.deepEqual(errors,[]);console.log(`${size}: landing → community → school login → task save/edit/pause/run → Weixin verification → logout passed`);
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
(async () => {
  const options = {headless:true,args:['--no-sandbox']};
  if (process.env.CHROMIUM_PATH) options.executablePath = process.env.CHROMIUM_PATH;
  const browser = await chromium.launch(options);
  try {await scenario(browser,{width:1440,height:1000},false);await scenario(browser,{width:390,height:844},true);await failedExchangeScenario(browser);} finally {await browser.close();}
})().catch(error => {console.error(error);process.exitCode = 1;});
