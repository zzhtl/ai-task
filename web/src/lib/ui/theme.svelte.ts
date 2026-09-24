// 主题。三选一：浅色、深色、跟随系统（默认）。
//
// 显式选择记在 localStorage；没记就跟随系统。app.html 里有一段在首帧前执行的
// 内联脚本读同一个键——两边的判断必须一致，否则浅色用户每次打开会先闪一下黑底。

export type Theme = 'dark' | 'light';
export type ThemePref = Theme | 'system';

const KEY = 'ai-task.theme';

function readPref(): ThemePref {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === 'light' || saved === 'dark') return saved;
  } catch {
    /* 隐私模式下读不了，按跟随系统处理 */
  }
  return 'system';
}

function systemIsLight(): boolean {
  try {
    return window.matchMedia('(prefers-color-scheme: light)').matches;
  } catch {
    return false;
  }
}

let pref = $state<ThemePref>(typeof window === 'undefined' ? 'system' : readPref());
let osLight = $state(typeof window === 'undefined' ? false : systemIsLight());

function resolved(): Theme {
  return pref === 'system' ? (osLight ? 'light' : 'dark') : pref;
}

function apply() {
  if (resolved() === 'light') document.documentElement.dataset.theme = 'light';
  else delete document.documentElement.dataset.theme;
}

// 跟随系统时，系统在白天/夜间自动切换，界面要跟着变，不用刷新
if (typeof window !== 'undefined') {
  try {
    window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', (event) => {
      osLight = event.matches;
      if (pref === 'system') apply();
    });
  } catch {
    /* 老浏览器没有 addEventListener，就不跟随了 */
  }
}

export const theme = {
  /** 用户选的：可能是"跟随系统"。 */
  get pref(): ThemePref {
    return pref;
  },
  /** 实际生效的那一个。 */
  get current(): Theme {
    return resolved();
  },
  set(next: ThemePref) {
    pref = next;
    apply();
    try {
      if (next === 'system') localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, next);
    } catch {
      /* 隐私模式下存不了，就只对这一次会话生效 */
    }
  },
  toggle() {
    theme.set(resolved() === 'dark' ? 'light' : 'dark');
  }
};
