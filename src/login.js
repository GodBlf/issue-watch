(() => {
  const form=document.getElementById('login-form'),password=document.getElementById('password'),button=document.getElementById('login-submit'),message=document.getElementById('login-message');
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(button.disabled)return;
    button.disabled=true;message.textContent='正在登录…';
    try {
      const response=await fetch('/api/auth/login',{method:'POST',cache:'no-store',signal:AbortSignal.timeout(15000),headers:{'Content-Type':'application/json','X-Issue-Watch-Admin':'1'},body:JSON.stringify({password:password.value})});
      const data=await response.json();
      if(!response.ok)throw new Error(data.error||'登录失败，请稍后重试');
      password.value='';location.assign('/');
    } catch(error) { message.textContent=error.message||'连接失败，请稍后重试'; }
    finally { button.disabled=false; }
  });
})();
