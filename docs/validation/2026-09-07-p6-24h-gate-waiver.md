步骤：P6.2 24h soak **阻塞**（不是完成，不是豁免通过）
基线 commit：29ff9df（P6 代码/harness）
实现 commit / diff 标识：本阻塞记录

停止位置：P6.2 声明负载 24 小时持续运行。

事实：
- 86400s soak **未执行**。
- `MARKETFORGE_SOAK_SECONDS=8` 只验证了 `scripts/backend_soak.sh` 能启动并停机，**不是** 24h，也不得记为 24h 通过。
- 操作者要求不要跑 24h soak；本环境也没有 86400s 进程预算。
- 验证计划：若不能跑 24h，写入 soak-unavailable 并 **不要把 P6 标为完成**。

P6 已落地但不足以过门槛：
- 容量探针、低基数 `/metrics`
- 故障矩阵子集（DB 不可用、进程退出、客户端取消、租约/双实例、慢读、关停）
- postgres_smoke / postgres_multi_active_smoke
- 文档与 CI
- soak 脚本默认仍为 86400s

是否把 24h 项勾选为完成：否。
是否把 8s 当作 24h 门槛：否。
是否满足 P6 阶段门槛：否。
