// 主题。**显式选择优先，没选过就跟随系统**；选择记在 localStorage，
// app.html 里在首帧前读回来——那段内联脚本和这里的 read() 必须是同一套判断。

export type Theme = 'dark' | 'light';

const KEY = 'ai-task.theme';

function read(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === 'light' || saved === 'dark') return saved;
  } catch {
    /* 隐私模式下读不了，往下走系统偏好 */
  }
  // 没选过：跟随系统。之前这里恒为 dark，浅色系统的人第一次打开会被强行塞一个暗色界面。
  try {
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
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
