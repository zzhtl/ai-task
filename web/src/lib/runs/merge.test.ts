import { describe, expect, test } from 'bun:test';
import { mergeFirstPage } from './merge';

const row = (id: string, status = 'running') => ({ id, status });

describe('轮询合并进已翻出来的列表', () => {
  test('翻出来的后几页不会被冲掉', () => {
    const current = [row('c'), row('b'), row('a')];
    // 第一页只有最近两条
    const merged = mergeFirstPage(current, [row('c'), row('b')]);
    expect(merged.map((r) => r.id)).toEqual(['c', 'b', 'a']);
  });

  test('已有的行换成新状态，新出现的插到最前', () => {
    const merged = mergeFirstPage([row('b'), row('a')], [row('c'), row('b', 'succeeded')]);
    expect(merged).toEqual([row('c'), row('b', 'succeeded'), row('a')]);
  });
});
