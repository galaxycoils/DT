/* Browser → authenticated API → isolated storage → rendered result. No real inference. */
const {chromium}=require('playwright');
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const os=require('node:os');
const path=require('node:path');
const net=require('node:net');
const {spawn}=require('node:child_process');
const crypto=require('node:crypto');
async function freePort(){return new Promise(resolve=>{const s=net.createServer();s.listen(0,'127.0.0.1',()=>{const port=s.address().port;s.close(()=>resolve(port));});});}

async function smokeReactApp(browser, base, token, request, spacePort) {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const eventsPath = '/api/v1/workspaces/smoke-local/events';
  await page.addInitScript(({ eventsPath }) => {
    const fetchFromBrowser = window.fetch.bind(window);
    window.fetch = async (url, init) => {
      const response = await fetchFromBrowser(url, init);
      if (url !== eventsPath || window.disconnectDtStream) return response;
      if (!response.ok || !response.body) return response;
      const body = response.body.pipeThrough(new TransformStream({
        start(controller) {
          window.disconnectDtStream = () => {
            controller.enqueue(new TextEncoder().encode('id: 0\nevent: TEXT_MESSAGE_CONTENT\ndata: {}\n\n'));
            controller.terminate();
          };
        },
      }));
      return new Response(body, { status: response.status, headers: response.headers });
    };
  }, { eventsPath });

  try {
    const app = await page.goto(base + '/app/');
    assert.equal(app.status(), 200);
    assert.ok(app.headers()['content-security-policy'].includes(`frame-src http://127.0.0.1:${spacePort}`));
    const tokenField = page.getByLabel('Companion token', { exact: true });
    const connect = page.getByRole('button', { name: 'Connect Workspace', exact: true });
    await tokenField.fill('invalid-token');
    await connect.click();
    await page.getByRole('alert').filter({ hasText: 'Authentication required' }).waitFor();
    await tokenField.fill(token);
    const initialStream = page.waitForResponse(res => res.url() === base + eventsPath && res.ok());
    await connect.click();
    await initialStream;

    const message = page.getByRole('textbox', { name: 'Message', exact: true });
    await message.fill('React browser smoke test');
    await page.getByRole('button', { name: 'Send', exact: true }).click();
    await page.getByText('React browser smoke test', { exact: true }).waitFor();
    const history = await (await request('/api/v1/dm/muse/messages')).json();
    assert.ok(history.some(item => item.content === 'React browser smoke test'));

    const sendToWorkspace = async content => {
      const response = await request('/api/v1/dm/muse/messages', { content });
      assert.ok(response.ok, await response.text());
    };
    await sendToWorkspace('Live before reconnect');
    await page.getByText('Live before reconnect', { exact: true }).waitFor();
    let resumeStream;
    const paused = new Promise(resolve => { resumeStream = resolve; });
    await page.route(base + eventsPath, async route => {
      await paused;
      await route.continue();
    }, { times: 1 });
    const reconnect = page.waitForRequest(req => req.url() === base + eventsPath);
    await page.evaluate(() => window.disconnectDtStream());
    try {
      const resumedRequest = await reconnect;
      assert.match(resumedRequest.headers()['last-event-id'], /^[1-9]\d*$/);
      await sendToWorkspace('Saved during reconnect');
    } finally {
      resumeStream();
    }
    await page.getByText('Saved during reconnect', { exact: true }).waitFor();
    await sendToWorkspace('Live after reconnect');
    await page.getByText('Live after reconnect', { exact: true }).waitFor();

    await page.getByRole('button', { name: 'Pages' }).click();
    const space = page.getByRole('article').filter({ hasText: 'smoke-space' });
    await space.getByRole('button', { name: 'Open Split View' }).click();
    const frame = page.locator('iframe[title="Isolated Space smoke-space"]');
    await frame.waitFor();
    assert.equal(await frame.getAttribute('sandbox'), 'allow-scripts');
    await frame.contentFrame().getByRole('heading', { name: 'Hello' }).waitFor();
    await page.getByRole('button', { name: 'Close', exact: true }).click();

    const previewRoute = base + '/api/v1/spaces/smoke-space/url';
    await page.route(previewRoute, route => route.fulfill({ json: { url: base } }), { times: 1 });
    await space.getByRole('button', { name: 'Open Split View' }).click();
    await page.getByRole('alert').filter({ hasText: 'Update the DT daemon' }).waitFor();
    assert.equal(await frame.count(), 0);
    await page.getByRole('button', { name: 'Close', exact: true }).click();

    const alias = new URL(base);
    alias.hostname = 'localhost';
    await page.route(previewRoute, route => route.fulfill({
      json: { url: base, preview_parent_origin: alias.origin },
    }), { times: 1 });
    await space.getByRole('button', { name: 'Open Split View' }).click();
    await page.getByRole('alert').filter({ hasText: `Open DT at ${alias.origin}/app/` }).waitFor();
    assert.equal(await frame.count(), 0);
    await page.getByRole('button', { name: 'Close', exact: true }).click();

    await page.route(previewRoute, route => route.fulfill({ status: 404 }), { times: 1 });
    await space.getByRole('button', { name: 'Open Split View' }).click();
    await page.getByRole('alert').filter({ hasText: 'Could not load this Page preview' }).waitFor();
    assert.equal(await frame.count(), 0);
    await page.getByRole('button', { name: 'Close', exact: true }).click();

    const disconnected = page.waitForEvent('requestfailed', { predicate: req => req.url() === base + eventsPath });
    await page.getByRole('button', { name: 'Sign out', exact: true }).click();
    await disconnected;
    await tokenField.waitFor();
    assert.equal(await tokenField.inputValue(), '');
    assert.equal(await page.evaluate(value => [localStorage, sessionStorage].some(
      storage => Object.values(storage).some(item => item.includes(value)),
    ), token), false);
    await page.reload();
    await tokenField.waitFor();
    assert.deepEqual(errors, []);
  } finally {
    await page.close();
  }
  console.log('PASS: React local pairing, persisted chat, live event resume, sandboxed Page preview, and sign-out.');
}

(async()=>{
 const temp=await fs.mkdtemp(path.join(os.tmpdir(),'grok-browser-'));
 const port=await freePort(),spacePort=await freePort(),base=`http://127.0.0.1:${port}`,token=crypto.randomUUID();
 const config=path.join(temp,'config.json');
 await fs.writeFile(config,JSON.stringify({storage_dir:path.join(temp,'storage'),socket_path:path.join(temp,'helper.sock'),daemon_socket_path:path.join(temp,'daemon.sock'),openbot:{bots_dir:path.join(temp,'storage/bots')},muse:{enable_spaces:true,spaces_port:spacePort}}));
 const daemon=spawn(process.env.GM_DAEMON_BIN||path.resolve(__dirname,'../../target/debug/gm-daemon'),['--config',config,'serve','--bind',`127.0.0.1:${port}`],{env:{...process.env,GM_FORCE_MOCK_LLM:'1',GM_WORKSPACE_ID:'smoke-local',GROK_MUSE_COMPANION_TOKEN:token,DT_WEB_DIST:path.resolve(__dirname,'../../clients/web/dist'),DT_HOSTED_ENABLED:'0',DT_HOSTED_REQUIRED:'0',DT_DATABASE_URL:'',DATABASE_URL:''},stdio:['ignore','pipe','pipe']});
 let logs='',browser;daemon.stdout.on('data',v=>logs+=v);daemon.stderr.on('data',v=>logs+=v);daemon.on('error',e=>logs+=e.message);
 async function request(route,body,auth=true){return fetch(base+route,{method:body===undefined?'GET':'POST',headers:{...(auth?{Authorization:'Bearer '+token}:{}),'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});}
 try{
  let ready=false;for(let i=0;i<200;i++){try{if((await request('/api/bootstrap')).ok){ready=true;break;}}catch{}await new Promise(r=>setTimeout(r,100));}assert.ok(ready,'Daemon failed to start: '+logs);
  assert.equal((await request('/api/bootstrap',undefined,false)).status,401);
  assert.equal((await request('/ag-ui/events',undefined,false)).status,401);
  for(const asset of ['/','/companion.css','/companion.js'])assert.equal((await fetch(base+asset)).status,200);
  const index=await fetch(base);assert.ok(index.headers.get('content-security-policy').includes(`frame-src http://127.0.0.1:${spacePort}`));
  browser=await chromium.launch({executablePath:process.env.CHROME_BIN||'/usr/bin/chromium',headless:true,args:['--no-sandbox']});
  const page=await browser.newPage({viewport:{width:1440,height:1000}}),errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(base);await page.getByLabel('Companion bearer token').fill(token);await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#overlay').waitFor({state:'hidden'});
  await page.locator('#draft').fill('Browser smoke test');await page.locator('#draft').press('Enter');await page.locator('.message').first().waitFor();
  const dm=await (await request('/api/dm/muse/messages')).json();assert.ok((dm.messages||dm).some(m=>m.content==='Browser smoke test'));
  await page.reload();await page.getByLabel('Companion bearer token').fill(token);await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#overlay').waitFor({state:'hidden'});await page.getByText('Browser smoke test',{exact:true}).first().waitFor();
  await page.keyboard.press('Control+n');await page.getByLabel('Bot ID').fill('smoke-bot');await page.getByLabel('Name',{exact:true}).fill('Smoke Bot');await page.getByRole('button',{name:'Save Changes',exact:true}).click();await page.locator('#overlay').waitFor({state:'hidden'});
  let snapshot=await (await request('/api/bootstrap')).json();assert.ok(snapshot.bots.some(b=>b.id==='smoke-bot'));
  await page.keyboard.press('Control+,');await page.getByLabel('Theme',{exact:true}).selectOption('dark');await page.getByRole('button',{name:'Save Changes',exact:true}).click();await page.waitForFunction(()=>document.documentElement.dataset.theme==='dark');await page.keyboard.press('Escape');
  await page.getByRole('button',{name:'Pages',exact:true}).click();await page.getByRole('button',{name:'New Page'}).click();await page.getByLabel('Slug').fill('smoke-space');await page.getByLabel('Title',{exact:true}).fill('Smoke Space');await page.getByRole('button',{name:'Save Changes',exact:true}).click();await page.locator('#overlay').waitFor({state:'hidden'});await page.getByRole('button',{name:'Open split view',exact:true}).first().click();await page.locator('#space-iframe').waitFor({state:'visible'});assert.equal(await page.locator('#space-iframe').getAttribute('sandbox'),'allow-scripts');assert.ok((await page.locator('#space-iframe').getAttribute('src')).startsWith(`http://127.0.0.1:${spacePort}/spaces/v2/smoke-space/?token=`));await page.frameLocator('#space-iframe').getByRole('heading',{name:'Hello'}).waitFor();
  assert.equal((await request('/api/spaces/smoke-space/publish',{confirmed:false})).status,403);
  await page.keyboard.press('Escape');await page.getByRole('button',{name:'Toggle sidebar',exact:true}).click();await page.locator('#sidebar .chat').first().click();
  await page.screenshot({path:path.join(temp,'desktop.png')});
  await page.setViewportSize({width:390,height:844});assert.ok(await page.locator('#rail').evaluate(el=>el.getBoundingClientRect().bottom<=innerHeight));await page.getByRole('button',{name:'Toggle sidebar',exact:true}).click();await page.locator('#sidebar').waitFor({state:'visible'});await page.screenshot({path:path.join(temp,'mobile.png')});
  assert.deepEqual(errors,[]);
  await smokeReactApp(browser, base, token, request, spacePort);
  if(process.env.GM_SMOKE_ARTIFACT_DIR){await fs.mkdir(process.env.GM_SMOKE_ARTIFACT_DIR,{recursive:true});for(const name of ['desktop.png','mobile.png'])await fs.copyFile(path.join(temp,name),path.join(process.env.GM_SMOKE_ARTIFACT_DIR,name));}
  console.log('PASS: authentication, assets/CSP, DM persistence, bot creation, theme save, isolated Space, publish gate, desktop and mobile chrome.');
 }finally{if(browser)await browser.close();daemon.kill('SIGINT');await Promise.race([new Promise(r=>daemon.once('exit',r)),new Promise(r=>setTimeout(()=>{daemon.kill('SIGKILL');r();},3000))]);await fs.rm(temp,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exitCode=1;});
