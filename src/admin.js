(() => {
  const byId = id => document.getElementById(id);
  function markDirty(form, dirty) { if (form?.dataset) form.dataset.dirty = String(dirty); }
  ['add-subscription','poll-interval','tracking-interval','add-repository'].forEach(id => byId(id).addEventListener('input',()=>markDirty(byId(id),true)));
  const pages={subscription:1,repository:1};
  function listPage(kind,rows,text){
    const query=byId(kind+'-search').value.trim().toLowerCase();rows=rows.filter(row=>text(row).toLowerCase().includes(query));
    const total=Math.max(1,Math.ceil(rows.length/20));pages[kind]=Math.min(pages[kind],total);
    byId(kind+'-page').textContent=`${rows.length} 项 · 第 ${pages[kind]}/${total} 页`;
    byId(kind+'-previous').disabled=pages[kind]===1;byId(kind+'-next').disabled=pages[kind]===total;
    return rows.slice((pages[kind]-1)*20,pages[kind]*20);
  }
  let trackingDirty=false,trackingVersion=null;
  let subscriptionLoading = false;
  let subscriptionWrites = 0;
  let configData = null, intervalVersion = null, intervalDirty = false, configurationLoading = false, configurationBusy = false;
  let configurationSequence = 0;
  let repositoryRenderKey = null;
  const noteDrafts = new Map();
  const repositoryDrafts = new Map();
  const liveDrafts = new Map();
  const expandedSubscriptions = new Set();
  function livePicker(row) {
    const block=document.createElement('div'); block.className='live-subscriptions';
    const heading=document.createElement('p'); heading.textContent='直播订阅'; block.append(heading);
    const rooms=row.live_subscriptions||[];
    if(!rooms.length){const empty=document.createElement('p');empty.textContent='未订阅任何直播间';block.append(empty);}
    for(const room of rooms){
      const item=document.createElement('p'); item.textContent=`${room.anchor} · 房间 ${room.room_id}`;
      const remove=document.createElement('button');remove.type='button';remove.textContent='取消直播订阅';
      remove.addEventListener('click',()=>operation(block,'subscription-message',async()=>{
        await request(`/api/admin/subscriptions/${row.id}/bilibili/${room.room_id}`,'DELETE');
        byId('subscription-message').textContent='已取消直播订阅，待发送通知已取消；已发出的请求可能仍送达。';
      })); item.append(remove);block.append(item);
    }
    const form=document.createElement('form');
    const label=document.createElement('label');label.textContent='直播房间号或链接';
    const input=document.createElement('input');input.required=true;input.maxLength=2048;input.value=liveDrafts.get(row.id)||'';
    input.addEventListener('input',()=>{liveDrafts.set(row.id,input.value);markDirty(form,Boolean(input.value));});
    const add=document.createElement('button');add.textContent='添加直播订阅';
    label.append(input);form.append(label,add);markDirty(form,Boolean(input.value));block.append(form);
    form.addEventListener('submit',event=>{
      event.preventDefault();operation(block,'subscription-message',async()=>{
        const data=await request(`/api/admin/subscriptions/${row.id}/bilibili`,'POST',{room:input.value.trim()});
        liveDrafts.delete(row.id);input.value='';markDirty(form,false);
        byId('subscription-message').textContent=data.message||'直播订阅已生效。';
      });
    });
    return block;
  }
  function repositoryPicker(row, choices) {
    const form = document.createElement('form');
    form.className = 'repository-picker';
    const selected = new Set(repositoryDrafts.get(row.id) || row.repositories || []);
    const search = document.createElement('input'); search.type = 'search'; search.placeholder = '搜索可订阅仓库'; search.setAttribute?.('aria-label','搜索可订阅仓库');
    const all = document.createElement('button'); all.type = 'button'; all.textContent = '全选';
    const clear = document.createElement('button'); clear.type = 'button'; clear.textContent = '清空';
    const options = document.createElement('div');
    const summary = document.createElement('p');
    const save = document.createElement('button'); save.textContent = '保存仓库订阅';
    const remember = () => { repositoryDrafts.set(row.id,[...selected]); markDirty(form,true); };
    function render() {
      summary.textContent = selected.size ? `已选择 ${selected.size} 个仓库` : '未订阅任何仓库';
      options.replaceChildren();
      const names = [...new Set([...choices,...selected])];
      for (const name of names.filter(name=>name.toLowerCase().includes(search.value.toLowerCase()))) {
        const label = document.createElement('label');
        const check = document.createElement('input'); check.type = 'checkbox'; check.checked = selected.has(name);
        check.addEventListener('change',()=>{ if(check.checked)selected.add(name);else selected.delete(name);remember();summary.textContent=selected.size?`已选择 ${selected.size} 个仓库`:'未订阅任何仓库'; });
        const text = document.createElement('span'); text.textContent = name + (choices.includes(name)?'':'（已不可用，请取消选择）');
        label.append(check,text); options.append(label);
      }
    }
    search.addEventListener('input',render);
    all.addEventListener('click',()=>{ selected.clear(); choices.forEach(name=>selected.add(name)); remember();render(); });
    clear.addEventListener('click',()=>{ selected.clear();remember();render(); });
    form.append(summary,search,all,clear,options,save); markDirty(form,repositoryDrafts.has(row.id)); render();
    form.addEventListener('submit',event=>{
      event.preventDefault();
      operation(form,'subscription-message',async()=>{
        await request(`/api/admin/subscriptions/${row.id}/repositories`,'PUT',{repositories:[...selected]});
        repositoryDrafts.delete(row.id);markDirty(form,false);
        byId('subscription-message').textContent='仓库订阅已生效；取消的积压不会恢复，已开始的请求可能仍送达。';
      });
    });
    return form;
  }
  async function request(path, method = 'GET', body) {
    const response = await fetch(path, {method, cache:'no-store', signal:AbortSignal.timeout(8000),
      headers: method === 'GET' ? {} : {'Content-Type':'application/json','X-Issue-Watch-Admin':'1'},
      ...(body === undefined ? {} : {body:JSON.stringify(body)})});
    let data;
    try { data = await response.json(); } catch (_) { throw new Error('无法读取服务响应，请刷新确认操作结果'); }
    if (!response.ok) throw new Error(data.error || '操作失败，请检查输入并刷新确认');
    return data;
  }
  async function refreshSubscriptions() {
    if (subscriptionLoading || subscriptionWrites) return;
    subscriptionLoading = true;
    try {
      const [allRows,choices] = await Promise.all([request('/api/admin/subscriptions'),request('/api/admin/repositories')]);
      const rows=listPage('subscription',allRows,row=>row.user_openid+' '+(row.qq_number_note||'')+' '+(row.repositories||[]).join(' '));
      if (subscriptionWrites) return;
      const container = byId('subscriptions');
      container.replaceChildren();
      if (!rows.length) container.textContent = allRows.length ? '没有匹配的通知目标，请尝试其他关键词。' : '尚无通知目标。';
      for (const row of rows) {
        const block = document.createElement('details'); block.className = 'component';
        block.open = expandedSubscriptions.has(row.id) || noteDrafts.has(row.id) || repositoryDrafts.has(row.id) || liveDrafts.has(row.id);
        block.addEventListener('toggle', () => { if (block.open) expandedSubscriptions.add(row.id); else expandedSubscriptions.delete(row.id); });
        const identity = document.createElement('summary');
        identity.textContent = `通知目标 #${row.id} · ${row.qq_number_note || row.user_openid} · ${row.repositories?.length || 0} 个仓库`;
        const identifier = document.createElement('p'); identifier.textContent = `user_openid：${row.user_openid}`;
        const form = document.createElement('form');
        const noteLabel = document.createElement('label'); noteLabel.textContent = 'QQ 号备注（选填） ';
        const note = document.createElement('input'); note.value = noteDrafts.has(row.id) ? noteDrafts.get(row.id) : row.qq_number_note || ''; note.maxLength = 256;
        note.addEventListener('input', () => { noteDrafts.set(row.id, note.value); markDirty(form,true); });
        noteLabel.append(note);
        const save = document.createElement('button'); save.textContent = '保存备注';
        const remove = document.createElement('button'); remove.type = 'button'; remove.textContent = '移除'; remove.className = 'danger';
        form.append(noteLabel, save, remove);
        form.addEventListener('submit', event => {
          event.preventDefault(); operation(form, 'subscription-message', async () => {
            await request(`/api/admin/subscriptions/${row.id}`, 'PATCH', {qq_number_note:note.value.trim()});
            noteDrafts.delete(row.id); markDirty(form,false);
            byId('subscription-message').textContent = '备注已保存。'; await refreshSubscriptions();
          });
        });
        remove.addEventListener('click', () => {
          if (!confirm(`移除 ${row.user_openid}？未发送通知将取消，备注将删除；正在发出的请求可能仍送达。对方可重新 /bind，旧积压不会恢复。`)) return;
          operation(form, 'subscription-message', async () => {
            await request(`/api/admin/subscriptions/${row.id}`, 'DELETE');
            noteDrafts.delete(row.id); repositoryDrafts.delete(row.id); liveDrafts.delete(row.id); markDirty(form,false);
            byId('subscription-message').textContent = '已移除通知目标及全部仓库和直播订阅。'; await refreshSubscriptions();
          });
        });
        const subscribed = document.createElement('p'); subscribed.textContent = row.repositories?.length ? `订阅 ${row.repositories.length} 个仓库：${row.repositories.join('、')}` : '未订阅任何仓库';
        block.append(identity, form, identifier, subscribed, repositoryPicker(row,choices),livePicker(row)); container.append(block);
      }
    } catch (error) { byId('subscription-message').textContent = '接收者刷新失败：'+error.message; }
    finally { subscriptionLoading = false; }
  }
  async function operation(form, messageId, work) {
    const controls = Array.from(form.querySelectorAll('button,input')).map(control => [control, control.disabled]);
    controls.forEach(([control]) => control.disabled = true);
    const subscriptionOperation = messageId === 'subscription-message';
    if (subscriptionOperation) subscriptionWrites++;
    byId(messageId).textContent = '正在提交…';
    try { await work(); }
    catch (error) { byId(messageId).textContent = error.message; }
    finally {
      controls.forEach(([control, disabled]) => control.disabled = disabled);
      if (subscriptionOperation) { subscriptionWrites--; await refreshSubscriptions(); }
    }
  }
  byId('add-subscription').addEventListener('submit', event => {
    event.preventDefault();
    const form = event.currentTarget;
    operation(form, 'subscription-message', async () => {
      const data = await request('/api/admin/subscriptions', 'POST', {
        user_openid:form.elements.user_openid.value.trim(), qq_number_note:form.elements.qq_number_note.value.trim()});
      form.reset(); markDirty(form,false);
      byId('subscription-message').textContent = data.added ? '已添加通知目标，尚未订阅任何仓库；请配置仓库订阅。' : '已存在相同接收者，原备注保持不变。';
      await refreshSubscriptions();
    });
  });
  function showConfiguration(data) {
    configData = data;
    const stages = {saved:'已保存，等待热加载接受',accepted:'仓库变更已生效，移除仓库已停止新发送；轮询设置等待当前任务结束后应用',applied:'监控任务已采用当前配置，仓库变更已生效',rejected:'加载失败，继续使用上一份有效配置'};
    byId('configuration-stage').textContent = (stages[data.stage] || '未知') + `。当前实际轮询间隔：${data.applied.poll_interval_seconds} 秒；追踪检查间隔：${data.applied.tracking_interval_seconds??300} 秒。` + (data.error ? ' '+data.error : '');
    const trackingForm=byId('tracking-interval');
    if(!trackingDirty && data.saved){trackingForm.elements.seconds.value=data.saved.tracking_interval_seconds??300;trackingVersion=data.version;}
    trackingForm.elements.seconds.disabled=!data.saved;trackingForm.querySelector('button').disabled=!data.saved||configurationBusy;
    if(trackingDirty && trackingVersion!==data.version)byId('configuration-message').textContent='配置已变化，追踪间隔草稿尚未保存；请刷新配置后重新编辑。';
    const form = byId('poll-interval');
    if (!intervalDirty && data.saved) { form.elements.seconds.value = data.saved.poll_interval_seconds; intervalVersion = data.version; }
    form.elements.seconds.disabled = !data.saved;
    form.querySelector('button').disabled = !data.saved || configurationBusy;
    if (intervalDirty && intervalVersion !== data.version) byId('configuration-message').textContent = '配置已变化，当前间隔草稿尚未保存；请刷新配置后重新编辑。';
    byId('add-repository').querySelector('button').disabled = !data.saved || configurationBusy;
    const renderKey = JSON.stringify([data.version, data.saved?.repositories, configurationBusy,byId('repository-search').value,pages.repository]);
    if (repositoryRenderKey === renderKey) return;
    repositoryRenderKey = renderKey;
    const container = byId('repositories'); container.replaceChildren();
    if (data.saved) for (const repository of listPage('repository',data.saved.repositories,row=>row)) {
      const block = document.createElement('div'); block.className = 'component';
      const name = document.createElement('p'); name.textContent = repository;
      const remove = document.createElement('button'); remove.type = 'button'; remove.textContent = '移除仓库'; remove.className = 'danger';
      remove.disabled = data.saved.repositories.length <= 1 || configurationBusy;
      if (data.saved.repositories.length <= 1) remove.title = '至少保留一个监控仓库';
      remove.addEventListener('click', () => {
        if (data.saved.repositories.length <= 1 || configurationBusy) return;
        operation(block,'repository-message',async()=>{
          const tracked=await request('/api/admin/tracking');const count=tracked.filter(row=>row.repository.toLowerCase()===repository.toLowerCase()).length;
          if(!confirm(`移除 ${repository}？将取消该仓库未发送及等待重试的新建 Issue 通知、${count} 个共享追踪及其未发送动态。已开始的请求可能仍送达。所有用户对此仓库的订阅将清除。重新添加从生效时间开始，不补发停监期间的 Issue，也不恢复旧追踪或个人订阅。`))return;
          await saveConfiguration(block,{version:data.version,repositories:data.saved.repositories.filter(name=>name!==repository)});
        });
      });
      const open=document.createElement('button');open.type='button';open.className='primary';open.textContent='管理追踪';open.addEventListener('click',()=>{window.location.hash='repositories?repo='+encodeURIComponent(repository);});
      block.append(name,remove,open); container.append(block);
    } else container.textContent = '配置无法读取，请修正文件后刷新。';
  }
  async function refreshConfiguration() {
    if (configurationLoading || configurationBusy) return;
    configurationLoading = true;
    const sequence = configurationSequence;
    try { const data = await request('/api/admin/config'); if (sequence === configurationSequence) showConfiguration(data); }
    catch (error) { byId('configuration-message').textContent = '配置刷新失败：'+error.message; }
    finally { configurationLoading = false; }
  }
  async function saveConfiguration(form, body) {
    if (configurationBusy || !configData?.saved) return;
    configurationSequence++;
    configurationBusy = true;
    const messageId=form===byId('poll-interval')||form===byId('tracking-interval')?'configuration-message':'repository-message';
    await operation(form, messageId, async () => {
      const data = await request('/api/admin/config', 'PATCH', body);
      if (body.poll_interval_seconds !== undefined) { intervalDirty = false; markDirty(form,false); }
      if(body.tracking_interval_seconds!==undefined){trackingDirty=false;markDirty(form,false);}
      showConfiguration(data);
      if (form === byId('add-repository')) {form.reset();markDirty(form,false);}
      byId(messageId).textContent='配置已保存；等待热加载，请在设置查看应用状态。';
    });
    configurationBusy = false;
    await refreshConfiguration();
  }
  byId('poll-interval').elements.seconds.addEventListener('input', () => { intervalDirty = true; });
  byId('poll-interval').addEventListener('submit', event => {
    event.preventDefault(); const form = event.currentTarget;
    const seconds = Number(form.elements.seconds.value);
    if (!Number.isInteger(seconds) || seconds < 1 || seconds > 86400) { byId('configuration-message').textContent = '请输入 1–86400 的整数。'; return; }
    saveConfiguration(form, {version:intervalVersion, poll_interval_seconds:seconds});
  });
  byId('reload-configuration').addEventListener('click', () => { if (configurationBusy) return; if((intervalDirty||trackingDirty)&&!confirm('放弃未保存的间隔设置？'))return; intervalDirty=false;trackingDirty=false;markDirty(byId('poll-interval'),false);markDirty(byId('tracking-interval'),false);refreshConfiguration(); });
  byId('add-repository').addEventListener('submit', event => {
    event.preventDefault(); const form = event.currentTarget;
    if (!configData?.saved || configurationBusy) return;
    const repository = form.elements.repository.value.trim();
    if (!repository) return;
    if (configData.saved.repositories.some(name => name.toLowerCase() === repository.toLowerCase())) { byId('repository-message').textContent = '已存在相同监控仓库。'; return; }
    saveConfiguration(form, {version:configData.version,repositories:[...configData.saved.repositories,repository]});
  });
  byId('tracking-interval').elements.seconds.addEventListener('input',()=>trackingDirty=true);
  byId('tracking-interval').addEventListener('submit',event=>{
    event.preventDefault();const form=event.currentTarget;const seconds=Number(form.elements.seconds.value);
    if(!Number.isInteger(seconds)||seconds<1||seconds>86400){byId('configuration-message').textContent='请输入 1–86400 的整数。';return;}
    saveConfiguration(form,{version:trackingVersion,tracking_interval_seconds:seconds});
  });
  for(const kind of ['subscription','repository']){
    const render=()=>kind==='subscription'?refreshSubscriptions():refreshConfiguration();
    byId(kind+'-search').addEventListener('input',()=>{pages[kind]=1;render();});
    for(const [suffix,step]of [['previous',-1],['next',1]])byId(kind+'-'+suffix).addEventListener('click',()=>{pages[kind]+=step;render();});
  }
  let liveLoading=false;
  async function refreshLive() {
    if(liveLoading)return;liveLoading=true;
    try {
      const data=await request('/api/admin/bilibili');
      if(!Array.isArray(data.rooms)||!Array.isArray(data.failures))throw new Error('直播状态响应无效');
      const rooms=byId('live-rooms');rooms.replaceChildren();
      if(!data.rooms.length)rooms.textContent='暂无被订阅的直播间。';
      const time=value=>value==null?'尚无记录':new Date(value*1000).toISOString();
      const status={0:'未开播',1:'直播中',2:'轮播'};
      for(const row of data.rooms){
        const block=document.createElement('p');
        block.textContent=`${row.anchor} · 房间 ${row.room_id} · ${row.subscriber_count} 人订阅\n${row.error?'上次成功状态':'最近确认状态'}：${status[row.live_status]||'未知'}\n最近检查：${time(row.checked_at)} · 最近成功：${time(row.last_success_at)}${row.error?'\n检查异常：'+row.error:''}`;
        rooms.append(block);
      }
      const failures=byId('live-failures');failures.replaceChildren();
      if(!data.failures.length)failures.textContent='暂无永久投递失败。';
      for(const row of data.failures){const block=document.createElement('p');block.textContent=`通知目标 #${row.target_id} · 房间 ${row.room_id}：${row.error||'永久投递失败'}`;failures.append(block);}
      byId('live-message').textContent='直播状态已刷新；每两分钟检查一次。';
    }catch(error){byId('live-message').textContent='直播状态刷新失败，下方可能是上次取得的数据：'+error.message;}
    finally{liveLoading=false;}
  }
  refreshConfiguration();
  setInterval(refreshConfiguration, 2000);
  setInterval(() => { if (!byId('subscriptions').contains(document.activeElement)) refreshSubscriptions(); refreshLive(); }, 10000);
  refreshSubscriptions();
  refreshLive();
})();
