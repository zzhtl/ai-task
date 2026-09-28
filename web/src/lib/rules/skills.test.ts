import { describe, expect, test } from 'bun:test';
import { filesBody, nextVersion } from './skills';

describe('nextVersion', () => {
  test('末尾是数字就加一', () => {
    expect(nextVersion('1')).toBe('2');
    expect(nextVersion('1.2')).toBe('1.3');
    expect(nextVersion('v9')).toBe('v10');
    expect(nextVersion(' 2024.09 ')).toBe('2024.10');
  });

  test('末尾不是数字就补 .1，空的从 1 开始', () => {
    expect(nextVersion('beta')).toBe('beta.1');
    expect(nextVersion('')).toBe('1');
  });

  test('很长的数字也不丢精度', () => {
    expect(nextVersion('20260924123456789012')).toBe('20260924123456789013');
  });
});

describe('filesBody', () => {
  test('空行丢掉，路径去空白', () => {
    expect(
      filesBody([
        { path: ' ref/a.md ', content: 'A' },
        { path: '', content: '' }
      ])
    ).toEqual({ files: { 'ref/a.md': 'A' } });
  });

  test('路径重复或缺路径时报错，而不是悄悄丢一个', () => {
    expect('error' in filesBody([{ path: 'a', content: '1' }, { path: 'a', content: '2' }])).toBe(true);
    expect('error' in filesBody([{ path: '', content: '有内容' }])).toBe(true);
  });
});
