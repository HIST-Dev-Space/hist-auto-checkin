import { showError } from './common/dom.js';

const pages = {
  login: () => import('./pages/login.js'),
  tasks: () => import('./pages/tasks.js'),
  notifications: () => import('./pages/notifications.js'),
  account: () => import('./pages/account.js'),
};

const loadPage = pages[document.body.dataset.page];
if (loadPage) {
  loadPage()
    .then((module) => module.init())
    .catch((error) => {
      if (error.name === 'AbortError') return;
      const selector = document.body.dataset.page === 'login' ? '#login-error' : '#page-error';
      showError(selector, error.message);
    });
}
