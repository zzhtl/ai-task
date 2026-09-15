// fetch 封装的边界行为。之前这个文件零覆盖，而它是所有错误显示的源头。

import { describe, expect, test } from 'bun:test';
import { ApiFailure, describeError, fieldErrors, ignoreForbidden } from './client';

const failure = (status: number, body: Partial<ApiFailure['body']> = {}) =>
  new ApiFailure(status, {
    code: 'validation_failed',
    message: '请求校验未通过',
    request_id: 'req-1',
    details: [],
    ...body
  } as ApiFailure['body']);

describe('fieldErrors', () => {
  test('按字段拆开 422 的 details', () => {
    const e = failure(422, {
      details: [
        { field: 'name', code: 'required', message: '不能为空' },
        { field: 'port', code: 'range', message: '超出范围' }
      ]
    });
    expect(fieldErrors(e)).toEqual({ name: '不能为空', port: '超出范围' });
  });

  test('同一字段多条时只留第一条', () => {
    const e = failure(422, {
      details: [
        { field: 'name', code: 'required', message: '不能为空' },
        { field: 'name', code: 'too_long', message: '太长了' }
      ]
    });
    expect(fieldErrors(e)).toEqual({ name: '不能为空' });
  });

  test('网络错误不是字段问题，返回空', () => {
    expect(fieldErrors(new Error('boom'))).toEqual({});
    expect(fieldErrors(failure(500, { details: [] }))).toEqual({});
  });
});

describe('describeError', () => {
  test('有 details 时一次列全，而不是只显示 message', () => {
    const e = failure(422, {
      details: [
        { field: 'name', code: 'required', message: '不能为空' },
        { field: 'port', code: 'range', message: '超出范围' }
      ]
    });
    expect(describeError(e)).toBe('name：不能为空；port：超出范围');
  });

  test('没有 details 时退回 message', () => {
    expect(describeError(failure(409, { message: '重名了' }))).toBe('重名了');
  });

  test('普通 Error 也要能显示', () => {
    expect(describeError(new Error('断网了'))).toBe('断网了');
  });
});

describe('ignoreForbidden', () => {
  test('403 / 401 吞掉', () => {
    expect(() => ignoreForbidden(failure(403))).not.toThrow();
    expect(() => ignoreForbidden(failure(401))).not.toThrow();
  });

  test('500、超时、断网照常抛——这正是之前 .catch(() => {}) 吞掉的东西', () => {
    expect(() => ignoreForbidden(failure(500))).toThrow();
    expect(() => ignoreForbidden(new Error('timeout'))).toThrow();
  });
});
