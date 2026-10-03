(() => {
  const byId = id => document.getElementById(id);
  let subscriptionLoading = false;
  let subscriptionWrites = 0;
  let configData = null, intervalVersion = null, intervalDirty = false, configurationLoading = false, configurationBusy = false;
  let configurationSequence = 0;
  let repositoryRenderKey = null;
  const noteDrafts = new Map();
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
      const rows = await request('/api/admin/subscriptions');
      if (subscriptionWrites) return;
      const container = byId('subscriptions');
      container.replaceChildren();
      if (!rows.length) container.textContent = '尚无广播接收者。';
      for (const row of rows) {
        const block = document.createElement('div'); block.className = 'component';
        const identity = document.createElement('p');
        identity.textContent = `订阅 #${row.id} · ${row.user_openid} · QQ 号备注：${row.qq_number_note || '未填写'}`;
        const form = document.createElement('form');
        const noteLabel = document.createElement('label'); noteLabel.textContent = 'QQ 号备注（选填） ';
        const note = document.createElement('input'); note.value = noteDrafts.has(row.id) ? noteDrafts.get(row.id) : row.qq_number_note || ''; note.maxLength = 256;
        note.addEventListener('input', () => noteDrafts.set(row.id, note.value));
        noteLabel.append(note);
        const save = document.createElement('button'); save.textContent = '保存备注';
        const remove = document.createElement('button'); remove.type = 'button'; remove.textContent = '移除';
        form.append(noteLabel, save, remove);
        form.addEventListener('submit', event => {
          event.preventDefault(); operation(form, 'subscription-message', async () => {
            await request(`/api/admin/subscriptions/${row.id}`, 'PATCH', {qq_number_note:note.value.trim()});
            noteDrafts.delete(row.id);
            byId('subscription-message').textContent = '备注已保存。'; await refreshSubscriptions();
          });
        });
        remove.addEventListener('click', () => {
          if (!confirm(`移除 ${row.user_openid}？未发送通知将取消，备注将删除；正在发出的请求可能仍送达。对方可重新 /bind，旧积压不会恢复。`)) return;
          operation(form, 'subscription-message', async () => {
            await request(`/api/admin/subscriptions/${row.id}`, 'DELETE');
            noteDrafts.delete(row.id);
            byId('subscription-message').textContent = '已移除订阅。'; await refreshSubscriptions();
          });
        });
        block.append(identity, form); container.append(block);
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
      form.reset();
      byId('subscription-message').textContent = data.added ? '已添加；不补发历史通知。' : '已存在相同接收者，原备注保持不变。';
      await refreshSubscriptions();
    });
  });
  function showConfiguration(data) {
    configData = data;
    const stages = {saved:'已保存，等待热加载接受',accepted:'热加载已接受，等待当前任务结束后应用',applied:'监控任务已采用当前配置',rejected:'加载失败，继续使用上一份有效配置'};
    byId('configuration-stage').textContent = (stages[data.stage] || '未知') + `。当前实际轮询间隔：${data.applied.poll_interval_seconds} 秒。` + (data.error ? ' '+data.error : '');
    const form = byId('poll-interval');
    if (!intervalDirty && data.saved) { form.elements.seconds.value = data.saved.poll_interval_seconds; intervalVersion = data.version; }
    form.elements.seconds.disabled = !data.saved;
    form.querySelector('button').disabled = !data.saved || configurationBusy;
    if (intervalDirty && intervalVersion !== data.version) byId('configuration-message').textContent = '配置已变化，当前间隔草稿尚未保存；请刷新配置后重新编辑。';
    byId('add-repository').querySelector('button').disabled = !data.saved || configurationBusy;
    const renderKey = JSON.stringify([data.version, data.saved?.repositories, configurationBusy]);
    if (repositoryRenderKey === renderKey) return;
    repositoryRenderKey = renderKey;
    const container = byId('repositories'); container.replaceChildren();
    if (data.saved) for (const repository of data.saved.repositories) {
      const block = document.createElement('div'); block.className = 'component';
      const name = document.createElement('p'); name.textContent = repository;
      const remove = document.createElement('button'); remove.type = 'button'; remove.textContent = '移除仓库';
      remove.disabled = data.saved.repositories.length <= 1 || configurationBusy;
      if (data.saved.repositories.length <= 1) remove.title = '至少保留一个监控仓库';
      remove.addEventListener('click', () => {
        if (data.saved.repositories.length <= 1 || configurationBusy) return;
        if (!confirm(`移除 ${repository}？后续轮询将停止，已有通知继续投递；重新添加会补发停用期间的新建 Issue。`)) return;
        saveConfiguration(block, {version:data.version,repositories:data.saved.repositories.filter(name => name !== repository)});
      });
      block.append(name, remove); container.append(block);
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
    await operation(form, 'configuration-message', async () => {
      const data = await request('/api/admin/config', 'PATCH', body);
      if (body.poll_interval_seconds !== undefined) intervalDirty = false;
      showConfiguration(data);
      if (form === byId('add-repository')) form.reset();
      byId('configuration-message').textContent = '配置已保存；请查看上方的加载与应用状态。';
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
  byId('reload-configuration').addEventListener('click', () => { if (configurationBusy) return; intervalDirty = false; refreshConfiguration(); });
  byId('add-repository').addEventListener('submit', event => {
    event.preventDefault(); const form = event.currentTarget;
    if (!configData?.saved || configurationBusy) return;
    const repository = form.elements.repository.value.trim();
    if (!repository) return;
    if (configData.saved.repositories.some(name => name.toLowerCase() === repository.toLowerCase())) { byId('configuration-message').textContent = '已存在相同监控仓库。'; return; }
    saveConfiguration(form, {version:configData.version,repositories:[...configData.saved.repositories,repository]});
  });
  refreshConfiguration();
  setInterval(refreshConfiguration, 2000);
  setInterval(() => { if (!byId('subscriptions').contains(document.activeElement)) refreshSubscriptions(); }, 10000);
  refreshSubscriptions();
})();
