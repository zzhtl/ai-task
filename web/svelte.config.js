import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
export default {
  kit: {
    // 纯 SPA：产物由 Rust 二进制经 rust-embed 内嵌后 serve，没有 Node 运行时。
    // fallback 让所有前端路由（/runs/<id> 之类）都落回同一个入口。
    adapter: adapter({
      pages: '../crates/ai-task-server/dist',
      assets: '../crates/ai-task-server/dist',
      fallback: 'index.html',
      precompress: false,
      strict: false
    }),
    alias: {
      $api: 'src/lib/api'
    }
  }
};
