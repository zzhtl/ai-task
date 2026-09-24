import { describe, expect, test } from 'bun:test';
import { describeCron, fromCron, toCron, type CronBuilder } from './cron';

describe('构建器 → 表达式 → 构建器', () => {
  test.each<[CronBuilder, string]>([
    [{ mode: 'daily', time: '02:00' }, '0 2 * * *'],
    [{ mode: 'daily', time: '23:45' }, '45 23 * * *'],
    [{ mode: 'weekly', days: [1, 2, 3, 4, 5], time: '09:00' }, '0 9 * * 1-5'],
    [{ mode: 'weekly', days: [1, 3, 5], time: '09:30' }, '30 9 * * 1,3,5'],
    [{ mode: 'weekly', days: [0, 6], time: '10:00' }, '0 10 * * 0,6'],
    [{ mode: 'monthly', day: 1, time: '03:00' }, '0 3 1 * *'],
    [{ mode: 'monthly', day: 'last', time: '22:00' }, '0 22 L * *'],
    [{ mode: 'interval', every: 5, unit: 'minute' }, '*/5 * * * *'],
    [{ mode: 'interval', every: 1, unit: 'minute' }, '* * * * *'],
    [{ mode: 'interval', every: 1, unit: 'hour' }, '0 * * * *'],
    [{ mode: 'interval', every: 6, unit: 'hour' }, '0 */6 * * *'],
    [{ mode: 'interval', every: 30, unit: 'second' }, '*/30 * * * * *']
  ])('%o ⇄ %s', (builder, expr) => {
    expect(toCron(builder)).toBe(expr);
    expect(fromCron(expr)).toEqual(builder);
  });

  test('星期乱序、重复也写成同一个表达式', () => {
    expect(toCron({ mode: 'weekly', days: [5, 1, 3, 1], time: '09:00' })).toBe('0 9 * * 1,3,5');
  });

  test('还没填完的不生成表达式，免得被当成"每天"存进去', () => {
    expect(toCron({ mode: 'weekly', days: [], time: '09:00' })).toBe('');
    expect(toCron({ mode: 'daily', time: '' })).toBe('');
  });

  test('周日写成 7 也认', () => {
    expect(fromCron('0 8 * * 7')).toEqual({ mode: 'weekly', days: [0], time: '08:00' });
  });

  test('七天全选就是每天', () => {
    expect(fromCron('0 8 * * 0-6')).toEqual({ mode: 'daily', time: '08:00' });
  });
});

describe('认不出的原样留着', () => {
  // 猜错一个表达式的意思比不翻译更糟：这些都落到自定义
  test.each([
    '0 9 * * MON-FRI', // 名字写法
    '*/7 * * * *', // 7 分钟除不尽 60，到整点会重来，不是真正的"每 7 分钟"
    '0 */5 * * *', // 同上，到零点重来
    '0 9 1 1 *', // 指定了月份
    '0 9 1 * 1', // 日和星期同时写，cron 的语义是"或"
    '15,45 * * * *',
    '0 9 * * 1#2',
    'nope'
  ])('%s', (expr) => {
    expect(fromCron(expr)).toEqual({ mode: 'custom', expr });
    expect(describeCron(expr)).toBe(expr);
  });
});

describe('翻成人话', () => {
  test.each([
    ['0 2 * * *', '每天 02:00'],
    ['0 9 * * 1-5', '工作日 09:00'],
    ['0 10 * * 0,6', '周末 10:00'],
    ['30 9 * * 1,3,5', '每周一、三、五 09:30'],
    ['0 8 * * 0', '每周日 08:00'],
    ['0 3 1 * *', '每月 1 号 03:00'],
    ['0 22 L * *', '每月最后一天 22:00'],
    ['*/5 * * * *', '每 5 分钟'],
    ['* * * * *', '每分钟'],
    ['0 * * * *', '每小时'],
    ['0 */6 * * *', '每 6 小时'],
    ['*/30 * * * * *', '每 30 秒'],
    ['  0 2 * * *  ', '每天 02:00']
  ])('%s → %s', (expr, text) => {
    expect(describeCron(expr)).toBe(text);
  });
});
