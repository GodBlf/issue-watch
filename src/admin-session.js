(() => {
  const originalFetch=window.fetch.bind(window);
  window.fetch=async(...args)=>{
    const response=await originalFetch(...args);
    if(response.status===401){
      document.getElementById('session-expired').hidden=false;
      const message='登录已失效，请重新登录；未确认结果的操作请在登录后刷新核实。';
      document.getElementById('session-message').textContent=message;
      throw new Error(message);
    }
    return response;
  };
  const button=document.getElementById('logout');
  button.addEventListener('click',async()=>{
    if(button.disabled)return;button.disabled=true;
    try{
      const response=await window.fetch('/api/auth/logout',{method:'POST',cache:'no-store',signal:AbortSignal.timeout(10000),headers:{'X-Issue-Watch-Admin':'1'}});
      if(!response.ok)throw new Error('退出失败，请稍后重试');
      location.assign('/login');
    }catch(error){document.getElementById('session-expired').hidden=false;document.getElementById('session-message').textContent=error.message;}
    finally{button.disabled=false;}
  });
})();
