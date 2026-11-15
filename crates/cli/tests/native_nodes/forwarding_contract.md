# D18-C 原生节点转发验收清单

D18-A 明确拒绝旧的原生节点控制协议和调用方 metadata。TLS 会话、固定 Hub 公钥证明和机器凭据验证成功，仅证明传输对端身份；它们不授予项目发现、文件访问、命令执行或委派权限。本目录当前可执行的测试验证这一拒绝边界，并独立验证配对、机器凭据、本机配置和进程生命周期。

下面依赖旧授权的转发成功场景已从 A 的可执行集成用例中退役。它们与 RFC-0015 §7.1 一起构成 **D18-C 必须恢复的验收覆盖**，应使用新的已验证调用方与受项目范围约束的转发协议。A 的拒绝测试通过，不代表这些正向场景已经完成。不得恢复伪 grant、网络请求冒充 LocalOperator、旧 metadata，或用伪造 HTTP 授权成功的夹具使测试通过。

## 必验行为

| 范围 | 必须恢复的正向行为及失败边界 |
| --- | --- |
| 项目路由 | 两台机器上的同名项目拥有独立、稳定的句柄。只发布明确共享的项目；移除共享后关闭对应路由；离线或已撤销节点不得回退到 Hub 本机同名项目。直连和全局入口共用机器连接登记。 |
| 调用权限 | 可信转发契约携带已认证调用方及有效项目权限。修改请求体 metadata 或 arguments 均不能扩大范围。文件写入与命令权限取 grant、目标机共享配置及项目策略的交集。 |
| 连接归属 | 每台机器只有一条已认证连接；直连与全局入口之间的重复连接被拒绝。执行代切换保留宿主持有的活动 socket；Hub 重启后重新连接，项目句柄保持稳定。 |
| 断线恢复 | 同一个节点进程从初始及运行中的网络故障恢复，不并行占用多个 socket、不重放命令、不丢失会话归属、不返回其他调用方的输出。 |
| 回包与并发 | 约 3 MB 图片只返回一份；超限结果只令当前调用失败；超过 30 秒的回包仍能到达；并发读取按连接串行；排在卡住调用后面的请求在有界等待后返回 Busy。 |
| 取消与停机 | 撤销机器与停止 daemon 能及时切断卡住的调用。结果不确定的操作不得重放。 |
| 文件写入 | 显式目标写权限、共享替换和多机器之间保持隔离。补丁预检、文件版本前置条件、原子性、路径逃逸、符号链接和私有配置边界在完整转发链中成立。 |
| 命令会话 | 目标机命令开关和项目命令策略同时生效。会话保持调用方及项目归属；read_output、write_stdin、kill_session 只能访问原调用方的对应会话。 |
| 节点进程生命周期 | 撤销、正常停止和 SIGHUP 收回节点启动的命令并留下准确记录。服务模式遇到 HTTP 401 时先停命令，再继续重试传输。SIGKILL 后记录不确定结果；nohup 保留其预期的信号行为。 |

## 旧测试定位

以下测试名可在基线 `05bec55` 的历史版本中定位原场景。部分旧测试混合了准备步骤、拒绝断言和转发成功链；可独立成立的准备步骤及拒绝边界已用更准确的名称保留在 A 中。

### `native_nodes.rs`

- `native_nodes_route_real_projects_through_authenticated_wss_and_global_gateway`
- `node_process_recovers_initial_and_live_network_outages_without_parallel_connections`
- `a_three_megabyte_image_comes_back_through_the_node_as_one_image_block`
- `an_oversized_node_result_fails_only_that_call_and_the_node_stays_online`
- `a_reply_slower_than_thirty_seconds_still_arrives_on_the_same_connection`
- `revoking_a_machine_cuts_a_stuck_call_instead_of_waiting_for_it`
- `stopping_the_daemon_is_not_held_up_by_a_stuck_call`
- `parallel_reads_wait_for_the_connection_instead_of_failing`
- `a_call_blocked_behind_a_stuck_one_gets_busy_after_a_short_wait`
- `removing_a_revoked_machine_clears_it_from_the_hub_and_frees_its_name` 与 `node_credentials_revoke_and_rotate_active_connections_without_changing_routes` 中的活动路由部分。

### `commands.rs`

- `commands_run_only_where_the_target_allows_them_and_a_disconnect_does_not_replay_them`
- `a_machine_without_any_command_project_explains_how_to_enable_them`
- `revoking_a_machine_stops_the_commands_it_was_running`
- `a_service_stops_its_commands_on_a_401_even_though_it_keeps_running`
- `stopping_the_node_stops_its_commands_and_leaves_a_readable_record`
- `a_killed_node_leaves_its_command_running_and_the_record_says_unknown`
- `a_hangup_stops_the_node_and_its_commands`
- `a_node_started_with_nohup_survives_a_hangup`

### `file_writes.rs`

- `native_file_writes_are_explicit_machine_scoped_and_revoked_by_sharing_replacement`
- `native_file_writes_keep_core_path_atomicity_and_version_preconditions`
- `native_file_write_permissions_cannot_be_forged_or_inherited_from_grants_or_noauth`：用新模型恢复合法调用成功和伪造调用失败；不恢复 noauth 或旧 grant 契约。

### 生命周期、共享与接入模块

- `ownership.rs::native_node_connection_is_host_owned_across_switch_and_hub_restart`。
- `sharing.rs::cli_sharing_changes_require_a_stopped_node_and_preserve_remaining_project_handles`：恢复修改共享后的远端句柄稳定性与访问权限收回。
- 配对成功、新机器接入及 CONNECT 代理测试中的远端项目发现和文件读取部分。
- 在线诊断与在线自启动、重启时的连接计数。A 当前明确让节点处于离线重试状态，验证进程锁和服务管理器动作，不宣称转发已经成功。

现有 core 原生节点传输、签名、大小限制、作用域和命令策略单元测试仍可执行。它们补充上述 D18-C 集成覆盖，不能代替完整转发链的正向验收。
