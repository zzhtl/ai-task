// 主题。暗色是默认，浅色是可选项；选择记在 localStorage，app.html 里在首帧前读回来。

export type Theme = 'dark' | 'light';

const KEY = 'ai-task.theme';

function read(): Theme {
  try {
    return localStorage.getItem(KEY) === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

let current = $state<Theme>(read());

export const theme = {
  get current() {
    return current;
  },
  set(next: Theme) {
    current = next;
    if (next === 'light') document.documentElement.dataset.theme = 'light';
    else delete document.documentElement.dataset.theme;
    try {
      localStorage.setItem(KEY, next);
    } catch {
      /* 隐私模式下存不了，就只对这一次会话生效 */
    }
  },
  toggle() {
    theme.set(current === 'dark' ? 'light' : 'dark');
  }
};
