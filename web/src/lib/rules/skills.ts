// 技能编辑的纯逻辑。

/**
 * 下一个版本号的建议值：末尾是数字就加一（"1"→"2"、"1.2"→"1.3"、"v3"→"v4"），否则补 ".1"。
 *
 * 只是建议：版本号是自由文本，服务端按"用过没有"判重，撞了会报在 version 字段上。
 */
export function nextVersion(version: string): string {
  const v = version.trim();
  const m = /^(.*?)(\d+)$/.exec(v);
  if (!m) return v ? `${v}.1` : '1';
  return `${m[1]}${(BigInt(m[2]) + 1n).toString()}`;
}

export interface FileRow {
  path: string;
  content: string;
}

/**
 * 附件行 → 报文里的 `files`。整行空着的丢掉（那是"加了一行还没填"）。
 * 路径重复时返回错误：对象里同名键只会留下最后一个，悄悄丢一个附件比报错糟得多。
 */
export function filesBody(rows: FileRow[]): { files: Record<string, string> } | { error: string } {
  const files: Record<string, string> = {};
  for (const row of rows) {
    const path = row.path.trim();
    if (!path && row.content === '') continue;
    if (!path) return { error: '有附件没写路径' };
    if (path in files) return { error: `附件路径 ${path} 重复了` };
    files[path] = row.content;
  }
  return { files };
}
