import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// 后端不在默认端口时（比如本机常驻实例、临时预览实例）用它指过去，
// 不必为了连另一个端口改这份文件。
const backend = process.env.AI_TASK_DEV_BACKEND ?? 'http://127.0.0.1:8930';

export default defineConfig({
  plugins: [sveltekit()],
  server: {
    port: 5173,
    // 开发期前后端同源：/api 与 /events 直接代理到后端，不依赖 CORS。
    // 生产是同一个二进制 serve，本来就同源。
    proxy: {
      '/api': { target: backend, changeOrigin: false },
      // SSE 必须关掉代理层的缓冲，否则事件会攒够一块才吐出来
      '/internal': { target: backend, changeOrigin: false }
    }
  }
});
