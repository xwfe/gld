# 事实与边界

- 本地服务端口与 Cloudflare 下发回源端口不一致，隧道注册成功仍返回 502。
- start 已有 --port；named 模式仅运行 cloudflared tunnel run --token。
- start/share/upgrade 共用 ensure_tunnel_up，可以在此补 named 公网检查。
- 保留 --tunnel-token 兼容别名；不增加云端配置 API 或凭据，不自动更改监听端口。
