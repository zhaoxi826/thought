#!/bin/sh
# 把编译好的思想安装到 /home/thought/thought，并注册 systemd 服务。
# 用法（在仓库根目录）：
#   cargo build --profile dist -p thought-core
#   sudo deploy/install.sh
# 重复执行即升级：只替换程序与前端，不动配置和数据。
set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
HOME_DIR=/home/thought/thought
BIN="$REPO/target/dist/thought"

[ "$(id -u)" -eq 0 ] || { echo "需要 root：sudo $0" >&2; exit 1; }
id thought >/dev/null 2>&1 || { echo "用户 thought 不存在" >&2; exit 1; }
[ -x "$BIN" ] || { echo "找不到 $BIN，先运行：cargo build --profile dist -p thought-core" >&2; exit 1; }
command -v bwrap >/dev/null || echo "警告：未安装 bubblewrap，命令将不经沙箱执行" >&2

install -d -o thought -g thought -m 0700 "$HOME_DIR" "$HOME_DIR/bin" "$HOME_DIR/data"
install -o thought -g thought -m 0755 "$BIN" "$HOME_DIR/bin/thought"
rm -rf "$HOME_DIR/web"
cp -r "$REPO/web" "$HOME_DIR/web"
chown -R thought:thought "$HOME_DIR/web"

if [ ! -f "$HOME_DIR/thought.toml" ]; then
    install -o thought -g thought -m 0600 "$REPO/thought.example.toml" "$HOME_DIR/thought.toml"
    echo "已写入默认配置 $HOME_DIR/thought.toml，请按需修改"
fi
if [ ! -f "$HOME_DIR/thought.env" ]; then
    umask 077
    {
        echo "THOUGHT_JWT_SECRET=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
        echo "THOUGHT_ADMIN_PASSWORD=$(head -c 12 /dev/urandom | base64 | tr -d '/+=')"
        echo "DEEPSEEK_API_KEY="
    } > "$HOME_DIR/thought.env"
    chown thought:thought "$HOME_DIR/thought.env"
    echo "已生成 $HOME_DIR/thought.env（JWT 密钥、管理员初始密码），请填入 DEEPSEEK_API_KEY"
fi

install -m 0644 "$REPO/deploy/thought.service" /etc/systemd/system/thought.service
systemctl daemon-reload
echo "安装完成。启动：sudo systemctl enable --now thought"
echo "查看日志：journalctl -u thought -f"
