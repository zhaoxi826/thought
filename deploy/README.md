# 部署：以 `thought` 用户运行思想

思想作为常驻服务，以专用系统用户 `thought` 运行（无 sudo / docker）。
程序、前端、配置、数据都放在 `/home/thought/thought`，与开发仓库 `/data/thought` 分离。

## 安装 / 升级

```bash
# 在仓库根目录，以开发用户编译
cargo build --profile dist -p thought-core

# 以 root 安装（重复执行即升级：只替换程序与前端，不动配置和数据）
sudo deploy/install.sh

# 首次安装后：填 DeepSeek key，按需改配置
sudo -u thought $EDITOR /home/thought/thought/thought.env
sudo -u thought $EDITOR /home/thought/thought/thought.toml

sudo systemctl enable --now thought
journalctl -u thought -f
```

首次安装时 `install.sh` 会生成：

| 文件 | 内容 |
| --- | --- |
| `thought.toml` | 从 `thought.example.toml` 复制，权限 0600 |
| `thought.env` | 随机生成的 JWT 密钥、管理员初始密码（`THOUGHT_ADMIN_PASSWORD`），以及待填写的 `DEEPSEEK_API_KEY`；权限 0600 |

管理员初始密码在 `thought.env` 里：`sudo -u thought grep ADMIN /home/thought/thought/thought.env`。

## 目录

```
/home/thought/thought/
  bin/thought          程序
  web/                 前端
  thought.toml         配置（她不可读写）
  thought.env          密钥（她不可读写）
  data/thought/        SurrealKV：用户、意识流、记忆（她不可读写）
  data/mind/workspace/ 她的工作区（完全自由）
  data/mind/trash/     回收站（她删的东西都在这，可恢复）
  data/mind/backup/    她改外部文件前的备份
```

## 隔离分三层

| 层 | 机制 | 防住什么 |
| --- | --- | --- |
| 进程 | `thought` 用户 + systemd 加固 | 她碰不到其他用户的文件；`/home/zhaoxi` 等对她不可见（`ProtectHome=tmpfs`）；系统目录只读（`ProtectSystem=strict`） |
| 命令 | 每条 `run_command` 在 bubblewrap 中执行 | shell 里宿主只读、工作区可写；配置、数据库、密钥被遮住 |
| 文件工具 | `sandbox::FsPolicy` | 外部改前备份、删除进回收站；受保护路径拒绝 |

注意：因为 `ProtectSystem=strict` 和 `ProtectHome=tmpfs`，她能写的只有 `/home/thought/thought` 下的东西。如果你希望她能改仓库 `/data/thought` 里的代码，需要在 unit 里加 `ReadWritePaths=/data/thought`，并让 `thought` 用户对该目录有写权限（例如加入同一个组）。她改外部文件仍然会先备份。

### 已知的加固取舍

bubblewrap 需要在服务内创建非特权 user namespace 并挂载，以下两个 systemd 选项与之冲突，因此未启用（已实测）：

- `RestrictSUIDSGID=`：seccomp 过滤挡住 bwrap 的 mount 调用（`Can't open source /: Function not implemented`）。`NoNewPrivileges=yes` 已经让 SUID 提权失效。
- `ProtectKernelTunables=`：`/proc` 被部分覆盖后 bwrap 无法挂载新的 proc。

## 运维

| 操作 | 命令 |
| --- | --- |
| 急停她的自主活动（对话不受影响） | `POST /api/mind/pause`（admin） |
| 恢复 | `POST /api/mind/resume` |
| 看今日用量 | `GET /api/mind/budget` |
| 停服务 | `sudo systemctl stop thought` |
| 备份数据 | 停服务后复制 `/home/thought/thought/data/` |
