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
(async()=>{
 const temp=await fs.mkdtemp(path.join(os.tmpdir(),'grok-browser-'));
 const port=await freePort(),spacePort=await freePort(),base=`http://127.0.0.1:${port}`,token=crypto.randomUUID();
 const config=path.join(temp,'config.json');
 await fs.writeFile(config,JSON.stringify({storage_dir:path.join(temp,'storage'),socket_path:path.join(temp,'helper.sock'),daemon_socket_path:path.join(temp,'daemon.sock'),openbot:{bots_dir:path.join(temp,'storage/bots')},muse:{enable_spaces:true,spaces_port:spacePort}}));
 const daemon=spawn(process.env.GM_DAEMON_BIN||path.resolve(__dirname,'../../target/debug/gm-daemon'),['--config',config,'serve','--bind',`127.0.0.1:${port}`],{env:{...process.env,GM_FORCE_MOCK_LLM:'1',GROK_MUSE_COMPANION_TOKEN:token},stdio:['ignore','pipe','pipe']});
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
  await page.keyboard.press('Control+n');await page.getByLabel('Bot ID').fill('smoke-bot');await page.getByLabel('Name',{exact:true}).fill('Smoke Bot');await page.getByRole('button',{name:'Save Changes',exact:true}).click();await page.locator('#overlay').waitFor({state:'hidden'});
  let snapshot=await (await request('/api/bootstrap')).json();assert.ok(snapshot.bots.some(b=>b.id==='smoke-bot'));
  await page.keyboard.press('Control+,');await page.getByLabel('Theme',{exact:true}).selectOption('dark');await page.getByRole('button',{name:'Save Changes',exact:true}).click();await page.waitForFunction(()=>document.documentElement.dataset.theme==='dark');await page.keyboard.press('Escape');
  await page.getByRole('button',{name:'Spaces',exact:true}).click();await page.getByRole('button',{name:'New Static Space'}).click();await page.getByLabel('Slug').fill('smoke-space');await page.getByLabel('Title',{exact:true}).fill('Smoke Space');await page.getByRole('button',{name:'Save Changes',exact:true}).click();await page.locator('#overlay').waitFor({state:'hidden'});await page.getByRole('button',{name:'Open split view',exact:true}).first().click();await page.locator('#space-iframe').waitFor({state:'visible'});assert.equal(await page.locator('#space-iframe').getAttribute('sandbox'),'allow-scripts');assert.ok((await page.locator('#space-iframe').getAttribute('src')).startsWith(`http://127.0.0.1:${spacePort}/spaces/v2/smoke-space/?token=`));await page.frameLocator('#space-iframe').getByRole('heading',{name:'Hello'}).waitFor();
  assert.equal((await request('/api/spaces/smoke-space/publish',{confirmed:false})).status,403);
  await page.keyboard.press('Escape');await page.getByRole('button',{name:'Bots',exact:true}).click();
  await page.screenshot({path:path.join(temp,'desktop.png')});
  await page.setViewportSize({width:390,height:844});assert.ok(await page.locator('#rail').evaluate(el=>el.getBoundingClientRect().bottom<=innerHeight));await page.getByRole('button',{name:'☰',exact:true}).click();await page.locator('#sidebar').waitFor({state:'visible'});await page.screenshot({path:path.join(temp,'mobile.png')});
  assert.deepEqual(errors,[]);
  if(process.env.GM_SMOKE_ARTIFACT_DIR){await fs.mkdir(process.env.GM_SMOKE_ARTIFACT_DIR,{recursive:true});for(const name of ['desktop.png','mobile.png'])await fs.copyFile(path.join(temp,name),path.join(process.env.GM_SMOKE_ARTIFACT_DIR,name));}
  console.log('PASS: authentication, assets/CSP, DM persistence, bot creation, theme save, isolated Space, publish gate, desktop and mobile chrome.');
 }finally{if(browser)await browser.close();daemon.kill('SIGINT');await Promise.race([new Promise(r=>daemon.once('exit',r)),new Promise(r=>setTimeout(()=>{daemon.kill('SIGKILL');r();},3000))]);await fs.rm(temp,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exitCode=1;});
