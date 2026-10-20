// Invoked only by public_admin_https.py with an isolated fixture and certificate.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const assert=require('node:assert/strict');
const {spawnSync}=require('node:child_process');
(async()=>{
  const base=process.env.SMOKE_ORIGIN;
  const browser=await chromium.launch({executablePath:process.env.CHROME_BIN,headless:true,args:[`--ignore-certificate-errors-spki-list=${process.env.SMOKE_SPKI}`,'--no-proxy-server']});
  try{
    const first=await browser.newContext(),second=await browser.newContext();
    async function login(context,password){
      const page=await context.newPage();await page.goto(base);
      await page.locator('#password').fill(password);await page.locator('#login-submit').click();
      await page.waitForURL(base+'/');await page.locator('#logout').waitFor();return page;
    }
    const page=await login(first,'long-test-admin-password');
    const cookies=await first.cookies();const cookie=cookies.find(item=>item.name==='__Host-issue_watch_session');
    assert.ok(cookie?.secure&&cookie.httpOnly&&cookie.sameSite==='Strict');
    assert.ok(cookie.expires>Date.now()/1000+364*86400);
    assert.equal(await page.evaluate(()=>document.cookie.includes('__Host-issue_watch_session')),false);
    assert.equal(await page.evaluate(async()=> (await fetch('/api/admin/config')).status),200);
    await page.reload();assert.equal(new URL(page.url()).pathname,'/');
    const other=await login(second,'long-test-admin-password');
    await page.locator('#logout').click();await page.waitForURL(base+'/login');
    assert.equal(await other.evaluate(async()=> (await fetch('/api/auth/session')).status),200);
    const reset=spawnSync('target/debug/issue-watch',['--admin-password-stdin'],{env:{...process.env,ISSUE_WATCH_CONFIG:process.env.ISSUE_WATCH_BROWSER_FIXTURE_ROOT+'/config.toml'},input:'a-replacement-browser-password\n',encoding:'utf8'});
    assert.equal(reset.status,0,reset.stderr);
    await other.evaluate(async()=>{try{await fetch('/api/admin/config');}catch{}});
    assert.equal(await other.locator('#session-expired').isVisible(),true);
    assert.match(await other.locator('#session-message').textContent(),/重新登录/);
    await login(second,'a-replacement-browser-password');
    console.log('Real Chrome HTTPS: login, Secure/HttpOnly cookies, reload, independent devices, logout and live SSH password reset passed.');
  }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
