步骤：P6.2 24h soak 门槛豁免（不是完成）
基线 commit：29ff9df（P6 功能提交）
实现 commit / diff 标识：本豁免记录

操作者指令（原文）：
「不需要跑24h soak。继续完成剩余部分」

本文件的作用：
- **明确豁免 P6 的 24 小时声明负载门槛。**
- 86400s soak **未执行**。
- `MARKETFORGE_SOAK_SECONDS=8` 只验证了 `scripts/backend_soak.sh` 能启动并停机，**不是** 24h，也不得记为 24h 通过。
- `scripts/backend_soak.sh` 默认仍是 86400s，留给后续有长时主机的环境。

P6 其余已验收且仍成立：
- 容量探针、低基数 `/metrics`
- 故障矩阵（DB 不可用、进程退出、客户端取消、租约/双实例、慢读、关停）
- postgres_smoke / postgres_multi_active_smoke
- 文档与 CI

是否把 24h 项勾选为完成：否。
是否把 8s 当作 24h 门槛：否。
是否满足已豁免后的本轮 P6 门槛：是。
