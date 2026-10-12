// 设置页「备份」卡：状态、已有备份列表、立即备份、未配置时启用（填一份默认配置进草稿）。
// 配置本身走设置页的统一保存（PUT /api/config）；口令与密钥只在服务器环境变量里。

import { api, ApiError } from './lib/api.js';
import { h } from './lib/dom.js';

const $ = (id) => document.getElementById(id);

function fmtTime(ms) {
  if (!ms) return '—';
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getMonth() + 1}月${d.getDate()}日 ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function fmtBytes(n) {
  if (n == null) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** 备份名 thought-YYYYMMDD-HHMMSS.thbk → 本地时间（文件名里是 UTC）。 */
function nameToTime(key) {
  const m = /^thought-(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})\.thbk$/.exec(key);
  if (!m) return key;
  const ms = Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]);
  return fmtTime(ms);
}

/** 新建备份配置时的默认值（与后端默认一致）。 */
export function defaultBackup() {
  return {
    enabled: true,
    daily_hour: 4,
    tz_offset_min: -new Date().getTimezoneOffset(),
    keep: 14,
    passphrase_env: 'THOUGHT_BACKUP_PASSPHRASE',
    s3: {
      endpoint: '', region: 'us-east-1', bucket: '', prefix: 'thought/', path_style: true,
      access_key_env: 'THOUGHT_S3_ACCESS_KEY', secret_key_env: 'THOUGHT_S3_SECRET_KEY',
    },
  };
}

/**
 * 按当前草稿刷新卡片：未配置时显示「启用」，否则显示字段。
 * `onEnable()` 由 settings.js 提供：把默认配置写入草稿并重绘。
 */
export function paintBackupCard(cfg, { editable, onEnable }) {
  const off = !cfg?.backup;
  $('backup-off').hidden = !off;
  $('backup-fields').hidden = off;
  $('backup-enable').disabled = !editable;
  $('backup-enable').onclick = editable ? onEnable : null;
  $('backup-run').hidden = off;
}

/** 拉状态与列表（admin）。非 admin / 未配置时安静地显示提示。 */
export async function refreshBackup() {
  const state = $('backup-state');
  const list = $('backup-list');
  const run = $('backup-run');
  list.textContent = '';
  let status;
  try {
    status = await api('/api/backup');
  } catch (e) {
    state.textContent = e instanceof ApiError && e.status === 403 ? '仅管理员可见' : `状态读取失败：${e.message}`;
    run.disabled = true;
    return;
  }
  const s = status.status || {};
  run.disabled = !s.enabled || s.running;
  if (!status.config) {
    state.textContent = '尚未配置';
    return;
  }
  if (s.running) state.textContent = '正在备份…';
  else if (s.last_error && (!s.last_ok || s.last_error.at_ms > s.last_ok.at_ms)) state.textContent = `上次失败（${fmtTime(s.last_error.at_ms)}）：${s.last_error.error}`;
  else if (s.last_ok) state.textContent = `上次成功：${fmtTime(s.last_ok.at_ms)} · ${fmtBytes(s.last_ok.bytes)}`;
  else state.textContent = s.enabled ? '还没有备份过' : '已停用';
  state.classList.toggle('err', !!s.last_error && !s.running && (!s.last_ok || s.last_error.at_ms > s.last_ok.at_ms));

  if (!s.enabled) return;
  try {
    const { backups } = await api('/api/backup/list');
    if (!backups.length) {
      list.append(h('li', { class: 'empty', text: '对象存储里还没有备份' }));
      return;
    }
    for (const b of backups) {
      list.append(h('li', {},
        h('span', { class: 'when', text: nameToTime(b.key) }),
        h('code', { class: 'name', text: b.key, title: '恢复：thought restore ' + b.key }),
        h('span', { class: 'size', text: fmtBytes(b.size) }),
      ));
    }
  } catch (e) {
    list.append(h('li', { class: 'empty err', text: `列表读取失败：${e.message}` }));
  }
}

/** 「立即备份」按钮。 */
export function bindBackupRun(toastError) {
  const run = $('backup-run');
  run.onclick = async () => {
    run.disabled = true;
    $('backup-state').textContent = '正在备份…';
    try {
      const { name } = await api('/api/backup/run', { method: 'POST' });
      $('backup-state').textContent = `已备份：${name}`;
    } catch (e) {
      toastError(`备份失败：${e.message}`);
    }
    await refreshBackup();
  };
}
