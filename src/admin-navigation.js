(() => {
  const views = [...document.querySelectorAll('[data-view]')];
  const links = [...document.querySelectorAll('[data-target]')];
  let current;
  const descriptions = {
    overview: '掌握监控、连接与通知投递的当前状态。',
    repositories: '管理共享监控仓库，以及值得持续关注的 Issue。',
    subscriptions: '为每个通知目标配置仓库与直播订阅。',
    bilibili: '查看共享直播间的最近状态与通知投递情况。',
    permissions: '分别管理添加与取消共享 Issue 追踪的权限。',
    settings: '调整检查频率，查看配置的实际应用状态。'
  };
  let currentHash = location.hash;
  function dirty() { return document.querySelector('[data-dirty="true"]') || window.issueWatchHasDrafts?.(); }
  function show(name) {
    if (!views.some(view => view.dataset.view === name)) name = 'overview';
    if (current && current !== name) window.scrollTo?.(0, 0);
    current = name;
    views.forEach(view => { view.hidden = view.dataset.view !== name; });
    links.forEach(link => link.setAttribute('aria-current', link.dataset.target === name ? 'page' : 'false'));
    const title = links.find(link => link.dataset.target === name)?.textContent?.trim() || '概览';
    const update = (id, text) => { const node = document.getElementById?.(id); if (node) node.textContent = text; };
    update('page-title', title);
    update('breadcrumb-current', title);
    update('page-description', descriptions[name]);
    document.title = `${title} · Issue Watch`;
  }
  links.forEach(link => link.addEventListener('click', event => {
    event.preventDefault();
    if (link.dataset.target === current) return;
    if (dirty() && !confirm('有尚未保存的输入。切换页面？输入会保留，尚未提交。')) return;
    history.pushState(null, '', '#'+link.dataset.target);
    currentHash = location.hash;
    show(link.dataset.target);
  }));
  function route() {
    if (location.hash === currentHash) return;
    if (dirty() && !confirm('有尚未保存的输入。切换页面？输入会保留，尚未提交。')) {
      history.replaceState(null, '', currentHash || '#overview');
      return;
    }
    currentHash = location.hash;
    show(location.hash.slice(1).split('?')[0]);
  }
  window.addEventListener('popstate', route);
  window.addEventListener('hashchange', route);
  window.addEventListener('beforeunload', event => {
    if (dirty()) { event.preventDefault(); event.returnValue = ''; }
  });
  show(location.hash.slice(1).split('?')[0]);
})();
