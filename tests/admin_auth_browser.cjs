const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
function element(){return {value:'',disabled:false,hidden:true,textContent:'',events:{},addEventListener(name,fn){this.events[name]=fn;}};}
test('expired management requests prompt login without retrying a write',async()=>{
  const elements=new Map();const byId=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id);};
  let calls=0;
  const context={document:{getElementById:byId},fetch:async()=>{calls++;return {status:401,ok:false};},location:{assign(){throw Error('must preserve the page and unsaved work');}},AbortSignal:{timeout:()=>({})}};
  context.window=context;
  vm.runInNewContext(fs.readFileSync('src/admin-session.js','utf8'),context);
  await assert.rejects(context.fetch('/api/admin/config',{method:'PATCH'}),/重新登录/);
  assert.equal(calls,1);assert.equal(byId('session-expired').hidden,false);
  assert.match(byId('session-message').textContent,/刷新核实/);
});
test('login reports rate limiting then navigates only on success',async()=>{
  const elements=new Map();const byId=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id);};
  let status=429,destination=null;
  const context={document:{getElementById:byId},fetch:async()=>({ok:status===200,json:async()=>({error:'尝试过于频繁，请稍后重试'})}),location:{assign:path=>destination=path},AbortSignal:{timeout:()=>({})}};
  vm.runInNewContext(fs.readFileSync('src/login.js','utf8'),context);
  byId('password').value='a-long-private-password';
  await byId('login-form').events.submit({preventDefault(){}});
  assert.equal(destination,null);assert.match(byId('login-message').textContent,/频繁/);assert.equal(byId('login-submit').disabled,false);
  status=200;await byId('login-form').events.submit({preventDefault(){}});
  assert.equal(destination,'/');assert.equal(byId('password').value,'');
});
