(() => {
  const views = [...document.querySelectorAll('[data-view]')];
  const links = [...document.querySelectorAll('[data-target]')];
  let current;
  let currentHash = location.hash;
  function dirty() { return document.querySelector('[data-dirty="true"]') || window.issueWatchHasDrafts?.(); }
  function show(name) {
    if (!views.some(view => view.dataset.view === name)) name = 'overview';
    current = name;
    views.forEach(view => { view.hidden = view.dataset.view !== name; });
    links.forEach(link => link.setAttribute('aria-current', link.dataset.target === name ? 'page' : 'false'));
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
