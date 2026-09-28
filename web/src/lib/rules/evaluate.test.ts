import { describe, expect, test } from 'bun:test';
import { argFor, buildInput } from './evaluate';

describe('argFor', () => {
  test('常见工具按模型真实发出的参数名', () => {
    expect(argFor('Bash')).toBe('command');
    expect(argFor('Write')).toBe('file_path');
    expect(argFor('remote_read')).toBe('path');
  });

  test('带 MCP 前缀的远端工具和服务端一样归一', () => {
    expect(argFor('mcp__ai_task_remote__remote_bash')).toBe('command');
  });

  test('认不出的工具不猜', () => {
    expect(argFor('mcp__somebody__do_it')).toBeNull();
    expect(argFor('')).toBeNull();
  });
});

describe('buildInput', () => {
  test('字段模式把值放进主参数', () => {
    expect(buildInput('Bash', 'field', 'rm -rf /', '')).toEqual({ input: { command: 'rm -rf /' } });
  });

  test('空值是"这个工具的任意调用"', () => {
    expect(buildInput('Read', 'field', '', '')).toEqual({ input: {} });
  });

  test('认不出的工具在字段模式下报错，而不是塞进 command 里', () => {
    expect('error' in buildInput('mcp__x__y', 'field', 'a', '')).toBe(true);
  });

  test('原始 JSON 原样解析', () => {
    expect(buildInput('Edit', 'json', '', '{"file_path": "/etc/hosts", "old_string": "a"}')).toEqual({
      input: { file_path: '/etc/hosts', old_string: 'a' }
    });
    expect(buildInput('Edit', 'json', '', '  ')).toEqual({ input: {} });
  });

  test('原始 JSON 必须是对象', () => {
    for (const bad of ['[1]', '"rm"', 'null', '{oops']) {
      expect('error' in buildInput('Bash', 'json', '', bad)).toBe(true);
    }
  });
});
