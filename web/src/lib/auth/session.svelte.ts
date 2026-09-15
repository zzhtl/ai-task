// 当前登录身份。
//
// 会话 token 在 HttpOnly cookie 里，JS 读不到也不该读——这里只记"我是谁、
// 能做什么"，用来决定跳不跳登录页、哪些按钮该灰掉。

import { api, ApiFailure, setUnauthorizedHandler } from '$api/client';

export interface Identity {
  /** 未登录探测路径下拿不到，所以是可空的。 */
  email: string | null;
  display_name: string;
  role: 'viewer' | 'operator' | 'admin';
}

/** `null` = 未登录；`undefined` = 还没问过。 */
let identity = $state<Identity | null | undefined>(undefined);
/** 后端没开认证时为 true：所有人都是 admin，不显示登录页。 */
let authDisabled = $state(false);

export const session = {
  get identity() {
    return identity;
  },
  get authDisabled() {
    return authDisabled;
  },
  /** 够不够 `needed` 这一档。角色是包含关系。 */
  can(needed: Identity['role']): boolean {
    if (authDisabled) return true;
    const order = { viewer: 0, operator: 1, admin: 2 };
    return identity !== null && identity !== undefined && order[identity.role] >= order[needed];
  }
};

// 任何一个接口报 401（登录探测除外）都说明会话没了。把身份清空，
// LoginGate 会立刻切回登录页——不用整页重载，编辑器里没保存的内容还在。
setUnauthorizedHandler(() => {
  identity = null;
});

export async function refreshIdentity(): Promise<void> {
  try {
    identity = await api<Identity>('/api/v1/auth/me', { expected401: true });
  } catch (e) {
    if (e instanceof ApiFailure && e.status === 401) {
      // 401 可能是"没开认证所以没有身份"，也可能是"该登录了"。
      // 探一个受保护的只读接口来分辨：通了就是没开认证。
      try {
        await api('/api/v1/tasks?limit=1', { expected401: true });
        authDisabled = true;
        identity = null;
      } catch {
        authDisabled = false;
        identity = null;
      }
      return;
    }
    identity = null;
  }
}

export async function login(email: string, password: string): Promise<void> {
  identity = await api<Identity>('/api/v1/auth/login', {
    method: 'POST',
    body: { email, password },
    // 口令不对就是 401，那是这个接口的正常答案，不是会话过期
    expected401: true
  });
}

export async function logout(): Promise<void> {
  await api('/api/v1/auth/logout', { method: 'POST' });
  identity = null;
}

export async function bootstrap(
  email: string,
  password: string,
  displayName: string
): Promise<void> {
  await api('/api/v1/auth/bootstrap', {
    method: 'POST',
    body: { email, password, display_name: displayName }
  });
  await login(email, password);
}
