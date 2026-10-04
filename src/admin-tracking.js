(() => {
  const byId=id=>document.getElementById(id);
  const drafts=new Map();const pages={tracking:1,permissions:1};
  window.issueWatchHasDrafts=()=>drafts.size>0;
  let tracking=[],permissions=[],busy=false,loading=false,repository='';
  async function request(path,method='GET',body){
    const response=await fetch(path,{method,cache:'no-store',signal:AbortSignal.timeout(30000),headers:method==='GET'?{}:{'Content-Type':'application/json','X-Issue-Watch-Admin':'1'},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const data=await response.json();if(!response.ok)throw new Error(data.error||'操作失败，请刷新确认');return data;
  }
  function page(kind,rows){
    const query=byId(kind+'-search').value.trim().toLowerCase();
    rows=rows.filter(row=>JSON.stringify(row).toLowerCase().includes(query));
    const total=Math.max(1,Math.ceil(rows.length/20));pages[kind]=Math.min(pages[kind],total);
    byId(kind+'-page').textContent=`${rows.length} 项 · 第 ${pages[kind]}/${total} 页`;
    byId(kind+'-previous').disabled=pages[kind]===1;byId(kind+'-next').disabled=pages[kind]===total;
    return rows.slice((pages[kind]-1)*20,pages[kind]*20);
  }
  async function operation(form,message,work){
    if(busy)return;busy=true;
    const controls=[...form.querySelectorAll('input,button')];controls.forEach(c=>c.disabled=true);byId(message).textContent='正在提交…';
    try{await work();}catch(error){byId(message).textContent=error.message;}
    finally{busy=false;controls.forEach(c=>c.disabled=false);await refresh();}
  }
  function renderTracking(){
    const container=byId('tracking-list');container.replaceChildren();
    const rows=page('tracking',tracking.filter(row=>!repository||row.repository.toLowerCase()===repository.toLowerCase()));
    if(!rows.length)container.textContent='暂无追踪 Issue。';
    for(const row of rows){
      const block=document.createElement('div');block.className='component';
      const title=document.createElement('p');title.textContent=`${row.repository} #${row.number} · ${row.title}`;
      const link=document.createElement('a');link.href=row.url;link.target='_blank';link.rel='noopener';link.textContent='查看 Issue';
      const status=document.createElement('p');status.textContent=`最近成功检查：${row.last_success_at||'尚未检查'}${row.error?' · 异常：'+row.error:''}`;
      const cancel=document.createElement('button');cancel.type='button';cancel.textContent='取消追踪';
      cancel.addEventListener('click',()=>{
        if(!confirm(`取消 ${row.repository} #${row.number} 的共享追踪？所有订阅者将停止收到动态，未发送动态将取消。`))return;
        operation(block,'tracking-message',async()=>{await request('/api/admin/tracking/'+row.id,'DELETE');byId('tracking-message').textContent='已取消共享追踪。';});
      });block.append(title,link,status,cancel);container.append(block);
    }
  }
  function renderPermissions(){
    if(byId('permissions-list').contains(document.activeElement))return;
    const container=byId('permissions-list');container.replaceChildren();
    const rows=page('permissions',permissions);if(!rows.length)container.textContent='尚未配置用户权限；所有用户默认无权限。';
    for(const row of rows){
      const block=document.createElement('div');block.className='component';const title=document.createElement('p');title.textContent=row.user_openid;
      const form=document.createElement('form');const draft=drafts.get(row.user_openid)||row;
      form.dataset.dirty=String(drafts.has(row.user_openid));const inputs={};
      for(const [key,label] of [['can_add','添加追踪'],['can_cancel','取消任意共享追踪']]){
        const field=document.createElement('label');field.textContent=label;const input=document.createElement('input');input.type='checkbox';input.checked=draft[key];
        inputs[key]=input;field.append(input);form.append(field);
        input.addEventListener('input',()=>{drafts.set(row.user_openid,{can_add:inputs.can_add.checked,can_cancel:inputs.can_cancel.checked});form.dataset.dirty='true';});
      }
      const save=document.createElement('button');save.textContent='保存权限';form.append(save);
      form.addEventListener('submit',event=>{event.preventDefault();operation(form,'permission-message',async()=>{
        await request('/api/admin/permissions/'+encodeURIComponent(row.user_openid),'PUT',{can_add:inputs.can_add.checked,can_cancel:inputs.can_cancel.checked});drafts.delete(row.user_openid);form.dataset.dirty='false';byId('permission-message').textContent='权限已保存；已有共享追踪保持运行。';
      });});block.append(title,form);container.append(block);
    }
  }
  async function refresh(){
    if(busy||loading)return;loading=true;
    const results=await Promise.allSettled([request('/api/admin/tracking'),request('/api/admin/permissions')]);
    if(results[0].status==='fulfilled'){tracking=results[0].value;renderTracking();}else byId('tracking-message').textContent='刷新失败：'+results[0].reason.message;
    if(results[1].status==='fulfilled'){permissions=results[1].value;renderPermissions();}else byId('permission-message').textContent='刷新失败：'+results[1].reason.message;
    loading=false;
  }
  byId('add-tracking').addEventListener('submit',event=>{event.preventDefault();const form=event.currentTarget;operation(form,'tracking-message',async()=>{
    const result=await request('/api/admin/tracking','POST',{url:form.elements.url.value.trim()});form.reset();form.dataset.dirty='false';byId('tracking-message').textContent=`${result.message} · ${result.tracking.title} · 当前状态：开放`;
  });});
  byId('add-permission').addEventListener('submit',event=>{event.preventDefault();const form=event.currentTarget;operation(form,'permission-message',async()=>{
    const user=form.elements.user_openid.value.trim();if(!user)throw new Error('请输入 user_openid');
    await request('/api/admin/permissions/'+encodeURIComponent(user),'PUT',{can_add:form.elements.can_add.checked,can_cancel:form.elements.can_cancel.checked});form.reset();form.dataset.dirty='false';byId('permission-message').textContent='权限已保存。';
  });});
  for(const id of ['add-tracking','add-permission'])byId(id).addEventListener('input',()=>byId(id).dataset.dirty='true');
  for(const kind of ['tracking','permissions']){
    byId(kind+'-search').addEventListener('input',()=>{pages[kind]=1;kind==='tracking'?renderTracking():renderPermissions();});
    for(const [suffix,step] of [['previous',-1],['next',1]])byId(kind+'-'+suffix).addEventListener('click',()=>{pages[kind]+=step;kind==='tracking'?renderTracking():renderPermissions();});
  }
  function repositoryRoute(){
    repository=new URLSearchParams(location.hash.split('?')[1]||'').get('repo')||'';
    byId('tracking-repository').textContent=repository?repository+' · Issue 追踪':'全部共享追踪';
    byId('repository-list-panel').hidden=!!repository;byId('repository-back').hidden=!repository;renderTracking();
  }
  window.addEventListener('hashchange',repositoryRoute);window.addEventListener('popstate',repositoryRoute);
  byId('repository-back').addEventListener('click',()=>{location.hash='repositories';});
  repositoryRoute();refresh();setInterval(refresh,10000);
})();
