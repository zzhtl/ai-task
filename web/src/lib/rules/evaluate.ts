// 规则页"试算"面板的纯逻辑：把人填的东西拼成一次工具调用的 `input`。
//
// 服务端拿它和真实调用走同一个判决函数，所以这里拼出来的形状必须和模型发出的
// `tool_input` 一致——Bash 的命令在 `command` 里、文件类工具的路径在 `file_path` 里。

import type { JsonValue } from '$api/types/serde_json/JsonValue';

/** 常见工具的"主参数"：规则通常就是拿它去匹配的。 */
const ARG_FOR_TOOL: Record<string, string> = {
  Bash: 'command',
  remote_bash: 'command',
  Read: 'file_path',
  Write: 'file_path',
  Edit: 'file_path',
  remote_read: 'path',
  remote_write: 'path',
  Glob: 'pattern',
  Grep: 'pattern',
  remote_glob: 'pattern',
  remote_grep: 'pattern',
  WebFetch: 'url',
  WebSearch: 'query'
};

/** 这个工具的主参数名。认不出的工具返回 null：调用方该让人直接写 JSON。 */
export function argFor(tool: string): string | null {
  const bare = tool.trim().replace(/^mcp__ai_task_remote__/, '');
  return ARG_FOR_TOOL[bare] ?? null;
}

export type InputMode = 'field' | 'json';

/** 模型发出的 `tool_input` 永远是一个 JSON 对象。 */
export type ToolInput = { [key: string]: JsonValue };

export type BuiltInput = { input: ToolInput } | { error: string };

/**
 * 表单 → `input`。
 *
 * - `field`：只填主参数的值；空值就是空对象（"这个工具的任意调用"）
 * - `json`：原样解析，必须是对象——模型发出的 `tool_input` 永远是对象
 */
export function buildInput(tool: string, mode: InputMode, value: string, raw: string): BuiltInput {
  if (mode === 'json') {
    const text = raw.trim();
    if (!text) return { input: {} };
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch (e) {
      return { error: `JSON 写得不对：${(e as Error).message}` };
    }
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return { error: '要写成一个 JSON 对象，比如 {"command": "ls"}' };
    }
    return { input: parsed as ToolInput };
  }
  const arg = argFor(tool);
  if (!arg) return { error: '认不出这个工具的参数名，换成"原始 JSON"写' };
  return { input: value === '' ? {} : { [arg]: value } };
}
