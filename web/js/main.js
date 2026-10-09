// 入口：登录态 → 应用启动；页面切换（幕帘过渡，取自 design/app.html）。

import { api, loadAuth, saveAuth, clearAuth, setUnauthorizedHandler, ApiError } from './lib/api.js';
import { TasksStore } from './tasks-store.js';
import { HomePage } from './home.js';
import { TasksPage } from './tasks.js';
import { SettingsPage } from './settings.js';

// ---------------------------------------------------------------- 页面切换（幕帘，逐行取自设计稿）

const Nav = (() => {
  const ORDER = ['home', 'tasks', 'settings'];
  const D = 680;          /* 幕帘总时长 ms */
  const COVER = 300;      /* 幕帘完全遮蔽时刻（扫入快） */
  const FADE_DELAY = 380; /* 新页淡入开始 = 遮蔽 + 80ms（与旧页淡出错开） */
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');

  const body = document.body;
  const curtain = document.getElementById('curtain');
  const edgeL = curtain.querySelector('.edge.l');
  const edgeR = curtain.querySelector('.edge.r');
  const indicator = document.getElementById('rail-ind');
  const railItems = Array.prototype.slice.call(document.querySelectorAll('.rail-item'));
  const pages = {};
  ORDER.forEach((n) => { pages[n] = document.querySelector('.page-' + n); });

  let current = 'home';
  let switching = false;

  function validPage(n) { return ORDER.indexOf(n) !== -1; }

  /* rail 指示条：唯一在切换时滑动的 rail 元素 */
  function moveIndicator(name, animate) {
    let item = null;
    railItems.forEach((el) => {
      const on = el.dataset.page === name;
      el.classList.toggle('active', on);
      if (on) item = el;
    });
    if (!item) return;
    if (!animate) indicator.classList.add('instant');
    indicator.style.height = (item.offsetHeight * 0.56) + 'px';
    indicator.style.transform = 'translateY(' + (item.offsetTop + item.offsetHeight * 0.22) + 'px)';
    if (!animate) {
      void indicator.offsetWidth; /* 强制回流后恢复过渡 */
      indicator.classList.remove('instant');
    }
  }

  function activate(name) {
    ORDER.forEach((n) => {
      pages[n].classList.toggle('active', n === name);
      pages[n].classList.remove('t-out', 't-in');
    });
    current = name;
    if (location.hash !== '#' + name) history.replaceState(null, '', '#' + name);
  }

  /* 幕帘 + 双页微动的整组动画 */
  function buildAnims(from, to, dir) {
    const off = (v) => `translateX(${v * dir}%)`;
    const anims = [];
    /* 幕帘：扫入快，遮蔽一拍，扫出稍缓 */
    anims.push(curtain.animate([
      { transform: off(-102), offset: 0, easing: 'cubic-bezier(.7,0,.3,1)' },
      { transform: 'translateX(0%)', offset: 0.44, easing: 'linear' },
      { transform: 'translateX(0%)', offset: 0.47, easing: 'cubic-bezier(.33,.46,.32,1)' },
      { transform: off(102), offset: 1 },
    ], { duration: D, fill: 'both' }));
    /* 1px 朱红前缘线：扫入在前缘，遮蔽瞬间换到扫出侧 */
    const lead = dir > 0 ? edgeR : edgeL;
    const trail = dir > 0 ? edgeL : edgeR;
    anims.push(lead.animate([
      { opacity: 1, offset: 0 }, { opacity: 1, offset: 0.45 },
      { opacity: 0, offset: 0.46 }, { opacity: 0, offset: 1 },
    ], { duration: D, fill: 'both' }));
    anims.push(trail.animate([
      { opacity: 0, offset: 0 }, { opacity: 0, offset: 0.45 },
      { opacity: 1, offset: 0.46 }, { opacity: 1, offset: 1 },
    ], { duration: D, fill: 'both' }));
    /* 幕帘之下：旧页 3px 位移淡出，新页错开 80ms 淡入 */
    const dx = (3 * dir) + 'px';
    anims.push(pages[from].animate([
      { opacity: 1, transform: 'translateX(0)' },
      { opacity: 0, transform: `translateX(${dx})` },
    ], { duration: 280, easing: 'ease-out', fill: 'both' }));
    anims.push(pages[to].animate([
      { opacity: 0, transform: `translateX(${dx})` },
      { opacity: 1, transform: 'translateX(0)' },
    ], { duration: 240, delay: FADE_DELAY, easing: 'ease-out', fill: 'both' }));
    return anims;
  }

  function transition(to) {
    if (!validPage(to) || to === current || switching) return;
    const from = current;
    if (reduced.matches) { fadeTransition(from, to); return; }
    switching = true;
    const dir = ORDER.indexOf(to) > ORDER.indexOf(from) ? 1 : -1;
    body.classList.add('switching');
    pages[from].classList.add('t-out');
    pages[to].classList.add('t-in');
    const anims = buildAnims(from, to, dir);
    /* 幕帘遮蔽下换页，rail 指示条此时滑向新页 */
    setTimeout(() => {
      pages[from].classList.remove('active');
      pages[to].classList.add('active');
      moveIndicator(to, true);
    }, COVER);
    setTimeout(() => {
      anims.forEach((a) => a.cancel());
      body.classList.remove('switching');
      pages[from].classList.remove('t-out');
      pages[to].classList.remove('t-in');
      activate(to);
      switching = false;
    }, D + 40);
  }

  /* prefers-reduced-motion：降级为简单淡入淡出，无幕帘无位移 */
  function fadeTransition(from, to) {
    switching = true;
    body.classList.add('switching');
    pages[from].classList.add('t-out');
    pages[to].classList.add('t-in');
    moveIndicator(to, true);
    const a1 = pages[from].animate([{ opacity: 1 }, { opacity: 0 }], { duration: 220, fill: 'both' });
    const a2 = pages[to].animate([{ opacity: 0 }, { opacity: 1 }], { duration: 220, fill: 'both' });
    setTimeout(() => {
      a1.cancel(); a2.cancel();
      body.classList.remove('switching');
      pages[from].classList.remove('t-out');
      pages[to].classList.remove('t-in');
      activate(to);
      switching = false;
    }, 240);
  }

  railItems.forEach((el) => {
    el.addEventListener('click', () => transition(el.dataset.page));
  });
  window.addEventListener('hashchange', () => {
    const n = location.hash.slice(1);
    if (validPage(n)) transition(n);
  });

  return {
    switchPage: transition,
    init() {
      const hashPage = location.hash.slice(1);
      const initial = validPage(hashPage) ? hashPage : 'home';
      activate(initial);
      moveIndicator(initial, false);
    },
  };
})();

// ---------------------------------------------------------------- 登录 / 启动

const loginEl = document.getElementById('login');
const appEl = document.getElementById('app');

function showLogin() {
  loginEl.hidden = false;
  appEl.style.visibility = 'hidden';
}

async function enterApp(auth) {
  loginEl.hidden = true;
  appEl.style.visibility = '';
  document.getElementById('user-name').textContent = auth.user?.username || '';
  document.getElementById('user-avatar').textContent = (auth.user?.username || '?').slice(0, 1).toUpperCase();
  Nav.init();
  HomePage.start();
  SettingsPage.start();
}

document.getElementById('login-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const errEl = document.getElementById('login-err');
  const btn = document.getElementById('login-btn');
  errEl.textContent = '';
  btn.disabled = true;
  try {
    const username = document.getElementById('login-user').value.trim();
    const password = document.getElementById('login-pass').value;
    const auth = await api('/api/auth/login', { method: 'POST', body: { username, password }, skip401: true });
    saveAuth(auth);
    await enterApp(auth);
  } catch (err) {
    errEl.textContent = err instanceof ApiError && err.status === 401 ? '用户名或密码不正确' : err.message;
  } finally {
    btn.disabled = false;
  }
});

document.getElementById('logout').addEventListener('click', () => {
  clearAuth();
  location.reload();
});

setUnauthorizedHandler(showLogin);
TasksStore.setUnauthorizedHandler(showLogin);

// 模块初始化（数据启动在登录成功后）
HomePage.init({ switchPage: Nav.switchPage, focusTask: (key) => TasksPage.focusTask(key) });
TasksPage.init({ switchPage: Nav.switchPage });
SettingsPage.init();

// 启动：先验证本地登录态
(async () => {
  const auth = loadAuth();
  if (!auth) { showLogin(); return; }
  try {
    await api('/api/me');
    await enterApp(auth);
  } catch {
    showLogin();
  }
})();
