// Exercise the actual management script at browser DOM/network boundaries.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
class Element {
  constructor(tag='div') { this.tag=tag; this.children=[]; this.events={}; this.elements={}; this.value=''; this.textContent=''; this.disabled=false; }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren() { this.children=[]; this.textContent=''; }
  addEventListener(name, callback) { this.events[name]=callback; }
  querySelectorAll(tag) { return this.children.flatMap(child => [ ...(tag.split(',').includes(child.tag) ? [child] : []), ...child.querySelectorAll(tag)]); }
  querySelector(tag) { return this.querySelectorAll(tag)[0]; }
  contains(node) { return this === node || this.children.some(child=>child.contains(node)); }
  reset() { Object.values(this.elements).forEach(element => element.value=''); }
  async fire(event='submit') { this.events[event]({preventDefault(){},currentTarget:this}); await settle(); }
}
const settle = async () => { for(let i=0;i<5;i++) await new Promise(setImmediate); };
function setup() {
  const elements=new Map(); const byId=id=> { if(!elements.has(id)) elements.set(id,new Element()); return elements.get(id); };
  for(const [id,names] of [['add-subscription',['user_openid','qq_number_note']],['poll-interval',['seconds']],['tracking-interval',['seconds']],['add-repository',['repository']]]) {
    const form=byId(id); for(const name of names) { form.elements[name]=new Element('input'); form.append(form.elements[name]); } form.append(new Element('button'));
  }
  const requests=[], intervals=[], confirmations=[]; let rows=[], config={version:'v1',stage:'applied',error:null,saved:{repositories:['owner/repo'],poll_interval_seconds:60},applied:{repositories:['owner/repo'],poll_interval_seconds:60}};
  let liveDiagnostics={rooms:[],failures:[]}, liveError=false;
  let allowConfirm=true, pendingGate=null, selectionError=false;
  const sandbox={document:{getElementById:byId,createElement:tag=>new Element(tag),activeElement:null},AbortSignal:{timeout:()=>({})},confirm:text=>{confirmations.push(text);return allowConfirm;},setInterval:(fn,ms)=>intervals.push({fn,ms}),fetch:async(path,options)=>{
    requests.push({path,...options}); let data;
    if(selectionError && options.method==='PUT')return {ok:false,json:async()=>({error:'仓库已移除，请刷新'})};
    if(options.method!=='GET' && pendingGate) await pendingGate;
    if(path==='/api/admin/bilibili'){if(liveError)return {ok:false,json:async()=>({error:'offline'})};data=liveDiagnostics;} else if(path.endsWith('/bilibili') && options.method==='POST'){rows[0].live_subscriptions=[{room_id:7734200,anchor:'测试主播'}];data={message:'直播订阅已生效'};} else if(path.includes('/bilibili/') && options.method==='DELETE'){rows[0].live_subscriptions=[];data={removed:true};} else if(path==='/api/admin/repositories') { data=config.accepted?.repositories || config.applied.repositories; } else if(path.endsWith('/repositories') && options.method==='PUT') { rows.find(row=>path.includes('/'+row.id+'/')).repositories=JSON.parse(options.body).repositories; data={saved:true}; } else if(path.endsWith('/tracking')) { data=[]; } else if(path.endsWith('/config')) {
      if(options.method==='PATCH') { const body=JSON.parse(options.body); if(body.version!==config.version) return {ok:false,json:async()=>({error:'配置已变化，请刷新'})}; config={...config,version:'v2',stage:'saved',saved:{...config.saved,...body}}; }
      data=config;
    } else if(options.method==='POST') { const body=JSON.parse(options.body); rows.push({id:1,repositories:[],...body}); data={added:true}; }
    else if(options.method==='PATCH') { rows[0].qq_number_note=JSON.parse(options.body).qq_number_note; data={updated:true}; }
    else if(options.method==='DELETE') { rows=[]; data={removed:true}; } else data=rows;
    return {ok:true,json:async()=>JSON.parse(JSON.stringify(data))};
  }};
  vm.runInNewContext(fs.readFileSync('src/admin.js','utf8'),sandbox);
  return {byId,requests,intervals,confirmations,setLiveDiagnostics:data=>liveDiagnostics=data,failLiveDiagnostics:value=>liveError=value,setLiveRooms:roomsValue=>{rows[0].live_subscriptions=roomsValue;},failSelection:value=>{selectionError=value;},setConfig:value=>{config=value;},setConfirm:value=>{allowConfirm=value;},holdWrites:()=>{let resolve;pendingGate=new Promise(done=>resolve=done);return ()=>{pendingGate=null;resolve();};}};
}

test('administrator adds, edits and confirms removal of a receiver without interpreting user text as HTML',async()=>{
  const ui=setup(); await settle();
  const form=ui.byId('add-subscription'); form.elements.user_openid.value='openid-a'; form.elements.qq_number_note.value='<img src=x onerror=alert(1)>';
  await form.fire();
  assert.match(ui.byId('subscriptions').children[0].children[0].textContent, /<img/);
  const edit=ui.byId('subscriptions').children[0].children[1]; edit.children[0].children[0].value=''; await edit.fire();
  assert.equal(JSON.parse(ui.requests.find(request=>request.method==='PATCH').body).qq_number_note,'');
  let remove=ui.byId('subscriptions').querySelectorAll('button').find(button=>button.textContent==='移除');
  ui.setConfirm(false); await remove.fire('click'); assert.equal(ui.requests.filter(request=>request.method==='DELETE').length,0);
  ui.setConfirm(true); await remove.fire('click'); assert.equal(ui.requests.filter(request=>request.method==='DELETE').length,1);
  assert.equal(ui.byId('subscriptions').textContent,'尚无通知目标。');
  assert.ok(ui.requests.filter(request=>request.method!=='GET').every(request=>request.headers['X-Issue-Watch-Admin']==='1'));
});

test('submitted inputs are protected while saves are pending and become editable afterward',async()=>{
  const ui=setup(); await settle();
  const release=ui.holdWrites();
  const form=ui.byId('add-subscription'); form.elements.user_openid.value='openid-a';
  await form.fire();
  assert.equal(form.elements.user_openid.disabled,true,'addition inputs cannot be edited then discarded during a pending request');
  release(); await settle();
  assert.equal(form.elements.user_openid.disabled,false);
  const edit=ui.byId('subscriptions').children[0].children[1]; const note=edit.children[0].children[0];
  const releaseNote=ui.holdWrites(); note.value='123456'; await edit.fire(); assert.equal(note.disabled,true);
  releaseNote(); await settle();
  const intervalForm=ui.byId('poll-interval'); intervalForm.elements.seconds.value='120'; await intervalForm.elements.seconds.fire('input');
  const releaseInterval=ui.holdWrites(); await intervalForm.fire(); assert.equal(intervalForm.elements.seconds.disabled,true);
  releaseInterval(); await settle(); assert.equal(intervalForm.elements.seconds.disabled,false);
});

test('administrator adds and removes a shared watched repository and cannot remove the last one',async()=>{
  const ui=setup(); await settle();
  const form=ui.byId('add-repository'); form.elements.repository.value='owner/another'; await form.fire();
  const update=ui.requests.find(request=>request.method==='PATCH');
  assert.ok(update,'repository form must save a configuration');
  assert.deepEqual(JSON.parse(update.body).repositories,['owner/repo','owner/another']);
  assert.match(ui.byId('configuration-stage').textContent,/等待热加载/);
  let remove=ui.byId('repositories').querySelectorAll('button').find(button=>button.textContent==='移除仓库');
  ui.setConfirm(false); await remove.fire('click'); assert.equal(ui.requests.filter(request=>request.method==='PATCH').length,1);
  ui.setConfirm(true); await remove.fire('click');
  assert.deepEqual(JSON.parse(ui.requests.filter(request=>request.method==='PATCH')[1].body).repositories,['owner/another']);
  assert.equal(ui.byId('repositories').querySelectorAll('button').find(button=>button.textContent==='移除仓库').disabled,true);
});

test('repository changes preserve an unsaved polling interval and require a refresh before submitting that stale draft',async()=>{
  const ui=setup(); await settle();
  const interval=ui.byId('poll-interval').elements.seconds; interval.value='300'; await interval.fire('input');
  const form=ui.byId('add-repository'); form.elements.repository.value='owner/another'; await form.fire();
  assert.equal(interval.value,'300','repository save must not discard an interval draft');
  assert.match(ui.byId('configuration-message').textContent,/草稿/);
  await ui.byId('poll-interval').fire();
  assert.match(ui.byId('configuration-message').textContent,/刷新/);
  await ui.byId('reload-configuration').fire('click');
  assert.equal(interval.value,60);
});


test('tracking interval is saved independently from new issue polling', async()=>{
  const ui=setup(); await settle();
  const form=ui.byId('tracking-interval');form.elements.seconds.value='90';await form.elements.seconds.fire('input');await form.fire();
  const body=JSON.parse(ui.requests.filter(r=>r.method==='PATCH').at(-1).body);
  assert.equal(body.tracking_interval_seconds,90);assert.equal(body.poll_interval_seconds,undefined);
});

test('repository removal explains cancellation and accepted state distinguishes effective stopping from polling settings',async()=>{
  const ui=setup();await settle();
  ui.setConfig({version:'v2',stage:'accepted',saved:{repositories:['owner/a','owner/b'],poll_interval_seconds:60},applied:{repositories:['owner/a','owner/b','owner/old'],poll_interval_seconds:60}});
  await ui.byId('reload-configuration').fire('click');
  assert.match(ui.byId('configuration-stage').textContent,/已停止新发送/);
  const remove=ui.byId('repositories').querySelectorAll('button').find(b=>b.textContent==='移除仓库');
  await remove.fire('click');
  assert.match(ui.confirmations.at(-1),/等待重试的新建 Issue 通知/);
  assert.match(ui.confirmations.at(-1),/不补发停监期间/);
  assert.match(ui.confirmations.at(-1),/已开始的请求可能仍送达/);
});


test('repository picker keeps hidden selections, selects all active repos and clears without deleting target',async()=>{
  const ui=setup(); ui.setConfig({version:'v1',stage:'applied',saved:{repositories:['o/a','o/b']},applied:{repositories:['o/a','o/b']}}); await settle();
  const add=ui.byId('add-subscription'); add.elements.user_openid.value='alice'; await add.fire();
  const block=ui.byId('subscriptions').children[0];
  const picker=block.querySelectorAll('form').find(form=>form.querySelectorAll('button').some(b=>b.textContent==='保存仓库订阅'));
  assert.ok(picker,'target exposes a repository picker');
  const search=picker.querySelectorAll('input').find(input=>input.type==='search');
  let checks=picker.querySelectorAll('input').filter(input=>input.type==='checkbox');
  checks[0].checked=true; await checks[0].fire('change');
  search.value='o/b'; await search.fire('input');
  checks=picker.querySelectorAll('input').filter(input=>input.type==='checkbox');
  checks[0].checked=true; await checks[0].fire('change');
  await picker.fire();
  let writes=ui.requests.filter(r=>r.method==='PUT');
  assert.deepEqual(JSON.parse(writes.at(-1).body).repositories,['o/a','o/b']);
  let current=ui.byId('subscriptions').children[0].querySelectorAll('form').find(f=>f.querySelectorAll('button').some(b=>b.textContent==='保存仓库订阅'));
  await current.querySelectorAll('button').find(b=>b.textContent==='清空').fire('click'); await current.fire();
  writes=ui.requests.filter(r=>r.method==='PUT'); assert.deepEqual(JSON.parse(writes.at(-1).body).repositories,[]);
  assert.equal(ui.requests.filter(r=>r.method==='DELETE').length,0);
});


test('repository save failure retains draft and select-all covers hidden active repositories',async()=>{
  const ui=setup(); ui.setConfig({version:'v1',stage:'applied',saved:{repositories:['o/a','o/b']},applied:{repositories:['o/a','o/b']}}); await settle();
  const add=ui.byId('add-subscription'); add.elements.user_openid.value='alice'; await add.fire();
  const picker=()=>ui.byId('subscriptions').children[0].querySelectorAll('form').find(form=>form.querySelectorAll('button').some(b=>b.textContent==='保存仓库订阅'));
  let form=picker(); const search=form.querySelectorAll('input').find(i=>i.type==='search');search.value='o/a';await search.fire('input');
  await form.querySelectorAll('button').find(b=>b.textContent==='全选').fire('click');
  ui.failSelection(true);await form.fire();
  assert.match(ui.byId('subscription-message').textContent,/仓库已移除/);
  form=picker();assert.equal(form.querySelectorAll('input').filter(i=>i.type==='checkbox'&&i.checked).length,2);
  ui.failSelection(false);await form.fire();
  assert.deepEqual(JSON.parse(ui.requests.filter(r=>r.method==='PUT').at(-1).body).repositories,['o/a','o/b']);
});

test('live subscription editor preserves drafts and posts room input through the shared target',async()=>{
  const ui=setup(); await settle();
  const form=ui.byId('add-subscription'); form.elements.user_openid.value='live-user'; await form.fire();
  let block=ui.byId('subscriptions').children[0];
  let editor=block.children.find(child=>child.className==='live-subscriptions');
  assert.ok(editor,'target has a live subscription editor');
  const input=editor.querySelector('input'); input.value='https://live.bilibili.com/6'; await input.fire('input');
  await ui.intervals.find(item=>item.ms===10000).fn(); await settle();
  block=ui.byId('subscriptions').children[0];editor=block.children.find(child=>child.className==='live-subscriptions');
  assert.equal(editor.querySelector('input').value,'https://live.bilibili.com/6');
  await editor.querySelector('form').fire();
  const sent=ui.requests.find(request=>request.path==='/api/admin/subscriptions/1/bilibili' && request.method==='POST');
  assert.equal(JSON.parse(sent.body).room,'https://live.bilibili.com/6');
});

test('live subscription list shows anchor text safely and removes only the chosen room',async()=>{
  const ui=setup();await settle();const form=ui.byId('add-subscription');form.elements.user_openid.value='live-user';await form.fire();
  ui.setLiveRooms([{room_id:7734200,anchor:'<img src=x onerror=alert(1)>'}]);
  await ui.intervals.find(item=>item.ms===10000).fn();await settle();
  const block=ui.byId('subscriptions').children[0].children.find(child=>child.className==='live-subscriptions');
  const room=block.children.find(child=>child.textContent.includes('7734200'));
  assert.match(room.textContent,/<img/);
  await room.querySelector('button').fire('click');
  assert.ok(ui.requests.some(request=>request.path==='/api/admin/subscriptions/1/bilibili/7734200'&&request.method==='DELETE'));
});

test('live diagnostics distinguish stale live state from a failed check and show permanent failures',async()=>{
  const ui=setup();await settle();
  assert.equal(ui.byId('live-rooms').textContent,'暂无被订阅的直播间。');
  ui.setLiveDiagnostics({rooms:[{room_id:7734200,anchor:'测试主播',live_status:1,subscriber_count:2,checked_at:1700000000,last_success_at:1699999900,error:'查询失败'}],failures:[{target_id:7,room_id:7734200,error:'QQ 永久发送失败'}]});
  await ui.intervals.find(item=>item.ms===10000).fn();await settle();
  const text=ui.byId('live-rooms').children[0].textContent;
  assert.match(text,/上次成功状态：直播中/);assert.match(text,/查询失败/);assert.match(text,/2 人/);
  assert.match(ui.byId('live-failures').children[0].textContent,/通知目标 #7/);
  ui.failLiveDiagnostics(true);
  await ui.intervals.find(item=>item.ms===10000).fn();await settle();
  assert.match(ui.byId('live-message').textContent,/刷新失败/);
  assert.equal(ui.byId('live-rooms').children[0].textContent,text,'previous data remains visibly stale');
});
