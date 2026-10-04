const {test}=require('node:test');const assert=require('node:assert/strict');const fs=require('node:fs');const vm=require('node:vm');
class Element {
  constructor(tag='div'){this.tag=tag;this.children=[];this.events={};this.elements={};this.dataset={};this.value='';this.textContent='';this.disabled=false;this.hidden=false;}
  append(...nodes){this.children.push(...nodes);} replaceChildren(){this.children=[];this.textContent='';}
  addEventListener(event,fn){this.events[event]=fn;}setAttribute(){}contains(node){return this===node||this.children.some(child=>child.contains(node));}
  querySelectorAll(tags){return this.children.flatMap(c=>[...(tags.split(',').includes(c.tag)?[c]:[]),...c.querySelectorAll(tags)]);}
  reset(){Object.values(this.elements).forEach(e=>{e.value='';e.checked=false;});} async fire(event='submit'){this.events[event]({preventDefault(){},currentTarget:this});for(let i=0;i<8;i++)await new Promise(setImmediate);}
}
test('tracking and permissions can be managed without losing an unsaved permission draft',async()=>{
  const elements=new Map();const byId=id=>{if(!elements.has(id))elements.set(id,new Element());return elements.get(id);};
  for(const [id,names] of [['add-tracking',['url']],['add-permission',['user_openid','can_add','can_cancel']]]){const form=byId(id);for(const name of names){form.elements[name]=new Element('input');form.append(form.elements[name]);}form.append(new Element('button'));}
  let rows=[],permissions=[];const requests=[],scheduled=[];
  const sandbox={document:{getElementById:byId,createElement:tag=>new Element(tag),activeElement:null},window:{addEventListener(){}},location:{hash:'#repositories'},URLSearchParams,AbortSignal:{timeout:()=>({})},confirm:()=>true,setInterval:fn=>scheduled.push(fn),fetch:async(path,options)=>{
    requests.push({path,...options});let data;
    if(path.includes('/permissions')) {if(options.method==='PUT'){const body=JSON.parse(options.body);permissions=[{user_openid:'alice',...body}];data={saved:true};}else data=permissions;}
    else if(options.method==='POST'){rows=[{id:1,repository:'owner/repo',number:12,title:'bug',url:'https://github.com/owner/repo/issues/12'}];data={added:true,tracking:rows[0],message:'已添加追踪'};}
    else if(options.method==='DELETE'){rows=[];data={removed:true};}else data=rows;
    return {ok:true,json:async()=>data};
  }};
  vm.runInNewContext(fs.readFileSync('src/admin-tracking.js','utf8'),sandbox);
  const add=byId('add-tracking');add.elements.url.value='https://github.com/owner/repo/issues/12';await add.fire();
  assert.match(byId('tracking-list').children[0].children[0].textContent,/bug/);
  const grant=byId('add-permission');grant.elements.user_openid.value='alice';grant.elements.can_add.checked=true;await grant.fire();
  const edit=byId('permissions-list').querySelectorAll('input')[0];edit.checked=false;await edit.fire('input');
  for(const refresh of scheduled)await refresh();
  assert.equal(byId('permissions-list').querySelectorAll('input')[0].checked,false,'unsaved permissions survive polling');
  await byId('permissions-list').querySelectorAll('form')[0].fire();
  assert.equal(JSON.parse(requests.filter(r=>r.method==='PUT').at(-1).body).can_add,false);
  await byId('tracking-list').querySelectorAll('button')[0].fire('click');assert.equal(byId('tracking-list').textContent,'暂无追踪 Issue。');
});
