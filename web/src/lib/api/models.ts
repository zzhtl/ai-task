// 没走 ts-rs 的那几个接口的形状。
//
// 主机、定时、审批、规则、技能、用户、审计的响应体是 server 里的本地 struct，
// 不在 ai-task-proto 里——所以这里手写一份。以前每个页面各自 declare 一遍，
// 同一个 Host 在四个文件里有四种定义，改一个字段要找四处。

import { api } from './client';
import type { Page } from './types/Page';

export interface AiCli {
  name: string;
  path: string;
  version: string | null;
}

export interface Host {
  id: string;
  name: string;
  address: string;
  port: number;
  username: string;
  tags: string[];
  agent_version: string | null;
  ai_clis: AiCli[];
  /** `systemd` / `proc` / `none`；没探测过是 `null`。 */
  cgroup_mode: string | null;
  /** 这一档下资源上限根本没被强制。 */
  degraded: boolean;
  last_seen_at: string | null;
}

export interface Schedule {
  id: string;
  task_id: string;
  cron: string;
  timezone: string;
  misfire: string;
  overlap: string;
  jitter_s: number;
  enabled: boolean;
  next_fire_at: string | null;
  last_fired_at: string | null;
  /** 接下来三次触发，按该时区的本地时间给的字符串。 */
  next_three: string[];
}

export interface Approval {
  id: string;
  run_id: string;
  node_key: string | null;
  title: string;
  intent: Record<string, unknown>;
  rule_id: string | null;
  requested_at: string;
  expires_at: string;
  /** 服务端算好的剩余秒数。以它为准，别拿本地时钟去减 expires_at。 */
  expires_in_s: number;
}

export interface Rule {
  id: string;
  name: string;
  kind: 'prompt' | 'policy';
  scope: 'global' | 'task';
  spec: Record<string, unknown>;
  priority: number;
  enabled: boolean;
  created_at: string;
}

export interface Skill {
  name: string;
  version: string;
  description: string;
  content_hash: string;
}

export type Role = 'viewer' | 'operator' | 'admin';

export interface User {
  id: string;
  email: string;
  display_name: string;
  role: Role;
  disabled: boolean;
  active_sessions: number;
  created_at: string;
  /** 首次部署创建的内置管理员：不能删、不能降级停用，资料只能由本人改。 */
  system: boolean;
}

export interface AuditItem {
  id: number;
  /** `null` = 系统自身的动作（调度器触发、超时自动拒绝）。 */
  actor: string | null;
  action: string;
  target_kind: string;
  target_id: string;
  before: unknown;
  after: unknown;
  request_id: string | null;
  ts: string;
}

/**
 * 只取 items。
 *
 * **这会丢掉 `next_cursor`**——在服务端给这些接口补上真游标之前，调用方本来也用不上；
 * 但别忘了这一层在：等后端能分页了，这里就是那个"为什么翻不了下一页"的地方。
 */
const items = <T>(p: Promise<Page<T>>) => p.then((page) => page.items);

/** 主机是 admin 才能读。operator 页面上读不到时不该报错，调用方自己 catch。 */
export const listHosts = () => items(api<Page<Host>>('/api/v1/hosts'));
export const listSchedules = (taskId?: string) =>
  items(api<Page<Schedule>>(taskId ? `/api/v1/schedules?task_id=${taskId}` : '/api/v1/schedules'));
export const listApprovals = () => items(api<Page<Approval>>('/api/v1/approvals'));
export const listRules = () => items(api<Page<Rule>>('/api/v1/rules'));
export const listSkills = () => items(api<Page<Skill>>('/api/v1/skills'));
export const listUsers = () => items(api<Page<User>>('/api/v1/users'));
export const listAudit = (limit = 200) => items(api<Page<AuditItem>>(`/api/v1/audit?limit=${limit}`));
