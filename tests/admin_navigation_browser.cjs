const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
test('navigation restores a view and protects dirty forms before switching',()=>{
  const views=['overview','repositories','subscriptions','permissions','settings'].map(id=>({dataset:{view:id},hidden:false}));
  const links=views.map(v=>({dataset:{target:v.dataset.view},setAttribute(){},addEventListener(event,fn){this.click=fn;}}));
  const events={};let allowed=false;
  const sandbox={document:{querySelectorAll:s=>s==='[data-view]'?views:links,querySelector:()=>({})},location:{hash:'#settings'},history:{pushState(a,b,hash){sandbox.location.hash=hash;}},window:{addEventListener:(e,fn)=>events[e]=fn},confirm:()=>allowed};
  vm.runInNewContext(fs.readFileSync('src/admin-navigation.js','utf8'),sandbox);
  assert.deepEqual(views.filter(v=>!v.hidden).map(v=>v.dataset.view),['settings']);
  links[1].click({preventDefault(){}});
  assert.equal(sandbox.location.hash,'#settings');
  allowed=true;links[1].click({preventDefault(){}});
  assert.deepEqual(views.filter(v=>!v.hidden).map(v=>v.dataset.view),['repositories']);
  sandbox.location.hash='#overview';events.popstate();
  assert.deepEqual(views.filter(v=>!v.hidden).map(v=>v.dataset.view),['overview']);
});
